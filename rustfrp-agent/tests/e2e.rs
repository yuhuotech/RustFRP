//! 端到端：真实 TLS + 多路复用 + 回环 TCP，验证 Edge 与 Client 的完整隧道链路。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, ClientError, Event, ProxyCfg};
use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use rustfrp_core::tls;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

struct Harness {
    edge_addr: SocketAddr,
    shutdown: CancellationToken,
}

async fn start_edge(mut cfg: EdgeConfig) -> Harness {
    cfg.bind_addr = "127.0.0.1".into();
    cfg.bind_port = 0;
    cfg.proxy_bind_addr = "127.0.0.1".into();
    let edge = Edge::bind(cfg, None, Logger::silent()).await.unwrap();
    let edge_addr = edge.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    tokio::spawn(edge.serve(shutdown.clone()));
    Harness {
        edge_addr,
        shutdown,
    }
}

async fn start_echo() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            if s.write_all(&buf[..n]).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    port
}

fn tcp_proxy(name: &str, local_port: u16, remote_port: u16) -> ProxyCfg {
    ProxyCfg {
        name: name.into(),
        kind: "tcp".into(),
        local_ip: "127.0.0.1".into(),
        local_port,
        remote_port,
        ..Default::default()
    }
}

fn client_cfg(h: &Harness, proxies: Vec<ProxyCfg>) -> ClientConfig {
    ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: h.edge_addr.port(),
        proxies,
        backoff: (Duration::from_millis(50), Duration::from_millis(200)),
        heartbeat_interval: Duration::from_millis(100),
        heartbeat_timeout: Duration::from_secs(5),
        ..Default::default()
    }
}

fn spawn_client(
    cfg: ClientConfig,
) -> (
    mpsc::Receiver<Event>,
    CancellationToken,
    tokio::task::JoinHandle<Result<(), ClientError>>,
) {
    let (tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
    (rx, cancel, task)
}

async fn next_event(rx: &mut mpsc::Receiver<Event>) -> Event {
    timeout(T, rx.recv())
        .await
        .expect("event timeout")
        .expect("event channel closed")
}

async fn expect_proxy_port(rx: &mut mpsc::Receiver<Event>) -> u16 {
    loop {
        match next_event(rx).await {
            Event::ProxyStarted { remote_port, .. } => return remote_port,
            Event::ProxyFailed { error, .. } => panic!("proxy failed: {error}"),
            _ => {}
        }
    }
}

async fn echo_roundtrip(port: u16, payload: &[u8]) {
    let mut s = timeout(T, TcpStream::connect(("127.0.0.1", port)))
        .await
        .unwrap()
        .unwrap();
    let (mut r, mut w) = s.split();
    let write = async {
        w.write_all(payload).await.unwrap();
        w.shutdown().await.unwrap();
    };
    let read = async {
        let mut got = Vec::new();
        r.read_to_end(&mut got).await.unwrap();
        got
    };
    let (_, got) = timeout(T, async { tokio::join!(write, read) })
        .await
        .unwrap();
    assert_eq!(got.len(), payload.len());
    assert!(got == payload);
}

#[tokio::test]
async fn tcp_tunnel_carries_data_both_ways() {
    let h = start_edge(EdgeConfig::default()).await;
    let echo = start_echo().await;
    let (mut rx, cancel, task) = spawn_client(client_cfg(&h, vec![tcp_proxy("echo", echo, 0)]));
    assert_eq!(next_event(&mut rx).await, Event::Connected);
    let port = expect_proxy_port(&mut rx).await;
    assert_ne!(port, 0);

    echo_roundtrip(port, b"hello through the tunnel").await;
    // 1 MiB，且多条并发，验证流控与多路复用。
    let big: Vec<u8> = (0..1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let big = big.clone();
        set.spawn(async move { echo_roundtrip(port, &big).await });
    }
    while let Some(r) = set.join_next().await {
        r.unwrap();
    }

    cancel.cancel();
    assert!(timeout(T, task).await.unwrap().unwrap().is_ok());
    h.shutdown.cancel();
}

#[tokio::test]
async fn token_is_enforced() {
    let h = start_edge(EdgeConfig {
        token: "s3cret".into(),
        ..Default::default()
    })
    .await;
    let echo = start_echo().await;

    let mut bad = client_cfg(&h, vec![tcp_proxy("echo", echo, 0)]);
    bad.token = "wrong".into();
    let (_rx, _c, task) = spawn_client(bad);
    let err = timeout(T, task).await.unwrap().unwrap().unwrap_err();
    assert!(matches!(err, ClientError::LoginFailed(m) if m.contains("token")));

    let mut good = client_cfg(&h, vec![tcp_proxy("echo", echo, 0)]);
    good.token = "s3cret".into();
    let (mut rx, cancel, task) = spawn_client(good);
    assert_eq!(next_event(&mut rx).await, Event::Connected);
    let port = expect_proxy_port(&mut rx).await;
    echo_roundtrip(port, b"authorized").await;
    cancel.cancel();
    task.await.unwrap().unwrap();
    h.shutdown.cancel();
}

#[tokio::test]
async fn login_failure_retries_when_not_fatal() {
    let h = start_edge(EdgeConfig {
        token: "a".into(),
        ..Default::default()
    })
    .await;
    let mut cfg = client_cfg(&h, vec![]);
    cfg.token = "b".into();
    cfg.login_fail_exit = false;
    let (_rx, cancel, task) = spawn_client(cfg);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!task.is_finished(), "client keeps retrying");
    cancel.cancel();
    task.await.unwrap().unwrap();
    h.shutdown.cancel();
}

#[tokio::test]
async fn edge_policy_rejections_reach_the_client() {
    let h = start_edge(EdgeConfig {
        allow_ports: vec![(0, 0)],
        ..Default::default()
    })
    .await;
    // allow_ports = [(0,0)] 视为「只允许 0」，任何显式端口都应被拒绝。
    let echo = start_echo().await;
    let (mut rx, cancel, _t) = spawn_client(client_cfg(&h, vec![tcp_proxy("p1", echo, 45678)]));
    assert_eq!(next_event(&mut rx).await, Event::Connected);
    match next_event(&mut rx).await {
        Event::ProxyFailed { name, error } => {
            assert_eq!(name, "p1");
            assert!(error.contains("port not allowed"), "{error}");
        }
        other => panic!("{other:?}"),
    }
    cancel.cancel();
    h.shutdown.cancel();
}

#[tokio::test]
async fn unsupported_type_duplicate_names_and_port_cap() {
    let h = start_edge(EdgeConfig {
        max_ports_per_client: 2,
        ..Default::default()
    })
    .await;
    let echo = start_echo().await;
    let mut udp = tcp_proxy("u", echo, 0);
    udp.kind = "stcp".into();
    let proxies = vec![
        udp,
        tcp_proxy("a", echo, 0),
        tcp_proxy("a", echo, 0),
        tcp_proxy("b", echo, 0),
        tcp_proxy("c", echo, 0),
    ];
    let (mut rx, cancel, _t) = spawn_client(client_cfg(&h, proxies));
    assert_eq!(next_event(&mut rx).await, Event::Connected);
    let mut failures = Vec::new();
    let mut started = Vec::new();
    for _ in 0..5 {
        match next_event(&mut rx).await {
            Event::ProxyFailed { name, error } => failures.push((name, error)),
            Event::ProxyStarted { name, .. } => started.push(name),
            other => panic!("{other:?}"),
        }
    }
    assert!(failures[0].1.contains("not supported yet"));
    assert!(failures[1].1.contains("already exists"));
    assert!(failures[2].1.contains("max_ports_per_client"));
    assert_eq!(started, vec!["a", "b"]);
    cancel.cancel();
    h.shutdown.cancel();
}

#[tokio::test]
async fn allow_ports_picks_a_free_port_from_the_range() {
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = probe.local_addr().unwrap().port();
    drop(probe);
    let h = start_edge(EdgeConfig {
        allow_ports: vec![(base, base.saturating_add(20))],
        ..Default::default()
    })
    .await;
    let echo = start_echo().await;
    let (mut rx, cancel, _t) = spawn_client(client_cfg(&h, vec![tcp_proxy("r", echo, 0)]));
    assert_eq!(next_event(&mut rx).await, Event::Connected);
    let port = expect_proxy_port(&mut rx).await;
    assert!((base..=base.saturating_add(20)).contains(&port));
    echo_roundtrip(port, b"ranged").await;
    cancel.cancel();
    h.shutdown.cancel();
}

#[tokio::test]
async fn exhausted_allow_ports_and_busy_ports() {
    let busy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    let h = start_edge(EdgeConfig {
        allow_ports: vec![(busy_port, busy_port)],
        ..Default::default()
    })
    .await;
    let echo = start_echo().await;
    let (mut rx, cancel, _t) = spawn_client(client_cfg(
        &h,
        vec![
            tcp_proxy("auto", echo, 0),
            tcp_proxy("fixed", echo, busy_port),
        ],
    ));
    assert_eq!(next_event(&mut rx).await, Event::Connected);
    let mut errors = Vec::new();
    for _ in 0..2 {
        if let Event::ProxyFailed { error, .. } = next_event(&mut rx).await {
            errors.push(error);
        }
    }
    assert!(errors[0].contains("no available port"));
    assert!(errors[1].contains("listen on port"));
    cancel.cancel();
    h.shutdown.cancel();
}

#[tokio::test]
async fn visitor_is_dropped_when_local_service_is_down() {
    let h = start_edge(EdgeConfig::default()).await;
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let (mut rx, cancel, _t) = spawn_client(client_cfg(&h, vec![tcp_proxy("dead", dead_port, 0)]));
    assert_eq!(next_event(&mut rx).await, Event::Connected);
    let port = expect_proxy_port(&mut rx).await;
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let _ = s.write_all(b"x").await;
    let mut buf = [0u8; 1];
    let n = timeout(T, s.read(&mut buf)).await.unwrap().unwrap_or(0);
    assert_eq!(
        n, 0,
        "tunnel closes when the local service cannot be reached"
    );
    cancel.cancel();
    h.shutdown.cancel();
}

#[tokio::test]
async fn client_reconnects_after_the_edge_comes_back() {
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let echo = start_echo().await;
    let mut cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: port,
        proxies: vec![tcp_proxy("echo", echo, 0)],
        backoff: (Duration::from_millis(30), Duration::from_millis(100)),
        dial_timeout: Duration::from_millis(500),
        ..Default::default()
    };
    cfg.login_fail_exit = false;
    let (mut rx, cancel, _t) = spawn_client(cfg);
    // 边缘节点还没起来：先收到断开事件。
    assert!(matches!(next_event(&mut rx).await, Event::Disconnected(_)));

    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: port,
            proxy_bind_addr: "127.0.0.1".into(),
            ..Default::default()
        },
        None,
        Logger::silent(),
    )
    .await
    .unwrap();
    let shutdown = CancellationToken::new();
    tokio::spawn(edge.serve(shutdown.clone()));
    loop {
        if next_event(&mut rx).await == Event::Connected {
            break;
        }
    }
    let remote = expect_proxy_port(&mut rx).await;
    echo_roundtrip(remote, b"back online").await;

    // 边缘节点退出 → 客户端感知断线并继续重试。
    shutdown.cancel();
    loop {
        if matches!(next_event(&mut rx).await, Event::Disconnected(_)) {
            break;
        }
    }
    cancel.cancel();
}

#[tokio::test]
async fn silent_client_is_dropped_by_the_edge() {
    let h = start_edge(EdgeConfig {
        heartbeat_timeout: Duration::from_millis(300),
        ..Default::default()
    })
    .await;
    let mut cfg = client_cfg(&h, vec![]);
    cfg.heartbeat_interval = Duration::from_secs(60);
    let (mut rx, cancel, _t) = spawn_client(cfg);
    assert_eq!(next_event(&mut rx).await, Event::Connected);
    assert!(matches!(next_event(&mut rx).await, Event::Disconnected(_)));
    cancel.cancel();
    h.shutdown.cancel();
}

#[tokio::test]
async fn client_notices_unresponsive_edge() {
    // 一个只完成 TCP 握手、之后不说话的「假边缘」：客户端应在握手超时后断开重试。
    let fake = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = fake.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = fake.accept().await {
            held.push(s);
        }
    });
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: port,
        handshake_timeout: Duration::from_millis(200),
        backoff: (Duration::from_millis(30), Duration::from_millis(60)),
        login_fail_exit: false,
        ..Default::default()
    };
    let (mut rx, cancel, _t) = spawn_client(cfg);
    assert!(matches!(next_event(&mut rx).await, Event::Disconnected(m) if m.contains("timeout")));
    cancel.cancel();
}

#[tokio::test]
async fn trusted_ca_is_verified_and_bad_config_is_rejected() {
    // 正确的 CA：校验通过。
    let id = tls::self_signed(vec!["localhost".into()]).unwrap();
    let ca_pem = {
        use std::fmt::Write;
        let mut pem = String::new();
        let b64 = |der: &[u8]| {
            const A: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for c in der.chunks(3) {
                let n = (u32::from(c[0]) << 16)
                    | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                    | u32::from(*c.get(2).unwrap_or(&0));
                for i in 0..4 {
                    if i <= c.len() {
                        out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
                    } else {
                        out.push('=');
                    }
                }
            }
            out
        };
        let body = b64(id.certs[0].as_ref());
        writeln!(pem, "-----BEGIN CERTIFICATE-----").unwrap();
        for chunk in body.as_bytes().chunks(64) {
            writeln!(pem, "{}", std::str::from_utf8(chunk).unwrap()).unwrap();
        }
        writeln!(pem, "-----END CERTIFICATE-----").unwrap();
        pem
    };
    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            ..Default::default()
        },
        Some(id),
        Logger::silent(),
    )
    .await
    .unwrap();
    let port = edge.local_addr().unwrap().port();
    let shutdown = CancellationToken::new();
    tokio::spawn(edge.serve(shutdown.clone()));
    let cfg = ClientConfig {
        server_addr: "localhost".into(),
        server_port: port,
        trusted_ca_pem: Some(ca_pem.into_bytes()),
        ..Default::default()
    };
    let (mut rx, cancel, _t) = spawn_client(cfg);
    assert_eq!(next_event(&mut rx).await, Event::Connected);
    cancel.cancel();
    shutdown.cancel();

    // 无法解析的 CA：直接配置错误。
    let bad = ClientConfig {
        trusted_ca_pem: Some(b"not a pem".to_vec()),
        ..Default::default()
    };
    let err = client::run(bad, Logger::silent(), None, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Config(_)));
}
