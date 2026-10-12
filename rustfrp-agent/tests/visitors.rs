//! stcp / sudp 与访问者：两个客户端经同一个 Edge 私密互通，不占公网端口。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, Event, ProxyCfg};
use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use rustfrp_core::wire::{SecretOpts, VisitorSpec};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

struct Rig {
    port: u16,
    stop: CancellationToken,
    tokens: Vec<CancellationToken>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.cancel();
        for t in &self.tokens {
            t.cancel();
        }
    }
}

async fn rig() -> Rig {
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
    let port = edge.local_addr().unwrap().port();
    let stop = CancellationToken::new();
    tokio::spawn(edge.serve(stop.clone()));
    Rig {
        port,
        stop,
        tokens: vec![],
    }
}

fn spawn(
    rig: &mut Rig,
    user: &str,
    proxies: Vec<ProxyCfg>,
    visitors: Vec<VisitorSpec>,
) -> mpsc::Receiver<Event> {
    let (tx, rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    rig.tokens.push(cancel.clone());
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: rig.port,
        user: user.into(),
        proxies,
        visitors,
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    };
    tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel));
    rx
}

async fn echo_tcp() -> u16 {
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

async fn echo_udp() -> u16 {
    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = s.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut b = [0u8; 2048];
        while let Ok((n, from)) = s.recv_from(&mut b).await {
            let _ = s.send_to(&b[..n], from).await;
        }
    });
    port
}

fn secret_proxy(name: &str, kind: &str, local: u16, sk: &str, allow: &[&str]) -> ProxyCfg {
    ProxyCfg {
        name: name.into(),
        kind: kind.into(),
        local_ip: "127.0.0.1".into(),
        local_port: local,
        secret: SecretOpts {
            sk: sk.into(),
            allow_users: allow.iter().map(|s| s.to_string()).collect(),
        },
        ..Default::default()
    }
}

fn visitor(name: &str, kind: &str, server_user: &str, server: &str, sk: &str) -> VisitorSpec {
    VisitorSpec {
        name: name.into(),
        kind: kind.into(),
        server_user: server_user.into(),
        server_name: server.into(),
        sk: sk.into(),
        bind_addr: "127.0.0.1".into(),
        bind_port: 0,
        use_encryption: false,
        use_compression: false,
    }
}

fn transformed_visitor(
    name: &str,
    kind: &str,
    server_user: &str,
    server: &str,
    sk: &str,
) -> VisitorSpec {
    VisitorSpec {
        use_encryption: true,
        use_compression: true,
        ..visitor(name, kind, server_user, server, sk)
    }
}

async fn until<F: FnMut(&Event) -> bool>(rx: &mut mpsc::Receiver<Event>, mut f: F) -> Event {
    loop {
        let e = timeout(T, rx.recv())
            .await
            .expect("timeout")
            .expect("closed");
        if f(&e) {
            return e;
        }
    }
}

async fn visitor_port(rx: &mut mpsc::Receiver<Event>) -> u16 {
    match until(rx, |e| {
        matches!(
            e,
            Event::VisitorStarted { .. } | Event::VisitorFailed { .. }
        )
    })
    .await
    {
        Event::VisitorStarted { port, .. } => port,
        other => panic!("{other:?}"),
    }
}

async fn tcp_roundtrip(port: u16, msg: &[u8]) -> bool {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    if s.write_all(msg).await.is_err() {
        return false;
    }
    let mut got = vec![0u8; msg.len()];
    matches!(
        timeout(Duration::from_secs(3), s.read_exact(&mut got)).await,
        Ok(Ok(_))
    ) && got == msg
}

#[tokio::test]
async fn stcp_visitor_reaches_the_private_service_with_the_right_secret() {
    let mut rig = rig().await;
    let echo = echo_tcp().await;
    // 提供方带用户名前缀：完整名 alice.ssh；访问者属于另一个用户，需 allowUsers。
    let mut provider = spawn(
        &mut rig,
        "alice",
        vec![secret_proxy("alice.ssh", "stcp", echo, "s3cret", &["bob"])],
        vec![],
    );
    until(&mut provider, |e| {
        matches!(e, Event::ProxyStarted { remote_port: 0, .. })
    })
    .await;
    let mut bob = spawn(
        &mut rig,
        "bob",
        vec![],
        vec![
            transformed_visitor("good", "stcp", "alice", "ssh", "s3cret"),
            visitor("wrong-key", "stcp", "alice", "ssh", "nope"),
            visitor("missing", "stcp", "alice", "nothing", "s3cret"),
        ],
    );
    let (mut good, mut wrong, mut missing) = (None, None, None);
    for _ in 0..3 {
        if let Event::VisitorStarted { name, port } =
            until(&mut bob, |e| matches!(e, Event::VisitorStarted { .. })).await
        {
            match name.as_str() {
                "good" => good = Some(port),
                "wrong-key" => wrong = Some(port),
                _ => missing = Some(port),
            }
        }
    }
    assert!(tcp_roundtrip(good.unwrap(), b"private hello").await);
    assert!(
        !tcp_roundtrip(wrong.unwrap(), b"x").await,
        "wrong secret is refused"
    );
    assert!(
        !tcp_roundtrip(missing.unwrap(), b"x").await,
        "unknown proxy is refused"
    );
}

#[tokio::test]
async fn allow_users_default_to_the_owner_only_and_star_allows_everyone() {
    let mut rig = rig().await;
    let echo = echo_tcp().await;
    let mut p = spawn(
        &mut rig,
        "alice",
        vec![
            secret_proxy("alice.mine", "stcp", echo, "k", &[]),
            secret_proxy("alice.shared", "stcp", echo, "k", &["*"]),
        ],
        vec![],
    );
    for _ in 0..2 {
        until(&mut p, |e| matches!(e, Event::ProxyStarted { .. })).await;
    }
    // 同一用户的另一台设备可以访问；其他用户只能访问 allowUsers=* 的。
    let mut same = spawn(
        &mut rig,
        "alice",
        vec![],
        vec![visitor("v", "stcp", "", "mine", "k")],
    );
    assert!(tcp_roundtrip(visitor_port(&mut same).await, b"same user").await);
    let mut other = spawn(
        &mut rig,
        "carol",
        vec![],
        vec![
            visitor("mine", "stcp", "alice", "mine", "k"),
            visitor("shared", "stcp", "alice", "shared", "k"),
        ],
    );
    let mut ports = std::collections::HashMap::new();
    for _ in 0..2 {
        if let Event::VisitorStarted { name, port } =
            until(&mut other, |e| matches!(e, Event::VisitorStarted { .. })).await
        {
            ports.insert(name, port);
        }
    }
    assert!(!tcp_roundtrip(ports["mine"], b"x").await);
    assert!(tcp_roundtrip(ports["shared"], b"everyone").await);
}

#[tokio::test]
async fn sudp_visitor_relays_datagrams_to_the_private_udp_service() {
    let mut rig = rig().await;
    let echo = echo_udp().await;
    let mut p = spawn(
        &mut rig,
        "",
        vec![secret_proxy("dns", "sudp", echo, "k", &[])],
        vec![],
    );
    until(&mut p, |e| matches!(e, Event::ProxyStarted { .. })).await;
    let mut v = spawn(
        &mut rig,
        "",
        vec![],
        vec![transformed_visitor("v", "sudp", "", "dns", "k")],
    );
    let port = visitor_port(&mut v).await;
    let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for msg in [&b"one"[..], b"two"] {
        c.send_to(msg, ("127.0.0.1", port)).await.unwrap();
        let mut buf = [0u8; 16];
        let n = timeout(T, c.recv(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], msg);
    }
    // 密钥不对：没有任何应答。
    let mut bad = spawn(
        &mut rig,
        "",
        vec![],
        vec![visitor("bad", "sudp", "", "dns", "wrong")],
    );
    let bad_port = visitor_port(&mut bad).await;
    c.send_to(b"x", ("127.0.0.1", bad_port)).await.unwrap();
    let mut buf = [0u8; 16];
    assert!(timeout(Duration::from_millis(500), c.recv(&mut buf))
        .await
        .is_err());
}

#[tokio::test]
async fn bind_conflicts_and_secret_proxy_misconfigurations_are_reported() {
    let mut rig = rig().await;
    let echo = echo_tcp().await;
    let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let taken_port = taken.local_addr().unwrap().port();
    let mut busy = visitor("busy", "stcp", "", "x", "k");
    busy.bind_port = taken_port;
    let mut rx = spawn(&mut rig, "", vec![], vec![busy]);
    let e = until(&mut rx, |e| matches!(e, Event::VisitorFailed { .. })).await;
    assert!(matches!(e, Event::VisitorFailed { name, .. } if name == "busy"));
    // 空密钥 / 组合 group 的 stcp 被拒绝。
    let mut grouped = secret_proxy("g", "stcp", echo, "k", &[]);
    grouped.lb.group = "grp".into();
    let mut p = spawn(
        &mut rig,
        "",
        vec![secret_proxy("nokey", "stcp", echo, "", &[]), grouped],
        vec![],
    );
    for want in [
        "secretKey must not be empty",
        "secret proxies don't support",
    ] {
        let e = until(&mut p, |e| matches!(e, Event::ProxyFailed { .. })).await;
        assert!(
            matches!(&e, Event::ProxyFailed { error, .. } if error.contains(want)),
            "{e:?}"
        );
    }
    drop(taken);
}

#[tokio::test]
async fn provider_leaving_unregisters_the_proxy_and_server_side_limits_apply() {
    let mut rig = rig().await;
    let echo = echo_tcp().await;
    let mut limited = secret_proxy("limited", "stcp", echo, "k", &[]);
    limited.bandwidth_bps = 1 << 20;
    limited.bandwidth_server = true;
    limited.max_connections = 1;
    let provider_cancel = {
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();
        let cfg = ClientConfig {
            server_addr: "127.0.0.1".into(),
            server_port: rig.port,
            proxies: vec![limited],
            heartbeat_interval: Duration::from_millis(100),
            ..Default::default()
        };
        tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
        until(&mut rx, |e| matches!(e, Event::ProxyStarted { .. })).await;
        cancel
    };
    let mut v = spawn(
        &mut rig,
        "",
        vec![],
        vec![visitor("v", "stcp", "", "limited", "k")],
    );
    let port = visitor_port(&mut v).await;
    let mut first = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    first
        .write_all(b"through the server-side limiter")
        .await
        .unwrap();
    let mut received = vec![0u8; b"through the server-side limiter".len()];
    timeout(T, first.read_exact(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, b"through the server-side limiter");
    assert!(!tcp_roundtrip(port, b"at the connection cap").await);
    drop(first);
    let mut released = false;
    for _ in 0..50 {
        if tcp_roundtrip(port, b"connection permit released").await {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(released);
    // 提供方断开后，Edge 摘掉登记：访问者得到「找不到代理」。
    provider_cancel.cancel();
    for _ in 0..50 {
        if !tcp_roundtrip(port, b"x").await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the proxy stayed registered after its owner left");
}
