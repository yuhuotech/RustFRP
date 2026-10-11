// HTTP 虚拟主机的逐请求能力：路径前缀（locations）、Basic 认证、routeByHTTPUser、请求头改写、
// 正文分帧与保持连接上的逐请求路由。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, Event, ProxyCfg};
use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use rustfrp_core::wire::HttpOpts;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

struct Rig {
    http: SocketAddr,
    edge: SocketAddr,
    stop: CancellationToken,
}

async fn start() -> Rig {
    let cfg = EdgeConfig {
        bind_addr: "127.0.0.1".into(),
        bind_port: 0,
        proxy_bind_addr: "127.0.0.1".into(),
        vhost_http_port: Some(0),
        vhost_head_timeout: Duration::from_millis(600),
        ..Default::default()
    };
    let edge = Edge::bind(cfg, None, Logger::silent()).await.unwrap();
    let rig = Rig {
        http: edge.http_addr().unwrap(),
        edge: edge.local_addr().unwrap(),
        stop: CancellationToken::new(),
    };
    tokio::spawn(edge.serve(rig.stop.clone()));
    rig
}

fn run_client(rig: &Rig, proxies: Vec<ProxyCfg>) -> (mpsc::Receiver<Event>, CancellationToken) {
    let (tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: rig.edge.port(),
        proxies,
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    };
    tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
    (rx, cancel)
}

async fn events(rx: &mut mpsc::Receiver<Event>, want: usize) -> Vec<Event> {
    let mut out = Vec::new();
    while out.len() < want {
        match timeout(T, rx.recv()).await.unwrap().unwrap() {
            Event::Connected => {}
            e => out.push(e),
        }
    }
    out
}

fn web(name: &str, port: u16, domain: &str, http: HttpOpts) -> ProxyCfg {
    ProxyCfg {
        name: name.into(),
        kind: "http".into(),
        local_ip: "127.0.0.1".into(),
        local_port: port,
        custom_domains: vec![domain.into()],
        http,
        ..Default::default()
    }
}

/// 一条后端收到的请求：头 + 正文。
type Seen = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// 支持保持连接的最小 HTTP 后端：按 Content-Length / chunked 读完正文，回 `backend=<tag>`；
/// `Upgrade` 请求回 101 后原样回显。
async fn backend(tag: &'static str) -> (u16, Seen) {
    backend_with(tag, Mode::KeepAlive).await
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    KeepAlive,
    /// 回一个响应后关闭连接。
    CloseAfter,
    /// 响应前先等一会儿。
    Slow,
    /// 回非 HTTP 的内容。
    Junk,
    /// 回一个很大的响应体。
    Big,
}

async fn backend_with(tag: &'static str, mode: Mode) -> (u16, Seen) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let end = loop {
                        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break p + 4;
                        }
                        match s.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_string();
                    buf.drain(..end);
                    let lower = head.to_ascii_lowercase();
                    let mut body = Vec::new();
                    if let Some(i) = lower.find("content-length:") {
                        let n: usize = lower[i + 15..]
                            .lines()
                            .next()
                            .unwrap()
                            .trim()
                            .parse()
                            .unwrap();
                        while buf.len() < n {
                            let k = s.read(&mut tmp).await.unwrap_or(0);
                            if k == 0 {
                                return;
                            }
                            buf.extend_from_slice(&tmp[..k]);
                        }
                        body = buf.drain(..n).collect();
                    } else if lower.contains("transfer-encoding: chunked") {
                        while !buf.ends_with(b"0\r\n\r\n") {
                            let k = s.read(&mut tmp).await.unwrap_or(0);
                            if k == 0 {
                                return;
                            }
                            buf.extend_from_slice(&tmp[..k]);
                        }
                        body = std::mem::take(&mut buf);
                    }
                    let upgrade = lower.contains("upgrade: websocket");
                    log.lock().unwrap().push((head, body));
                    if upgrade {
                        let _ = s
                            .write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n")
                            .await;
                        if !buf.is_empty() {
                            let _ = s.write_all(&buf).await;
                        }
                        while let Ok(n) = s.read(&mut tmp).await {
                            if n == 0 || s.write_all(&tmp[..n]).await.is_err() {
                                return;
                            }
                        }
                        return;
                    }
                    if mode == Mode::Slow {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    if mode == Mode::Junk {
                        let _ = s.write_all(b"this is not http\r\n\r\n").await;
                        continue;
                    }
                    if mode == Mode::Big {
                        let body = vec![b'z'; 24 * 1024 * 1024];
                        let h =
                            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                        let _ = s.write_all(h.as_bytes()).await;
                        let _ = s.write_all(&body).await;
                        return;
                    }
                    let b = format!("backend={tag}");
                    let r = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{b}", b.len());
                    if s.write_all(r.as_bytes()).await.is_err() || mode == Mode::CloseAfter {
                        return;
                    }
                }
            });
        }
    });
    (port, seen)
}

/// 读取一个带 Content-Length 的响应（保持连接，不等关闭）；多余的字节留在 `buf` 里给下一次读取。
async fn read_response(s: &mut TcpStream, buf: &mut Vec<u8>) -> Option<String> {
    let mut tmp = [0u8; 1024];
    loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p + 4]).to_ascii_lowercase();
            let n: usize = head
                .split("content-length:")
                .nth(1)
                .and_then(|r| r.lines().next())
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            if buf.len() >= p + 4 + n {
                let out: Vec<u8> = buf.drain(..p + 4 + n).collect();
                return Some(String::from_utf8_lossy(&out).to_string());
            }
        }
        let n = timeout(T, s.read(&mut tmp)).await.ok()?.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

async fn request(addr: SocketAddr, raw: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut pending: Vec<u8> = Vec::new();
    s.write_all(raw.as_bytes()).await.unwrap();
    read_response(&mut s, &mut pending)
        .await
        .unwrap_or_default()
}

fn basic(user: &str, pass: &str) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let data = format!("{user}:{pass}");
    let mut out = String::new();
    for c in data.as_bytes().chunks(3) {
        let n = c.iter().fold(0u32, |a, b| (a << 8) | u32::from(*b)) << (8 * (3 - c.len()));
        for i in 0..=c.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
        out.extend(std::iter::repeat_n('=', 3 - c.len()));
    }
    format!("Basic {out}")
}

#[tokio::test]
async fn basic_auth_is_enforced_before_the_backend_is_touched() {
    let rig = start().await;
    let (port, seen) = backend("secure").await;
    let opts = HttpOpts {
        http_user: "alice".into(),
        http_password: "s3cret".into(),
        ..Default::default()
    };
    let (mut rx, cancel) = run_client(&rig, vec![web("s", port, "a.example.com", opts)]);
    assert!(matches!(
        events(&mut rx, 1).await[0],
        Event::ProxyStarted { .. }
    ));
    let get = |auth: Option<String>| {
        let extra = auth.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
        format!("GET / HTTP/1.1\r\nHost: a.example.com\r\n{extra}\r\n")
    };
    for bad in [
        None,
        Some(basic("alice", "wrong")),
        Some(basic("bob", "s3cret")),
        Some("Bearer token".to_string()),
    ] {
        let r = request(rig.http, &get(bad)).await;
        assert!(r.starts_with("HTTP/1.1 401"), "{r}");
        assert!(r.contains("WWW-Authenticate: Basic realm="), "{r}");
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "rejected requests never reach the backend"
    );
    let ok = request(rig.http, &get(Some(basic("alice", "s3cret")))).await;
    assert!(ok.ends_with("backend=secure"), "{ok}");
    assert_eq!(seen.lock().unwrap().len(), 1);
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn locations_pick_the_longest_prefix_per_request_on_one_connection() {
    let rig = start().await;
    let ((pa, _), (pb, _), (pc, _)) = (
        backend("root").await,
        backend("api").await,
        backend("api-v2").await,
    );
    let loc = |l: &[&str]| HttpOpts {
        locations: l.iter().map(ToString::to_string).collect(),
        ..Default::default()
    };
    let (mut rx, cancel) = run_client(
        &rig,
        vec![
            web("root", pa, "a.example.com", HttpOpts::default()),
            web("api", pb, "a.example.com", loc(&["/api", "/rpc"])),
            web("v2", pc, "a.example.com", loc(&["/api/v2"])),
        ],
    );
    for e in events(&mut rx, 3).await {
        assert!(matches!(e, Event::ProxyStarted { .. }), "{e:?}");
    }
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    let mut pending: Vec<u8> = Vec::new();
    for (path, want) in [
        ("/index.html", "backend=root"),
        ("/api/users?x=1", "backend=api"),
        ("/api/v2/items", "backend=api-v2"),
        ("/rpc", "backend=api"),
        ("/", "backend=root"),
        ("/api", "backend=api"),
    ] {
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: a.example.com\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let r = read_response(&mut s, &mut pending)
            .await
            .unwrap_or_default();
        assert!(r.ends_with(want), "{path}: {r}");
    }
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn overlapping_locations_and_users_are_rejected_at_registration() {
    let rig = start().await;
    let (port, _) = backend("x").await;
    let user = |u: &str| HttpOpts {
        route_by_http_user: u.into(),
        ..Default::default()
    };
    let (mut rx, cancel) = run_client(
        &rig,
        vec![
            web("a", port, "d.example.com", HttpOpts::default()),
            web("b", port, "d.example.com", HttpOpts::default()),
            web("c", port, "d.example.com", user("carol")),
            web(
                "bad",
                port,
                "e.example.com",
                HttpOpts {
                    locations: vec!["nope".into()],
                    ..Default::default()
                },
            ),
        ],
    );
    let ev = events(&mut rx, 4).await;
    assert!(matches!(ev[0], Event::ProxyStarted { .. }), "{:?}", ev[0]);
    assert!(
        matches!(&ev[1], Event::ProxyFailed { error, .. } if error.contains("already used")),
        "{:?}",
        ev[1]
    );
    assert!(matches!(ev[2], Event::ProxyStarted { .. }), "{:?}", ev[2]);
    assert!(
        matches!(&ev[3], Event::ProxyFailed { error, .. } if error.contains("invalid location")),
        "{:?}",
        ev[3]
    );
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn route_by_http_user_selects_the_backend_with_a_default_fallback() {
    let rig = start().await;
    let ((pd, _), (pa, _), (pb, _)) = (
        backend("default").await,
        backend("alice").await,
        backend("bob").await,
    );
    let user = |u: &str| HttpOpts {
        route_by_http_user: u.into(),
        ..Default::default()
    };
    let (mut rx, cancel) = run_client(
        &rig,
        vec![
            web("d", pd, "a.example.com", HttpOpts::default()),
            web("a", pa, "a.example.com", user("alice")),
            web("b", pb, "a.example.com", user("bob")),
        ],
    );
    events(&mut rx, 3).await;
    let get = |auth: Option<String>| {
        let extra = auth.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
        format!("GET / HTTP/1.1\r\nHost: a.example.com\r\n{extra}\r\n")
    };
    assert!(request(rig.http, &get(Some(basic("alice", "x"))))
        .await
        .ends_with("backend=alice"));
    assert!(request(rig.http, &get(Some(basic("bob", "y"))))
        .await
        .ends_with("backend=bob"));
    assert!(request(rig.http, &get(Some(basic("zed", "y"))))
        .await
        .ends_with("backend=default"));
    assert!(request(rig.http, &get(None))
        .await
        .ends_with("backend=default"));
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn request_headers_are_set_and_host_is_rewritten_without_touching_the_body() {
    let rig = start().await;
    let (port, seen) = backend("h").await;
    let mut p = web(
        "h",
        port,
        "a.example.com",
        HttpOpts {
            request_headers: [
                ("X-Env".to_string(), "prod".to_string()),
                ("Accept".to_string(), "text/plain".to_string()),
            ]
            .into(),
            ..Default::default()
        },
    );
    p.host_header_rewrite = "inner.lan".into();
    let (mut rx, cancel) = run_client(&rig, vec![p]);
    events(&mut rx, 1).await;
    let body = "hello=world";
    let r = request(
        rig.http,
        &format!(
            "POST /submit HTTP/1.1\r\nHost: a.example.com\r\nx-env: dev\r\nAccept: */*\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
    .await;
    assert!(r.ends_with("backend=h"), "{r}");
    let (head, got) = seen.lock().unwrap()[0].clone();
    assert!(
        head.starts_with("POST /submit HTTP/1.1\r\nHost: inner.lan\r\n"),
        "{head}"
    );
    assert!(
        head.contains("X-Env: prod\r\n") && !head.contains("x-env: dev"),
        "{head}"
    );
    assert!(
        head.contains("Accept: text/plain\r\n") && !head.contains("*/*"),
        "{head}"
    );
    assert_eq!(got, body.as_bytes());
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn bodies_are_framed_so_pipelined_next_requests_route_independently() {
    let rig = start().await;
    let ((pa, sa), (pb, sb)) = (backend("a").await, backend("b").await);
    let api = HttpOpts {
        locations: vec!["/api".into()],
        ..Default::default()
    };
    let (mut rx, cancel) = run_client(
        &rig,
        vec![
            web("a", pa, "a.example.com", HttpOpts::default()),
            web("b", pb, "a.example.com", api),
        ],
    );
    events(&mut rx, 2).await;
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    let mut pending: Vec<u8> = Vec::new();
    // 固定长度正文之后紧跟另一条请求（同一次写入）；分块正文同理。
    s.write_all(
        b"POST /up HTTP/1.1\r\nHost: a.example.com\r\nContent-Length: 4\r\n\r\nDATAGET /api/x HTTP/1.1\r\nHost: a.example.com\r\n\r\n",
    )
    .await
    .unwrap();
    assert!(read_response(&mut s, &mut pending)
        .await
        .unwrap()
        .ends_with("backend=a"));
    assert!(read_response(&mut s, &mut pending)
        .await
        .unwrap()
        .ends_with("backend=b"));
    s.write_all(
        b"POST /up HTTP/1.1\r\nHost: a.example.com\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\nGET /api/y HTTP/1.1\r\nHost: a.example.com\r\n\r\n",
    )
    .await
    .unwrap();
    assert!(read_response(&mut s, &mut pending)
        .await
        .unwrap()
        .ends_with("backend=a"));
    assert!(read_response(&mut s, &mut pending)
        .await
        .unwrap()
        .ends_with("backend=b"));
    let a = sa.lock().unwrap().clone();
    assert_eq!(a[0].1, b"DATA");
    assert!(String::from_utf8_lossy(&a[1].1).starts_with("3\r\nabc\r\n2\r\nde\r\n0"));
    assert_eq!(sb.lock().unwrap().len(), 2);
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn keep_alive_requests_are_authorized_individually() {
    let rig = start().await;
    let (pa, _) = backend("open").await;
    let (pb, sb) = backend("private").await;
    let private = HttpOpts {
        locations: vec!["/private".into()],
        http_user: "u".into(),
        http_password: "p".into(),
        ..Default::default()
    };
    let (mut rx, cancel) = run_client(
        &rig,
        vec![
            web("open", pa, "a.example.com", HttpOpts::default()),
            web("private", pb, "a.example.com", private),
        ],
    );
    events(&mut rx, 2).await;
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    let mut pending: Vec<u8> = Vec::new();
    s.write_all(b"GET / HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
        .await
        .unwrap();
    assert!(read_response(&mut s, &mut pending)
        .await
        .unwrap()
        .ends_with("backend=open"));
    // 同一连接上的后续请求命中受保护路径且没有凭据：连接被关闭，后端收不到任何东西。
    s.write_all(b"GET /private HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
        .await
        .unwrap();
    assert!(read_response(&mut s, &mut pending).await.is_none());
    assert!(sb.lock().unwrap().is_empty());
    // 带凭据则放行。
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    let mut pending: Vec<u8> = Vec::new();
    s.write_all(
        format!(
            "GET /private HTTP/1.1\r\nHost: a.example.com\r\nAuthorization: {}\r\n\r\n",
            basic("u", "p")
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    assert!(read_response(&mut s, &mut pending)
        .await
        .unwrap()
        .ends_with("backend=private"));
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn ambiguous_or_oversized_requests_get_400_and_slow_heads_time_out() {
    let rig = start().await;
    let (port, seen) = backend("x").await;
    let (mut rx, cancel) = run_client(
        &rig,
        vec![web("x", port, "a.example.com", HttpOpts::default())],
    );
    events(&mut rx, 1).await;
    for raw in [
        "POST / HTTP/1.1\r\nHost: a.example.com\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".to_string(),
        "POST / HTTP/1.1\r\nHost: a.example.com\r\nContent-Length: 4\r\nContent-Length: 5\r\n\r\n".to_string(),
        "GET / HTTP/1.1\r\nHost: a.example.com\r\nHost: b.example.com\r\n\r\n".to_string(),
        format!("GET / HTTP/1.1\r\nHost: a.example.com\r\nX: {}\r\n\r\n", "a".repeat(20_000)),
    ] {
        let r = request(rig.http, &raw).await;
        assert!(r.starts_with("HTTP/1.1 400"), "{raw:.80}: {r}");
    }
    assert!(seen.lock().unwrap().is_empty());
    // 分块正文语法错误：连接被关闭，后端只收到已转发的部分，不会把后续字节当作新请求。
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    s.write_all(
        b"POST / HTTP/1.1\r\nHost: a.example.com\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\nGET /smuggled HTTP/1.1\r\nHost: a.example.com\r\n\r\n",
    )
    .await
    .unwrap();
    let mut rest = Vec::new();
    timeout(T, s.read_to_end(&mut rest)).await.unwrap().unwrap();
    assert!(!String::from_utf8_lossy(&rest).contains("backend="), "{:?}", String::from_utf8_lossy(&rest));
    assert!(seen.lock().unwrap().iter().all(|(h, _)| !h.contains("/smuggled")));
    // 头只发一半就停住：超时后连接被关闭。
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nHost: a.exa").await.unwrap();
    let mut out = Vec::new();
    let n = timeout(T, s.read_to_end(&mut out)).await;
    assert!(n.is_ok(), "slow head must be dropped");
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn upgrade_switches_to_raw_passthrough_and_counts_bytes() {
    let rig = start().await;
    let (port, _) = backend("ws").await;
    let (mut rx, cancel) = run_client(
        &rig,
        vec![web("ws", port, "a.example.com", HttpOpts::default())],
    );
    events(&mut rx, 1).await;
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    s.write_all(
        b"GET /socket HTTP/1.1\r\nHost: a.example.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\nearly",
    )
    .await
    .unwrap();
    let mut got = Vec::new();
    let mut tmp = [0u8; 256];
    while !String::from_utf8_lossy(&got).contains("early") {
        let n = timeout(T, s.read(&mut tmp)).await.unwrap().unwrap();
        assert!(n > 0);
        got.extend_from_slice(&tmp[..n]);
    }
    assert!(String::from_utf8_lossy(&got).starts_with("HTTP/1.1 101"));
    s.write_all(b"ping").await.unwrap();
    let n = timeout(T, s.read(&mut tmp)).await.unwrap().unwrap();
    assert_eq!(&tmp[..n], b"ping");
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn invalid_http_options_are_refused_at_registration() {
    let rig = start().await;
    let (port, _) = backend("x").await;
    let bad_header = HttpOpts {
        request_headers: [("Bad Name".to_string(), "v".to_string())].into(),
        ..Default::default()
    };
    let bad_loc = |l: &str| HttpOpts {
        locations: vec![l.into()],
        ..Default::default()
    };
    let dup = HttpOpts {
        locations: vec!["/a".into(), "/a".into()],
        ..Default::default()
    };
    let (mut rx, cancel) = run_client(
        &rig,
        vec![
            web("h", port, "h.example.com", bad_header),
            web("l1", port, "l1.example.com", bad_loc("/a b")),
            web("l2", port, "l2.example.com", bad_loc("/a\tb")),
            web("l3", port, "l3.example.com", bad_loc("a")),
            web("d", port, "d.example.com", dup),
        ],
    );
    let ev = events(&mut rx, 5).await;
    for (i, want) in [
        (0, "invalid request header"),
        (1, "invalid location"),
        (2, "invalid location"),
        (3, "invalid location"),
    ] {
        assert!(
            matches!(&ev[i], Event::ProxyFailed { error, .. } if error.contains(want)),
            "{i}: {:?}",
            ev[i]
        );
    }
    assert!(matches!(ev[4], Event::ProxyStarted { .. }), "{:?}", ev[4]);
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn wildcard_and_exact_domains_resolve_in_frp_order() {
    let rig = start().await;
    let ((pe, _), (pw, _), (pb, _), (po, _)) = (
        backend("exact").await,
        backend("wild").await,
        backend("broad").await,
        backend("other").await,
    );
    let (mut rx, cancel) = run_client(
        &rig,
        vec![
            web("e", pe, "x.a.example.com", HttpOpts::default()),
            web("w", pw, "*.a.example.com", HttpOpts::default()),
            web("b", pb, "*.example.com", HttpOpts::default()),
            web("o", po, "*.other.org", HttpOpts::default()),
        ],
    );
    events(&mut rx, 4).await;
    let get = |host: &str| format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n");
    assert!(request(rig.http, &get("x.a.example.com"))
        .await
        .ends_with("backend=exact"));
    assert!(request(rig.http, &get("y.a.example.com"))
        .await
        .ends_with("backend=wild"));
    assert!(request(rig.http, &get("z.example.com"))
        .await
        .ends_with("backend=broad"));
    assert!(request(rig.http, &get("q.other.org"))
        .await
        .ends_with("backend=other"));
    assert!(request(rig.http, &get("nope.net"))
        .await
        .starts_with("HTTP/1.1 404"));
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn password_only_auth_and_header_only_rewrites_work() {
    let rig = start().await;
    let (port, seen) = backend("p").await;
    let opts = HttpOpts {
        http_password: "pw".into(),
        request_headers: [("X-A".to_string(), "1".to_string())].into(),
        ..Default::default()
    };
    let (mut rx, cancel) = run_client(&rig, vec![web("p", port, "a.example.com", opts)]);
    events(&mut rx, 1).await;
    let req = |auth: &str| {
        format!("GET / HTTP/1.1\r\nHost: a.example.com\r\nAuthorization: {auth}\r\n\r\n")
    };
    for bad in [
        basic("anyone", "bad"),
        basic("", ""),
        "Basic %%%".to_string(),
    ] {
        assert!(
            request(rig.http, &req(&bad))
                .await
                .starts_with("HTTP/1.1 401"),
            "{bad}"
        );
    }
    assert!(seen.lock().unwrap().is_empty());
    // 只配了密码：用户名为空即可通过；只改请求头（没有 Host 改写）。
    assert!(request(rig.http, &req(&basic("", "pw")))
        .await
        .ends_with("backend=p"));
    let head = seen.lock().unwrap()[0].0.clone();
    assert!(
        head.starts_with("GET / HTTP/1.1\r\nHost: a.example.com\r\n"),
        "{head}"
    );
    assert!(head.contains("X-A: 1\r\n"), "{head}");
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn cross_route_pipelining_waits_for_slow_responses_and_stops_on_unknown_framing() {
    let rig = start().await;
    let ((ps, _), (pf, _), (pj, _)) = (
        backend_with("slow", Mode::Slow).await,
        backend("fast").await,
        backend_with("junk", Mode::Junk).await,
    );
    let at = |l: &str| HttpOpts {
        locations: vec![l.into()],
        ..Default::default()
    };
    let (mut rx, cancel) = run_client(
        &rig,
        vec![
            web("slow", ps, "a.example.com", at("/slow")),
            web("fast", pf, "a.example.com", at("/fast")),
            web("junk", pj, "a.example.com", at("/junk")),
        ],
    );
    events(&mut rx, 3).await;
    // 慢响应在前、快响应在后，同一次写入：响应仍按请求顺序返回。
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    let mut pending: Vec<u8> = Vec::new();
    s.write_all(b"GET /slow HTTP/1.1\r\nHost: a.example.com\r\n\r\nGET /fast HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
        .await
        .unwrap();
    assert!(read_response(&mut s, &mut pending)
        .await
        .unwrap()
        .ends_with("backend=slow"));
    assert!(read_response(&mut s, &mut pending)
        .await
        .unwrap()
        .ends_with("backend=fast"));
    // 上一个后端的响应无法判定边界（非 HTTP）：切换到别的后端时关闭连接，而不是冒险乱序。
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    s.write_all(b"GET /junk HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
        .await
        .unwrap();
    let mut got = [0u8; 64];
    let n = timeout(T, s.read(&mut got)).await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&got[..n]).contains("not http"));
    s.write_all(b"GET /fast HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
        .await
        .unwrap();
    let n = timeout(T, s.read(&mut got)).await.unwrap().unwrap_or(0);
    assert_eq!(n, 0, "connection closed");
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn a_closed_backend_does_not_break_the_connection_when_another_route_is_current() {
    let rig = start().await;
    let ((pa, _), (pb, _)) = (
        backend_with("a", Mode::CloseAfter).await,
        backend("b").await,
    );
    let at = |l: &str| HttpOpts {
        locations: vec![l.into()],
        ..Default::default()
    };
    let (mut rx, cancel) = run_client(
        &rig,
        vec![
            web("a", pa, "a.example.com", at("/a")),
            web("b", pb, "a.example.com", at("/b")),
        ],
    );
    events(&mut rx, 2).await;
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    let mut pending: Vec<u8> = Vec::new();
    for (path, want) in [("/a", "backend=a"), ("/b", "backend=b")] {
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: a.example.com\r\n\r\n").as_bytes())
            .await
            .unwrap();
        assert!(read_response(&mut s, &mut pending)
            .await
            .unwrap()
            .ends_with(want));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // A 的后端早已关闭，再回到 A 会重新建流。
    s.write_all(b"GET /a HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
        .await
        .unwrap();
    assert!(read_response(&mut s, &mut pending)
        .await
        .unwrap()
        .ends_with("backend=a"));
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn too_many_routes_on_one_connection_and_broken_clients_are_dropped_quietly() {
    let rig = start().await;
    let (port, _) = backend("x").await;
    let proxies: Vec<ProxyCfg> = (0..10)
        .map(|i| {
            web(
                &format!("p{i}"),
                port,
                "a.example.com",
                HttpOpts {
                    locations: vec![format!("/p{i}")],
                    ..Default::default()
                },
            )
        })
        .collect();
    let (mut rx, cancel) = run_client(&rig, proxies);
    events(&mut rx, 10).await;
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    let mut pending: Vec<u8> = Vec::new();
    for i in 0..8 {
        s.write_all(format!("GET /p{i} HTTP/1.1\r\nHost: a.example.com\r\n\r\n").as_bytes())
            .await
            .unwrap();
        assert!(
            read_response(&mut s, &mut pending).await.is_some(),
            "route {i}"
        );
    }
    s.write_all(b"GET /p8 HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
        .await
        .unwrap();
    assert!(
        read_response(&mut s, &mut pending).await.is_none(),
        "9th distinct route is refused"
    );

    // 第二个请求的头是垃圾：不回包，直接关闭。
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    let mut pending: Vec<u8> = Vec::new();
    s.write_all(b"GET /p0 HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
        .await
        .unwrap();
    assert!(read_response(&mut s, &mut pending).await.is_some());
    s.write_all(b"BROKEN\r\n\r\n").await.unwrap();
    assert!(read_response(&mut s, &mut pending).await.is_none());

    // 请求头发到一半就断开 / 请求体发到一半就断开：服务端安静地结束。
    for partial in [
        &b"GET /p0 HTTP/1.1\r\nHost: a.exam"[..],
        b"POST /p0 HTTP/1.1\r\nHost: a.example.com\r\nContent-Length: 10\r\n\r\nabc",
        b"POST /p0 HTTP/1.1\r\nHost: a.example.com\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nab",
    ] {
        let mut s = TcpStream::connect(rig.http).await.unwrap();
        s.write_all(partial).await.unwrap();
        s.shutdown().await.unwrap();
        let mut out = Vec::new();
        timeout(T, s.read_to_end(&mut out)).await.unwrap().unwrap();
        assert!(
            out.is_empty() || out.starts_with(b"HTTP/1.1"),
            "{:?}",
            String::from_utf8_lossy(&out)
        );
    }
    // 正文分多次到达（头先到，正文后到）。
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    let mut pending: Vec<u8> = Vec::new();
    s.write_all(b"POST /p1 HTTP/1.1\r\nHost: a.example.com\r\nContent-Length: 6\r\n\r\nabc")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.write_all(b"def").await.unwrap();
    assert!(read_response(&mut s, &mut pending)
        .await
        .unwrap()
        .ends_with("backend=x"));
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn visitors_that_leave_mid_response_release_the_connection() {
    let rig = start().await;
    let (port, _) = backend_with("big", Mode::Big).await;
    let (mut rx, cancel) = run_client(
        &rig,
        vec![web("big", port, "a.example.com", HttpOpts::default())],
    );
    events(&mut rx, 1).await;
    for _ in 0..3 {
        let mut s = TcpStream::connect(rig.http).await.unwrap();
        s.write_all(b"GET / HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
            .await
            .unwrap();
        let mut tmp = [0u8; 1024];
        let _ = timeout(T, s.read(&mut tmp)).await.unwrap().unwrap();
        drop(s);
    }
    // 之后服务照常。
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    s.write_all(b"HEAD / HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
        .await
        .unwrap();
    let mut tmp = [0u8; 64];
    assert!(timeout(T, s.read(&mut tmp)).await.unwrap().unwrap() > 0);
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn upgrade_without_early_bytes_and_https_edge_cases() {
    let rig = start().await;
    let (port, _) = backend("ws").await;
    let (mut rx, cancel) = run_client(
        &rig,
        vec![web("ws", port, "a.example.com", HttpOpts::default())],
    );
    events(&mut rx, 1).await;
    let mut s = TcpStream::connect(rig.http).await.unwrap();
    s.write_all(b"GET /s HTTP/1.1\r\nHost: a.example.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n").await.unwrap();
    let mut tmp = [0u8; 256];
    let n = timeout(T, s.read(&mut tmp)).await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&tmp[..n]).starts_with("HTTP/1.1 101"));
    s.write_all(b"one").await.unwrap();
    let n = timeout(T, s.read(&mut tmp)).await.unwrap().unwrap();
    assert_eq!(&tmp[..n], b"one");
    // 访问者半关闭：Edge 收尾并关闭连接。
    s.shutdown().await.unwrap();
    let mut rest = Vec::new();
    timeout(T, s.read_to_end(&mut rest)).await.unwrap().unwrap();
    assert!(rest.is_empty());
    cancel.cancel();
    rig.stop.cancel();
}
