//! UDP 转发与 HTTP / HTTPS 虚拟主机路由（独立模式与受管模式）。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, Event, ProxyCfg};
use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

struct Rig {
    edge: SocketAddr,
    http: Option<SocketAddr>,
    https: Option<SocketAddr>,
    stop: CancellationToken,
}

async fn start(cfg: EdgeConfig) -> Rig {
    let cfg = EdgeConfig {
        bind_addr: "127.0.0.1".into(),
        bind_port: 0,
        proxy_bind_addr: "127.0.0.1".into(),
        ..cfg
    };
    let edge = Edge::bind(cfg, None, Logger::silent()).await.unwrap();
    let rig = Rig {
        edge: edge.local_addr().unwrap(),
        http: edge.http_addr(),
        https: edge.https_addr(),
        stop: CancellationToken::new(),
    };
    tokio::spawn(edge.serve(rig.stop.clone()));
    rig
}

fn vhost_cfg() -> EdgeConfig {
    EdgeConfig {
        vhost_http_port: Some(0),
        vhost_https_port: Some(0),
        sub_domain_host: "tunnel.example.com".into(),
        udp_idle: Duration::from_millis(400),
        ..Default::default()
    }
}

fn proxy(name: &str, kind: &str, local_port: u16) -> ProxyCfg {
    ProxyCfg {
        name: name.into(),
        kind: kind.into(),
        local_ip: "127.0.0.1".into(),
        local_port,
        ..Default::default()
    }
}

fn run_client(rig: &Rig, proxies: Vec<ProxyCfg>) -> (mpsc::Receiver<Event>, CancellationToken) {
    let (tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: rig.edge.port(),
        proxies,
        udp_idle: Duration::from_millis(400),
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

/// 回显 UDP 服务；`prefix` 方便区分是哪个后端。
async fn udp_echo(prefix: &'static [u8]) -> u16 {
    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = s.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        while let Ok((n, from)) = s.recv_from(&mut buf).await {
            let mut out = prefix.to_vec();
            out.extend_from_slice(&buf[..n]);
            let _ = s.send_to(&out, from).await;
        }
    });
    port
}

/// 把收到的全部字节回传（带前缀）的 TCP 后端：用来看清转发到底送了什么。
async fn tcp_backend(tag: &'static str) -> (u16, mpsc::UnboundedReceiver<Vec<u8>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let _ = tx.send(buf[..n].to_vec());
                let body = format!("backend={tag}");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    (port, rx)
}

async fn http_get(addr: SocketAddr, host: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET /x HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    timeout(T, s.read_to_string(&mut out))
        .await
        .unwrap()
        .unwrap();
    out
}

#[tokio::test]
async fn udp_proxy_forwards_per_peer_and_recycles_idle_sessions() {
    let rig = start(vhost_cfg()).await;
    let echo = udp_echo(b"echo:").await;
    let (mut rx, cancel) = run_client(&rig, vec![proxy("dns", "udp", echo)]);
    let Event::ProxyStarted { remote_port, .. } = events(&mut rx, 1).await.remove(0) else {
        panic!()
    };

    // 两个不同的访问者各自拿到自己的回复。
    let remote: SocketAddr = format!("127.0.0.1:{remote_port}").parse().unwrap();
    let (a, b) = (
        UdpSocket::bind("127.0.0.1:0").await.unwrap(),
        UdpSocket::bind("127.0.0.1:0").await.unwrap(),
    );
    let mut buf = vec![0u8; 2048];
    a.send_to(b"from-a", remote).await.unwrap();
    b.send_to(b"from-b", remote).await.unwrap();
    let n = timeout(T, a.recv(&mut buf)).await.unwrap().unwrap();
    assert_eq!(&buf[..n], b"echo:from-a");
    let n = timeout(T, b.recv(&mut buf)).await.unwrap().unwrap();
    assert_eq!(&buf[..n], b"echo:from-b");
    // 同一访问者的多个包复用同一条流，且保持顺序。
    for i in 0..5u8 {
        a.send_to(&[i], remote).await.unwrap();
    }
    for i in 0..5u8 {
        let n = timeout(T, a.recv(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], [b"echo:".as_slice(), &[i]].concat());
    }
    // 空闲回收之后，同一个访问者再来依然可用（会重新建流）。
    tokio::time::sleep(Duration::from_millis(900)).await;
    a.send_to(b"again", remote).await.unwrap();
    let n = timeout(T, a.recv(&mut buf)).await.unwrap().unwrap();
    assert_eq!(&buf[..n], b"echo:again");
    // 大一点的数据报（超过默认 MTU 但在 64KiB 之内）：受 udpPacketSize 限制会被截断，不应影响后续。
    a.send_to(&vec![9u8; 1000], remote).await.unwrap();
    let n = timeout(T, a.recv(&mut buf)).await.unwrap().unwrap();
    assert_eq!(n, 1005);
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn udp_proxy_with_unreachable_local_service_stays_quiet() {
    let rig = start(vhost_cfg()).await;
    let dead = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let (mut rx, cancel) = run_client(&rig, vec![proxy("dead", "udp", dead_port)]);
    let Event::ProxyStarted { remote_port, .. } = events(&mut rx, 1).await.remove(0) else {
        panic!()
    };
    let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    c.send_to(b"hello", format!("127.0.0.1:{remote_port}"))
        .await
        .unwrap();
    let mut buf = [0u8; 16];
    assert!(
        timeout(Duration::from_millis(500), c.recv(&mut buf))
            .await
            .is_err(),
        "no reply, no crash"
    );
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn http_routing_by_host_wildcard_subdomain_and_rewrite() {
    let rig = start(vhost_cfg()).await;
    let http = rig.http.unwrap();
    let ((pa, mut ra), (pb, _rb), (pw, _rw), (ps, _rs)) = (
        tcp_backend("a").await,
        tcp_backend("b").await,
        tcp_backend("wild").await,
        tcp_backend("sub").await,
    );
    let mut a = proxy("a", "http", pa);
    a.custom_domains = vec!["a.example.com".into()];
    a.host_header_rewrite = "internal.lan:8080".into();
    let mut b = proxy("b", "http", pb);
    b.custom_domains = vec!["b.example.com".into()];
    let mut w = proxy("w", "http", pw);
    w.custom_domains = vec!["*.wild.example.com".into()];
    let mut s = proxy("s", "http", ps);
    s.subdomain = "web".into();
    let (mut rx, cancel) = run_client(&rig, vec![a, b, w, s]);
    for e in events(&mut rx, 4).await {
        assert!(matches!(e, Event::ProxyStarted { .. }), "{e:?}");
    }

    assert!(http_get(http, "a.example.com").await.ends_with("backend=a"));
    assert!(
        http_get(http, "B.Example.COM:8080")
            .await
            .ends_with("backend=b"),
        "case and port are ignored"
    );
    assert!(http_get(http, "x.wild.example.com")
        .await
        .ends_with("backend=wild"));
    assert!(http_get(http, "web.tunnel.example.com")
        .await
        .ends_with("backend=sub"));
    // Host 改写只对配置了它的代理生效，且只改头部。
    let seen = String::from_utf8(ra.recv().await.unwrap()).unwrap();
    assert!(
        seen.starts_with("GET /x HTTP/1.1\r\nHost: internal.lan:8080\r\n"),
        "{seen}"
    );

    // 未知域名 → 404；没有 Host → 400；通配不匹配根域名。
    assert!(http_get(http, "unknown.example.com")
        .await
        .starts_with("HTTP/1.1 404"));
    assert!(http_get(http, "wild.example.com")
        .await
        .starts_with("HTTP/1.1 404"));
    let mut s = TcpStream::connect(http).await.unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nAccept: */*\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    timeout(T, s.read_to_string(&mut out))
        .await
        .unwrap()
        .unwrap();
    assert!(out.starts_with("HTTP/1.1 400"));
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn http_proxy_errors_and_route_cleanup() {
    // 没开 vhost 端口：明确报错。
    let plain = start(EdgeConfig::default()).await;
    let mut h = proxy("h", "http", 1);
    h.custom_domains = vec!["a.com".into()];
    let mut hs = proxy("hs", "https", 1);
    hs.custom_domains = vec!["a.com".into()];
    let (mut rx, cancel) = run_client(&plain, vec![h.clone(), hs]);
    let ev = events(&mut rx, 2).await;
    assert!(
        matches!(&ev[0], Event::ProxyFailed { error, .. } if error.contains("vhost http port is not set"))
    );
    assert!(
        matches!(&ev[1], Event::ProxyFailed { error, .. } if error.contains("vhost https port is not set"))
    );
    cancel.cancel();
    plain.stop.cancel();

    let rig = start(EdgeConfig {
        vhost_http_port: Some(0),
        ..Default::default()
    })
    .await;
    let mut empty = proxy("empty", "http", 1);
    empty.custom_domains = vec![];
    let mut sub = proxy("sub", "http", 1);
    sub.subdomain = "x".into();
    let mut dup1 = proxy("dup1", "http", 1);
    dup1.custom_domains = vec!["same.com".into()];
    let mut dup2 = proxy("dup2", "http", 1);
    dup2.custom_domains = vec!["SAME.com".into(), "other.com".into()];
    let (mut rx, cancel) = run_client(&rig, vec![empty, sub, dup1, dup2]);
    let ev = events(&mut rx, 4).await;
    assert!(
        matches!(&ev[0], Event::ProxyFailed { error, .. } if error.contains("should not be both empty"))
    );
    assert!(
        matches!(&ev[1], Event::ProxyFailed { error, .. } if error.contains("not enabled in server"))
    );
    assert!(matches!(&ev[2], Event::ProxyStarted { .. }));
    assert!(matches!(&ev[3], Event::ProxyFailed { error, .. } if error.contains("already used")));
    // 失败的代理不会留下半条路由：other.com 没有登记。
    let http = rig.http.unwrap();
    assert!(http_get(http, "other.com")
        .await
        .starts_with("HTTP/1.1 404"));
    // 客户端断开后路由被清理：404 而不是挂住。
    cancel.cancel();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(http_get(http, "same.com").await.starts_with("HTTP/1.1 404"));
    rig.stop.cancel();
}

#[tokio::test]
async fn slow_and_huge_heads_are_dropped() {
    let rig = start(EdgeConfig {
        vhost_http_port: Some(0),
        vhost_head_timeout: Duration::from_millis(300),
        ..Default::default()
    })
    .await;
    let http = rig.http.unwrap();
    // 慢速客户端：只发一半请求头 → 超时断开。
    let mut s = TcpStream::connect(http).await.unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nHost: slow").await.unwrap();
    let mut b = [0u8; 8];
    let n = timeout(T, s.read(&mut b)).await.unwrap().unwrap_or(0);
    assert_eq!(n, 0);
    // 超大请求头（没有空行）→ 400 并断开，不会无限缓冲。
    let mut s = TcpStream::connect(http).await.unwrap();
    s.write_all(&vec![b'a'; 20_000]).await.unwrap();
    let mut out = Vec::new();
    let _ = timeout(T, s.read_to_end(&mut out)).await.unwrap();
    assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 400"));
    // 连上就关：什么也不会发生。
    drop(TcpStream::connect(http).await.unwrap());
    rig.stop.cancel();
}

fn client_hello(sni: &str) -> Vec<u8> {
    let n = sni.as_bytes();
    let mut ext = vec![0, 0];
    let list = 3 + n.len();
    ext.extend(((list + 2) as u16).to_be_bytes());
    ext.extend((list as u16).to_be_bytes());
    ext.push(0);
    ext.extend((n.len() as u16).to_be_bytes());
    ext.extend(n);
    let mut hs = vec![3, 3];
    hs.extend([1u8; 32]);
    hs.push(0);
    hs.extend([0, 2, 0x13, 1, 1, 0]);
    hs.extend((ext.len() as u16).to_be_bytes());
    hs.extend(ext);
    let mut body = vec![1, 0, (hs.len() >> 8) as u8, hs.len() as u8];
    body.extend(hs);
    let mut rec = vec![0x16, 3, 1];
    rec.extend((body.len() as u16).to_be_bytes());
    rec.extend(body);
    rec
}

#[tokio::test]
async fn https_routing_by_sni_is_a_transparent_passthrough() {
    let rig = start(vhost_cfg()).await;
    let https = rig.https.unwrap();
    let (backend, mut seen) = tcp_backend("tls").await;
    let mut p = proxy("secure", "https", backend);
    p.custom_domains = vec!["secure.example.com".into()];
    let (mut rx, cancel) = run_client(&rig, vec![p]);
    assert!(matches!(
        events(&mut rx, 1).await[0],
        Event::ProxyStarted { .. }
    ));

    // ClientHello 被原样（一字节不差）送到后端：边缘节点不解密、不改写。
    let hello = client_hello("secure.example.com");
    let mut s = TcpStream::connect(https).await.unwrap();
    s.write_all(&hello).await.unwrap();
    let received = timeout(T, seen.recv()).await.unwrap().unwrap();
    assert_eq!(received, hello);
    let mut out = String::new();
    timeout(T, s.read_to_string(&mut out))
        .await
        .unwrap()
        .unwrap();
    assert!(out.ends_with("backend=tls"));

    // 未知 SNI / 没有 SNI / 非 TLS：直接断开。
    for bytes in [
        client_hello("nobody.example.com"),
        b"GET / HTTP/1.1\r\n\r\n".to_vec(),
    ] {
        let mut s = TcpStream::connect(https).await.unwrap();
        s.write_all(&bytes).await.unwrap();
        let mut b = Vec::new();
        let _ = timeout(T, s.read_to_end(&mut b)).await.unwrap();
        assert!(b.is_empty());
    }
    // 分两次发送 ClientHello 也能识别。
    let mut s = TcpStream::connect(https).await.unwrap();
    s.write_all(&hello[..10]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    s.write_all(&hello[10..]).await.unwrap();
    assert_eq!(timeout(T, seen.recv()).await.unwrap().unwrap(), hello);
    cancel.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn udp_port_policy_and_peer_limit() {
    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let base = probe.local_addr().unwrap().port();
    drop(probe);
    let busy = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    let rig = start(EdgeConfig {
        allow_ports: vec![(base, base.saturating_add(3)), (busy_port, busy_port)],
        udp_max_peers: 1,
        ..Default::default()
    })
    .await;
    let echo = udp_echo(b"").await;
    let mut auto = proxy("auto", "udp", echo);
    auto.remote_port = 0;
    let mut denied = proxy("denied", "udp", echo);
    denied.remote_port = 1;
    let mut taken = proxy("taken", "udp", echo);
    taken.remote_port = busy_port;
    let (mut rx, cancel) = run_client(&rig, vec![auto, denied, taken]);
    let ev = events(&mut rx, 3).await;
    let Event::ProxyStarted { remote_port, .. } = ev[0].clone() else {
        panic!("{:?}", ev[0])
    };
    assert!((base..=base.saturating_add(3)).contains(&remote_port));
    assert!(
        matches!(&ev[1], Event::ProxyFailed { error, .. } if error.contains("port not allowed"))
    );
    assert!(
        matches!(&ev[2], Event::ProxyFailed { error, .. } if error.contains("listen on udp port"))
    );

    // 只允许 1 个访问者：第二个访问者的包被丢弃。
    let remote: SocketAddr = format!("127.0.0.1:{remote_port}").parse().unwrap();
    let (a, b) = (
        UdpSocket::bind("127.0.0.1:0").await.unwrap(),
        UdpSocket::bind("127.0.0.1:0").await.unwrap(),
    );
    let mut buf = [0u8; 64];
    a.send_to(b"a", remote).await.unwrap();
    assert_eq!(timeout(T, a.recv(&mut buf)).await.unwrap().unwrap(), 1);
    b.send_to(b"b", remote).await.unwrap();
    assert!(timeout(Duration::from_millis(400), b.recv(&mut buf))
        .await
        .is_err());
    cancel.cancel();

    // 范围内所有端口都被占用 → 明确的错误。
    let rig2 = start(EdgeConfig {
        allow_ports: vec![(busy_port, busy_port)],
        ..Default::default()
    })
    .await;
    let mut any = proxy("any", "udp", echo);
    any.remote_port = 0;
    let (mut rx, cancel) = run_client(&rig2, vec![any]);
    assert!(
        matches!(&events(&mut rx, 1).await[0], Event::ProxyFailed { error, .. } if error.contains("no available port"))
    );
    cancel.cancel();
    rig.stop.cancel();
    rig2.stop.cancel();
    drop(busy);
}

#[tokio::test]
async fn udp_to_an_ipv6_local_target() {
    let Ok(echo) = UdpSocket::bind("[::1]:0").await else {
        return;
    };
    let port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut b = [0u8; 512];
        while let Ok((n, from)) = echo.recv_from(&mut b).await {
            let _ = echo.send_to(&b[..n], from).await;
        }
    });
    let rig = start(EdgeConfig::default()).await;
    let mut p = proxy("v6", "udp", port);
    p.local_ip = "::1".into();
    let (mut rx, cancel) = run_client(&rig, vec![p]);
    let Event::ProxyStarted { remote_port, .. } = events(&mut rx, 1).await.remove(0) else {
        panic!()
    };
    let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    c.send_to(b"six", ("127.0.0.1", remote_port)).await.unwrap();
    let mut buf = [0u8; 16];
    let n = timeout(T, c.recv(&mut buf)).await.unwrap().unwrap();
    assert_eq!(&buf[..n], b"six");
    // 无法解析的本地主机名：该访问者的流被关闭，代理本身继续工作。
    cancel.cancel();
    rig.stop.cancel();
}
