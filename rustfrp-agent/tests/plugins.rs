//! 本地插件：socks5 / http_proxy / static_file / unix_domain_socket / TLS 终结与发起。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, Event, ProxyCfg};
use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use rustfrp_agent::plugin::PluginCfg;
use rustfrp_core::tls;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

struct Rig {
    stop: CancellationToken,
    cancel: CancellationToken,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.cancel();
        self.cancel.cancel();
    }
}

/// 启动 Edge 与带一个插件代理的客户端，返回公网端口。
async fn start(plugin: PluginCfg) -> (Rig, u16) {
    start_with(plugin, 0).await.unwrap()
}

/// 同上，可设客户端侧限速；插件启动失败时返回错误原因。
async fn start_with(plugin: PluginCfg, bandwidth_bps: u64) -> Result<(Rig, u16), String> {
    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            ..Default::default()
        },
        None,
        Logger::silent(),
    )
    .await
    .unwrap();
    let edge_port = edge.local_addr().unwrap().port();
    let stop = CancellationToken::new();
    tokio::spawn(edge.serve(stop.clone()));
    let (tx, mut rx) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: edge_port,
        proxies: vec![ProxyCfg {
            name: "p".into(),
            kind: "tcp".into(),
            plugin: Some(plugin),
            bandwidth_bps,
            ..Default::default()
        }],
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    };
    tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
    let port = loop {
        match timeout(T, rx.recv()).await.unwrap().unwrap() {
            Event::ProxyStarted { remote_port, .. } => break remote_port,
            Event::ProxyFailed { error, .. } => return Err(error),
            _ => {}
        }
    };
    Ok((Rig { stop, cancel }, port))
}

fn cfg(kind: &str) -> PluginCfg {
    PluginCfg {
        kind: kind.into(),
        ..Default::default()
    }
}

async fn echo() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 4096];
                while let Ok(n) = s.read(&mut b).await {
                    if n == 0 || s.write_all(&b[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// 回一个带请求头回显的 HTTP 响应。
async fn http_backend() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut b = vec![0u8; 8192];
                let mut n = 0;
                loop {
                    let k = s.read(&mut b[n..]).await.unwrap_or(0);
                    if k == 0 {
                        return;
                    }
                    n += k;
                    if b[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let echoed = String::from_utf8_lossy(&b[..n]).to_string();
                let r = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{echoed}",
                    echoed.len()
                );
                let _ = s.write_all(r.as_bytes()).await;
            });
        }
    });
    port
}

async fn read_to_end<S: AsyncReadExt + Unpin>(s: &mut S) -> String {
    let mut out = Vec::new();
    let _ = timeout(Duration::from_secs(3), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

async fn connect(port: u16) -> TcpStream {
    timeout(T, TcpStream::connect(("127.0.0.1", port)))
        .await
        .unwrap()
        .unwrap()
}

/// SOCKS5 问候（可带用户名 / 密码）+ CONNECT 到 127.0.0.1:`target`；返回应答的 REP 字节。
async fn socks5_connect(s: &mut TcpStream, auth: Option<(&str, &str)>, target: u16) -> Option<u8> {
    s.write_all(&[5, 1, if auth.is_some() { 2 } else { 0 }])
        .await
        .unwrap();
    let mut m = [0u8; 2];
    s.read_exact(&mut m).await.unwrap();
    if m[1] == 0xff {
        return None;
    }
    if let Some((u, p)) = auth {
        let mut a = vec![1, u.len() as u8];
        a.extend(u.as_bytes());
        a.push(p.len() as u8);
        a.extend(p.as_bytes());
        s.write_all(&a).await.unwrap();
        let mut r = [0u8; 2];
        s.read_exact(&mut r).await.unwrap();
        if r[1] != 0 {
            return None;
        }
    }
    let mut req = vec![5, 1, 0, 1, 127, 0, 0, 1];
    req.extend(target.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await.unwrap();
    Some(rep[1])
}

#[tokio::test]
async fn socks5_connects_through_the_tunnel_with_and_without_auth() {
    let target = echo().await;
    let (_rig, port) = start(cfg("socks5")).await;
    let mut s = connect(port).await;
    assert_eq!(socks5_connect(&mut s, None, target).await, Some(0));
    s.write_all(b"hello").await.unwrap();
    let mut got = [0u8; 5];
    timeout(T, s.read_exact(&mut got)).await.unwrap().unwrap();
    assert_eq!(&got, b"hello");
    // 正常半关闭：转发干净结束。
    s.shutdown().await.unwrap();
    assert!(read_to_end(&mut s).await.is_empty());
    // 域名与 IPv6 地址类型、拒绝的命令。
    let mut s = connect(port).await;
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut m = [0u8; 2];
    s.read_exact(&mut m).await.unwrap();
    let mut req = vec![5, 1, 0, 3, 9];
    req.extend(b"localhost");
    req.extend(target.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], 0);
    let mut s = connect(port).await;
    s.write_all(&[5, 1, 0]).await.unwrap();
    s.read_exact(&mut m).await.unwrap();
    s.write_all(&[5, 2, 0, 1, 127, 0, 0, 1, 0, 80])
        .await
        .unwrap();
    s.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], 7, "BIND is not supported");
    let mut s = connect(port).await;
    s.write_all(&[5, 1, 0]).await.unwrap();
    s.read_exact(&mut m).await.unwrap();
    s.write_all(&[5, 1, 0, 9, 0]).await.unwrap();
    s.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], 8, "unknown address type");
    // 目标不可达。
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = l.local_addr().unwrap().port();
    drop(l);
    let mut s = connect(port).await;
    assert_eq!(socks5_connect(&mut s, None, dead).await, Some(5));
    // 不是 socks5 的客户端：断开。
    let mut s = connect(port).await;
    s.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
    assert!(read_to_end(&mut s).await.is_empty());
}

#[tokio::test]
async fn socks5_authentication_is_enforced() {
    let target = echo().await;
    let mut c = cfg("socks5");
    c.username = "u".into();
    c.password = "p".into();
    let (_rig, port) = start(c).await;
    let mut s = connect(port).await;
    assert_eq!(
        socks5_connect(&mut s, Some(("u", "p")), target).await,
        Some(0)
    );
    // 错误密码；不支持用户名密码方法的客户端。
    let mut s = connect(port).await;
    assert_eq!(
        socks5_connect(&mut s, Some(("u", "bad")), target).await,
        None
    );
    let mut s = connect(port).await;
    assert_eq!(socks5_connect(&mut s, None, target).await, None);
}

#[tokio::test]
async fn http_proxy_supports_connect_plain_requests_and_basic_auth() {
    let echo_port = echo().await;
    let web = http_backend().await;
    let mut c = cfg("http_proxy");
    c.http_user = "alice".into();
    c.http_password = "s3cret".into();
    let (_rig, port) = start(c).await;
    let auth = "Proxy-Authorization: Basic YWxpY2U6czNjcmV0\r\n";
    // CONNECT 隧道。
    let mut s = connect(port).await;
    s.write_all(
        format!("CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: x\r\n{auth}\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut head = vec![0u8; 39];
    s.read_exact(&mut head).await.unwrap();
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200 Connection Established"));
    s.write_all(b"tunnel").await.unwrap();
    let mut got = [0u8; 6];
    timeout(T, s.read_exact(&mut got)).await.unwrap().unwrap();
    assert_eq!(&got, b"tunnel");
    s.shutdown().await.unwrap();
    assert!(read_to_end(&mut s).await.is_empty());
    // 绝对形式的明文请求被改写成源站形式，代理头被去掉。
    let mut s = connect(port).await;
    s.write_all(
        format!("GET http://127.0.0.1:{web}/a/b?q=1 HTTP/1.1\r\nHost: 127.0.0.1:{web}\r\nProxy-Connection: keep-alive\r\nConnection: keep-alive\r\n{auth}\r\n").as_bytes(),
    )
    .await
    .unwrap();
    s.shutdown().await.unwrap();
    let r = read_to_end(&mut s).await;
    assert!(r.starts_with("HTTP/1.1 200 OK"), "{r}");
    assert!(r.contains("GET /a/b?q=1 HTTP/1.1\r\n"), "{r}");
    assert!(
        !r.contains("Proxy-Authorization") && !r.contains("Proxy-Connection"),
        "{r}"
    );
    assert!(r.contains("Connection: close"), "{r}");
    // 缺少 / 错误的凭据：407。
    for bad in ["", "Proxy-Authorization: Basic YWxpY2U6bm9wZQ==\r\n"] {
        let mut s = connect(port).await;
        s.write_all(format!("CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\n{bad}\r\n").as_bytes())
            .await
            .unwrap();
        assert!(read_to_end(&mut s).await.starts_with("HTTP/1.1 407"));
    }
    // 目标不可达：502；非代理形式的请求：400；乱码：400。
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = l.local_addr().unwrap().port();
    drop(l);
    for (req, want) in [
        (
            format!("CONNECT 127.0.0.1:{dead} HTTP/1.1\r\n{auth}\r\n"),
            "502",
        ),
        (format!("GET /relative HTTP/1.1\r\n{auth}\r\n"), "400"),
        ("\x01\x02 garbage\r\n\r\n".to_string(), "400"),
    ] {
        let mut s = connect(port).await;
        s.write_all(req.as_bytes()).await.unwrap();
        let r = read_to_end(&mut s).await;
        assert!(
            r.contains(want) || (want == "400" && r.is_empty()) || r.contains("407"),
            "{req:?} -> {r}"
        );
    }
}

fn site() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rfrp-static-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("docs/sub")).unwrap();
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    std::fs::write(dir.join("docs/index.html"), "<h1>home</h1>").unwrap();
    std::fs::write(dir.join("docs/a b.txt"), "spaced").unwrap();
    std::fs::write(dir.join("docs/sub/data.json"), "{\"k\":1}").unwrap();
    std::fs::write(dir.join("empty/<x>&.bin"), vec![1u8; 10]).unwrap();
    std::fs::write(dir.join("big.bin"), vec![7u8; 300_000]).unwrap();
    std::fs::write(
        dir.parent().unwrap().join(format!(
            "{}-secret",
            dir.file_name().unwrap().to_string_lossy()
        )),
        "SECRET",
    )
    .unwrap();
    dir
}

async fn get(port: u16, method: &str, path: &str, extra: &str) -> String {
    let mut s = connect(port).await;
    s.write_all(format!("{method} {path} HTTP/1.1\r\nHost: x\r\n{extra}\r\n").as_bytes())
        .await
        .unwrap();
    read_to_end(&mut s).await
}

#[tokio::test]
async fn static_files_are_served_safely() {
    let root = site();
    let mut c = cfg("static_file");
    c.local_path = root.join("").to_string_lossy().into_owned();
    let (_rig, port) = start(c).await;
    let r = get(port, "GET", "/docs/", "").await;
    assert!(
        r.starts_with("HTTP/1.1 200 OK") && r.ends_with("<h1>home</h1>"),
        "{r}"
    );
    assert!(r.contains("Content-Type: text/html"), "{r}");
    let r = get(port, "GET", "/docs/sub/data.json", "").await;
    assert!(
        r.contains("application/json") && r.ends_with("{\"k\":1}"),
        "{r}"
    );
    let r = get(port, "GET", "/docs/a%20b.txt?x=1", "").await;
    assert!(r.contains("text/plain") && r.ends_with("spaced"), "{r}");
    // HEAD 只有头；目录无斜杠重定向；无 index 的目录给列表（转义）。
    let r = get(port, "HEAD", "/docs/a%20b.txt", "").await;
    assert!(
        r.contains("Content-Length: 6") && !r.ends_with("spaced"),
        "{r}"
    );
    assert!(get(port, "GET", "/docs", "")
        .await
        .contains("301 Moved Permanently"));
    let r = get(port, "GET", "/empty/", "").await;
    assert!(
        r.contains("Index of /empty/") && r.contains("&lt;x&gt;&amp;.bin"),
        "{r}"
    );
    assert!(r.contains("%3Cx%3E%26.bin"), "{r}");
    // 大文件完整传输。
    let mut s = connect(port).await;
    s.write_all(b"GET /big.bin HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut all = Vec::new();
    timeout(T, s.read_to_end(&mut all)).await.unwrap().unwrap();
    let body = &all[all.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4..];
    assert_eq!(body.len(), 300_000);
    // 路径穿越、编码穿越、符号链接逃逸、不存在、方法不允许。
    let secret = format!("{}-secret", root.file_name().unwrap().to_string_lossy());
    for evil in [
        format!("/../{secret}"),
        format!("/%2e%2e/{secret}"),
        format!("/docs/../../{secret}"),
        "/docs/%00".to_string(),
        "/docs/%zz".to_string(),
        "/docs/a\\b".to_string(),
        "/nope".to_string(),
    ] {
        let r = get(port, "GET", &evil, "").await;
        assert!(
            r.starts_with("HTTP/1.1 404") || r.starts_with("HTTP/1.1 400"),
            "{evil}: {r}"
        );
        assert!(!r.contains("SECRET"), "{evil}");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.parent().unwrap().join(&secret), root.join("link"))
            .unwrap();
        let r = get(port, "GET", "/link", "").await;
        assert!(
            r.starts_with("HTTP/1.1 404") && !r.contains("SECRET"),
            "{r}"
        );
    }
    assert!(get(port, "POST", "/docs/", "Content-Length: 0\r\n")
        .await
        .starts_with("HTTP/1.1 405"));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(root.parent().unwrap().join(secret));
}

#[tokio::test]
async fn static_files_support_prefix_stripping_and_basic_auth() {
    let root = site();
    let mut c = cfg("static_file");
    c.local_path = root.join("docs").to_string_lossy().into_owned();
    c.strip_prefix = "static".into();
    c.http_user = "u".into();
    c.http_password = "p".into();
    let (_rig, port) = start(c).await;
    let ok = "Authorization: Basic dTpw\r\n";
    let r = get(port, "GET", "/static/a%20b.txt", ok).await;
    assert!(r.ends_with("spaced"), "{r}");
    let r = get(port, "GET", "/static/", ok).await;
    assert!(r.ends_with("<h1>home</h1>"), "{r}");
    assert!(get(port, "GET", "/a%20b.txt", ok)
        .await
        .starts_with("HTTP/1.1 404"));
    assert!(get(port, "GET", "/staticx/a", ok)
        .await
        .starts_with("HTTP/1.1 404"));
    let r = get(port, "GET", "/static/a%20b.txt", "").await;
    assert!(
        r.starts_with("HTTP/1.1 401") && r.contains("WWW-Authenticate"),
        "{r}"
    );
    let r = get(
        port,
        "GET",
        "/static/a%20b.txt",
        "Authorization: Basic dTpiYWQ=\r\n",
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 401"), "{r}");
    // 根目录不存在：404。
    let mut missing = cfg("static_file");
    missing.local_path = root.join("nope").to_string_lossy().into_owned();
    let (_rig2, port2) = start(missing).await;
    assert!(get(port2, "GET", "/", "").await.starts_with("HTTP/1.1 404"));
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn unix_domain_socket_plugin_forwards_to_the_socket() {
    let path = std::env::temp_dir().join(format!("rfrp-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let l = tokio::net::UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 64];
                while let Ok(n) = s.read(&mut b).await {
                    if n == 0 || s.write_all(&b[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let mut c = cfg("unix_domain_socket");
    c.unix_path = path.to_string_lossy().into_owned();
    let (_rig, port) = start(c).await;
    let mut s = connect(port).await;
    s.write_all(b"unix").await.unwrap();
    let mut got = [0u8; 4];
    timeout(T, s.read_exact(&mut got)).await.unwrap().unwrap();
    assert_eq!(&got, b"unix");
    s.shutdown().await.unwrap();
    assert!(read_to_end(&mut s).await.is_empty());
    // 套接字不存在：连接被断开。
    let mut gone = cfg("unix_domain_socket");
    gone.unix_path = "/nonexistent/rfrp.sock".into();
    let (_rig2, port2) = start(gone).await;
    let mut s = connect(port2).await;
    assert!(read_to_end(&mut s).await.is_empty());
    let _ = std::fs::remove_file(&path);
}

async fn tls_visitor(port: u16) -> tokio_rustls::client::TlsStream<TcpStream> {
    let name = ServerName::try_from("localhost").unwrap();
    TlsConnector::from(tls::app_client_config().unwrap())
        .connect(name, connect(port).await)
        .await
        .unwrap()
}

#[tokio::test]
async fn https2http_terminates_tls_and_rewrites_each_request() {
    let web = http_backend().await;
    let mut c = cfg("https2http");
    c.local_addr = format!("127.0.0.1:{web}");
    c.host_header_rewrite = "inner.lan".into();
    c.request_headers = vec![("X-From".into(), "plugin".into())];
    let (_rig, port) = start(c).await;
    let mut s = tls_visitor(port).await;
    // 同一条连接上的两个请求（含带正文的 POST）都被改写。
    for (i, req) in [
        "GET /one HTTP/1.1\r\nHost: a.example.com\r\n\r\n".to_string(),
        "POST /two HTTP/1.1\r\nHost: a.example.com\r\nContent-Length: 3\r\n\r\nabc".to_string(),
    ]
    .iter()
    .enumerate()
    {
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = timeout(T, s.read(&mut buf)).await.unwrap().unwrap();
        let r = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(r.starts_with("HTTP/1.1 200 OK"), "{i}: {r}");
        assert!(
            r.contains("Host: inner.lan\r\n") && r.contains("X-From: plugin\r\n"),
            "{i}: {r}"
        );
        if i == 0 {
            // 后端声明 Connection: close，所以第二个请求需要新连接。
            s = tls_visitor(port).await;
        }
    }
    // 分块正文与协议升级也能通过；坏请求断开。
    let mut s = tls_visitor(port).await;
    s.write_all(
        b"POST /c HTTP/1.1\r\nHost: a\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n",
    )
    .await
    .unwrap();
    assert!(read_to_end(&mut s).await.contains("Host: inner.lan"));
    let mut s = tls_visitor(port).await;
    s.write_all(
        b"GET /ws HTTP/1.1\r\nHost: a\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
    )
    .await
    .unwrap();
    assert!(read_to_end(&mut s).await.contains("GET /ws"));
    let mut s = tls_visitor(port).await;
    s.write_all(b"BAD\r\n\r\n").await.unwrap();
    assert!(read_to_end(&mut s).await.is_empty());
    // 明文访问者：TLS 握手失败，连接被丢弃。
    let mut plain = connect(port).await;
    plain.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
    let mut out = Vec::new();
    let _ = timeout(Duration::from_secs(3), plain.read_to_end(&mut out)).await;
    assert!(!String::from_utf8_lossy(&out).contains("200 OK"));
}

#[tokio::test]
async fn http2https_and_https2https_reach_a_tls_backend_and_tls2raw_reaches_a_raw_one() {
    // 本地 TLS 后端（自签名证书）。
    let id = tls::self_signed(vec!["localhost".into()]).unwrap();
    let acceptor = TlsAcceptor::from(tls::app_server_config(id).unwrap());
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut s) = acceptor.accept(s).await else {
                    return;
                };
                let mut b = [0u8; 2048];
                let n = s.read(&mut b).await.unwrap_or(0);
                let echoed = String::from_utf8_lossy(&b[..n]).to_string();
                let r = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{echoed}",
                    echoed.len()
                );
                let _ = s.write_all(r.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    let mut c = cfg("http2https");
    c.local_addr = format!("127.0.0.1:{backend}");
    c.host_header_rewrite = "secure.lan".into();
    let (_rig, port) = start(c).await;
    let mut s = connect(port).await;
    s.write_all(b"GET /x HTTP/1.1\r\nHost: a\r\n\r\n")
        .await
        .unwrap();
    let r = read_to_end(&mut s).await;
    assert!(
        r.contains("200 OK") && r.contains("Host: secure.lan"),
        "{r}"
    );

    // 自定义证书（PEM 文件内容）+ https2https。
    let (crt, key) = tls::self_signed_pem(vec!["localhost".into()]).unwrap();
    let mut c = cfg("https2https");
    c.local_addr = format!("127.0.0.1:{backend}");
    c.crt_pem = crt;
    c.key_pem = key;
    let (_rig2, port2) = start(c).await;
    let mut s = tls_visitor(port2).await;
    s.write_all(b"GET /y HTTP/1.1\r\nHost: b\r\n\r\n")
        .await
        .unwrap();
    let r = read_to_end(&mut s).await;
    assert!(r.contains("200 OK") && r.contains("GET /y"), "{r}");

    // tls2raw：TLS 访问者 → 原始 tcp 回显服务。
    let raw = echo().await;
    let mut c = cfg("tls2raw");
    c.local_addr = format!("127.0.0.1:{raw}");
    let (_rig3, port3) = start(c).await;
    let mut s = tls_visitor(port3).await;
    s.write_all(b"raw!").await.unwrap();
    let mut got = [0u8; 4];
    timeout(T, s.read_exact(&mut got)).await.unwrap().unwrap();
    assert_eq!(&got, b"raw!");
    // 本地服务不可达：断开。
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let mut c = cfg("tls2raw");
    c.local_addr = format!("127.0.0.1:{dead_port}");
    let (_rig4, port4) = start(c).await;
    let mut s = tls_visitor(port4).await;
    s.write_all(b"x").await.unwrap();
    assert!(read_to_end(&mut s).await.is_empty());
}

#[tokio::test]
async fn invalid_plugin_configs_fail_to_start() {
    let bad = |c: PluginCfg| rustfrp_agent::plugin::Plugin::new(c).err().unwrap();
    assert!(bad(cfg("virtual_net")).contains("not supported"));
    assert!(bad(cfg("static_file")).contains("localPath is required"));
    assert!(bad(cfg("unix_domain_socket")).contains("unixPath is required"));
    assert!(bad(cfg("https2http")).contains("localAddr is required"));
    let mut c = cfg("https2http");
    c.local_addr = "127.0.0.1:1".into();
    c.crt_pem = "not pem".into();
    c.key_pem = "not pem".into();
    assert!(bad(c).contains("https2http"));
    assert!(rustfrp_agent::plugin::Plugin::new(cfg("socks5")).is_ok());
    assert_eq!(
        format!(
            "{:?}",
            rustfrp_agent::plugin::Plugin::new(cfg("socks5")).unwrap()
        ),
        "Plugin(socks5)"
    );
}

#[tokio::test]
async fn invalid_plugin_is_reported_to_the_client_and_limits_apply_to_plugins() {
    let err = start_with(cfg("static_file"), 0).await.err().unwrap();
    assert!(err.contains("localPath is required"), "{err}");
    // 客户端侧限速也作用于插件：64 KiB/s 下传 192 KiB 至少要约 2 秒。
    let root = site();
    let mut c = cfg("static_file");
    c.local_path = root.to_string_lossy().into_owned();
    let (_rig, port) = start_with(c, 64 * 1024).await.unwrap();
    let began = std::time::Instant::now();
    let mut s = connect(port).await;
    s.write_all(b"GET /big.bin HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut all = Vec::new();
    timeout(T, s.read_to_end(&mut all)).await.unwrap().unwrap();
    assert!(all.len() > 300_000);
    assert!(
        began.elapsed() >= Duration::from_millis(1500),
        "{:?}",
        began.elapsed()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn socks5_protocol_edge_cases() {
    let target = echo().await;
    let mut c = cfg("socks5");
    c.username = "u".into();
    c.password = "p".into();
    let (_rig, port) = start(c).await;
    // 认证子协商版本不对。
    let mut s = connect(port).await;
    s.write_all(&[5, 1, 2]).await.unwrap();
    let mut m = [0u8; 2];
    s.read_exact(&mut m).await.unwrap();
    s.write_all(&[9, 1, b'u', 1, b'p']).await.unwrap();
    assert!(read_to_end(&mut s).await.is_empty());
    // IPv6 地址类型（目标是 ::1 上的回显服务；没有 IPv6 的环境只检查应答被拒或成功）。
    let mut s = connect(port).await;
    s.write_all(&[5, 1, 2]).await.unwrap();
    s.read_exact(&mut m).await.unwrap();
    s.write_all(&[1, 1, b'u', 1, b'p']).await.unwrap();
    s.read_exact(&mut m).await.unwrap();
    let mut req = vec![5, 1, 0, 4];
    req.extend([0u8; 15]);
    req.push(1);
    req.extend(target.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await.unwrap();
    assert!(rep[1] == 0 || rep[1] == 5, "{rep:?}");
    // 客户端中途离开。
    let mut s = connect(port).await;
    s.write_all(&[5, 1]).await.unwrap();
    drop(s);
}

#[tokio::test]
async fn http_proxy_error_paths() {
    let (_rig, port) = start(cfg("http_proxy")).await;
    // CONNECT 缺端口默认 443：本机 443 不可达 → 502。
    let mut s = connect(port).await;
    s.write_all(b"CONNECT 127.0.0.1 HTTP/1.1\r\n\r\n")
        .await
        .unwrap();
    let r = read_to_end(&mut s).await;
    assert!(
        r.starts_with("HTTP/1.1 502") || r.starts_with("HTTP/1.1 200"),
        "{r}"
    );
    // 绝对形式的目标不可达 → 502。
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = l.local_addr().unwrap().port();
    drop(l);
    let mut s = connect(port).await;
    s.write_all(format!("GET http://127.0.0.1:{dead}/ HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
        .await
        .unwrap();
    assert!(read_to_end(&mut s).await.starts_with("HTTP/1.1 502"));
    // 过大的请求头 / 头没发完就断开：直接关闭。
    let mut s = connect(port).await;
    s.write_all(&vec![b'a'; 20 * 1024]).await.unwrap();
    assert!(read_to_end(&mut s).await.is_empty());
    let mut s = connect(port).await;
    s.write_all(b"GET http://x/ HTTP/1.1\r\nHost")
        .await
        .unwrap();
    s.shutdown().await.unwrap();
    assert!(read_to_end(&mut s).await.is_empty());
}

#[tokio::test]
async fn static_file_bad_requests_unreadable_entries_and_long_listings() {
    let root = site();
    let mut c = cfg("static_file");
    c.local_path = root.to_string_lossy().into_owned();
    let (_rig, port) = start(c).await;
    // 目录列表标出子目录，并有数量上限。
    let r = get(port, "GET", "/docs/sub/", "").await;
    assert!(r.contains("data.json"), "{r}");
    let r = get(port, "GET", "/", "").await;
    assert!(r.contains("href=\"docs/\"") && r.contains("big.bin"), "{r}");
    let many = root.join("many");
    std::fs::create_dir_all(&many).unwrap();
    for i in 0..1100 {
        std::fs::write(many.join(format!("f{i:04}")), "").unwrap();
    }
    let r = get(port, "GET", "/many/", "").await;
    assert_eq!(r.matches("<a href").count(), 1000, "listing is bounded");
    // 无法解析的请求。
    let mut s = connect(port).await;
    s.write_all(b"GET /a HTTP/1.1\r\nHost: x\r\nBad Header\r\n\r\n")
        .await
        .unwrap();
    assert!(read_to_end(&mut s).await.starts_with("HTTP/1.1 400"));
    // 不可读的文件 / 目录（root 用户不受权限限制，跳过）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = root.join("locked.txt");
        std::fs::write(&locked, "x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let dir = root.join("locked-dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::File::open(&locked).is_err();
        if unreadable {
            assert!(get(port, "GET", "/locked.txt", "")
                .await
                .starts_with("HTTP/1.1 404"));
            assert!(get(port, "GET", "/locked-dir/", "")
                .await
                .starts_with("HTTP/1.1 403"));
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn https2http_connection_edge_cases() {
    let web = http_backend().await;
    let mut c = cfg("https2http");
    c.local_addr = format!("127.0.0.1:{web}");
    c.request_headers = vec![("X-From".into(), "plugin".into())];
    let (_rig, port) = start(c).await;
    // 访问者发完请求就半关闭：仍能收到完整响应。
    let mut s = tls_visitor(port).await;
    s.write_all(b"GET /half HTTP/1.1\r\nHost: a\r\n\r\n")
        .await
        .unwrap();
    s.shutdown().await.unwrap();
    assert!(read_to_end(&mut s).await.contains("X-From: plugin"));
    // 正文晚于请求头到达（分两次读）也能完整转发。
    let mut s = tls_visitor(port).await;
    s.write_all(b"POST /late HTTP/1.1\r\nHost: a\r\nContent-Length: 6\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.write_all(b"abcdef").await.unwrap();
    assert!(read_to_end(&mut s).await.contains("POST /late"));
    // 请求头过大、头发一半断开、正文发一半断开：连接被关闭，不 panic。
    for payload in [
        vec![b'a'; 80 * 1024],
        b"GET /x HTTP/1.1\r\nHost".to_vec(),
        b"POST /x HTTP/1.1\r\nHost: a\r\nContent-Length: 10\r\n\r\nabc".to_vec(),
    ] {
        let mut s = tls_visitor(port).await;
        s.write_all(&payload).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.shutdown().await.unwrap();
        // 本地服务拿到完整请求头后就会应答；这里只要求连接最终被关闭、插件不崩溃。
        let _ = read_to_end(&mut s).await;
    }
    // 本地服务不主动关闭（回显）：访问者先结束时由插件收尾，升级后的字节原样透传。
    let raw = echo().await;
    let mut c = cfg("https2http");
    c.local_addr = format!("127.0.0.1:{raw}");
    let (_rig2, port2) = start(c).await;
    let mut s = tls_visitor(port2).await;
    s.write_all(b"GET /ws HTTP/1.1\r\nHost: a\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\nextra-bytes")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.write_all(b"more").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.shutdown().await.unwrap();
    let r = read_to_end(&mut s).await;
    assert!(
        r.contains("GET /ws") && r.contains("extra-bytes") && r.contains("more"),
        "{r}"
    );
    // 请求头收到一半 / 正文收到一半就离开。
    for payload in [
        &b"GET /x HTTP/1.1\r\nHost: a\r\n"[..],
        &b"POST /x HTTP/1.1\r\nHost: a\r\nContent-Length: 10\r\n\r\nabc"[..],
    ] {
        let mut s = tls_visitor(port2).await;
        s.write_all(payload).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        s.write_all(b"").await.unwrap();
        s.shutdown().await.unwrap();
        let _ = read_to_end(&mut s).await;
    }
}
