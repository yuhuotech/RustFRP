//! 负载均衡组与健康检查（独立模式）。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, Event, ProxyCfg};
use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use rustfrp_core::wire::{HealthSpec, LbOpts};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

struct Rig {
    edge_port: u16,
    http: u16,
    https: u16,
    stop: CancellationToken,
}

async fn rig() -> Rig {
    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            vhost_http_port: Some(0),
            vhost_https_port: Some(0),
            ..Default::default()
        },
        None,
        Logger::silent(),
    )
    .await
    .unwrap();
    let r = Rig {
        edge_port: edge.local_addr().unwrap().port(),
        http: edge.http_addr().unwrap().port(),
        https: edge.https_addr().unwrap().port(),
        stop: CancellationToken::new(),
    };
    tokio::spawn(edge.serve(r.stop.clone()));
    r
}

/// 回显 `tag` 的 tcp 服务；`http` 为真时按 HTTP 回包。返回端口。
async fn backend(tag: &'static str, http: bool, status: u16) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    backend_on(l, tag, http, status);
    port
}

fn backend_on(l: TcpListener, tag: &'static str, http: bool, status: u16) -> CancellationToken {
    let stop = CancellationToken::new();
    let s2 = stop.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = s2.cancelled() => return,
                a = l.accept() => {
                    let Ok((mut s, _)) = a else { continue };
                    tokio::spawn(async move {
                        if http {
                            let mut b = [0u8; 2048];
                            let _ = s.read(&mut b).await;
                            let body = format!("backend={tag}");
                            let r = format!(
                                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = s.write_all(r.as_bytes()).await;
                        } else {
                            let _ = s.write_all(tag.as_bytes()).await;
                        }
                        let _ = s.shutdown().await;
                    });
                }
            }
        }
    });
    stop
}

fn proxy(name: &str, kind: &str, local: u16, lb: LbOpts) -> ProxyCfg {
    ProxyCfg {
        name: name.into(),
        kind: kind.into(),
        local_ip: "127.0.0.1".into(),
        local_port: local,
        lb,
        ..Default::default()
    }
}

fn group(g: &str, key: &str) -> LbOpts {
    LbOpts {
        group: g.into(),
        group_key: key.into(),
        health: None,
    }
}

fn health(kind: &str, path: &str) -> HealthSpec {
    HealthSpec {
        kind: kind.into(),
        timeout_secs: 1,
        max_failed: 1,
        interval_secs: 1,
        path: path.into(),
        headers: BTreeMap::new(),
    }
}

fn start(rig: &Rig, proxies: Vec<ProxyCfg>) -> (mpsc::Receiver<Event>, CancellationToken) {
    let (tx, rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: rig.edge_port,
        proxies,
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    };
    tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
    (rx, cancel)
}

async fn events(rx: &mut mpsc::Receiver<Event>, n: usize) -> Vec<Event> {
    let mut out = Vec::new();
    while out.len() < n {
        let e = timeout(T, rx.recv()).await.unwrap().unwrap();
        if matches!(e, Event::ProxyStarted { .. } | Event::ProxyFailed { .. }) {
            out.push(e);
        }
    }
    out
}

fn started_port(e: &Event) -> u16 {
    match e {
        Event::ProxyStarted { remote_port, .. } => *remote_port,
        other => panic!("{other:?}"),
    }
}

/// 连上去读到 EOF，返回读到的内容（被拒绝 / 断开则为空）。
async fn fetch(port: u16) -> String {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)).await else {
        return String::new();
    };
    let mut out = String::new();
    let _ = timeout(Duration::from_secs(2), s.read_to_string(&mut out)).await;
    out
}

async fn http_get(port: u16, host: &str) -> String {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)).await else {
        return String::new();
    };
    let _ = s
        .write_all(format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
        .await;
    let mut out = String::new();
    let _ = timeout(Duration::from_secs(2), s.read_to_string(&mut out)).await;
    out
}

async fn eventually<F: std::future::Future<Output = bool>>(mut f: impl FnMut() -> F) {
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("condition not reached");
}

#[tokio::test]
async fn tcp_group_shares_one_port_and_rotates_between_clients() {
    let rig = rig().await;
    let (a, b) = (backend("A", false, 0).await, backend("B", false, 0).await);
    let (mut rx1, c1) = start(&rig, vec![proxy("one", "tcp", a, group("g", "k"))]);
    let port = started_port(&events(&mut rx1, 1).await[0]);
    let mut second = proxy("two", "tcp", b, group("g", "k"));
    second.remote_port = port;
    let (mut rx2, c2) = start(&rig, vec![second]);
    assert_eq!(started_port(&events(&mut rx2, 1).await[0]), port);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..6 {
        seen.insert(fetch(port).await);
    }
    assert_eq!(seen, ["A".to_string(), "B".to_string()].into(), "{seen:?}");
    // 一个成员离开：另一个继续服务；全部离开：端口关闭。
    c1.cancel();
    eventually(|| async { fetch(port).await == "B" }).await;
    for _ in 0..3 {
        assert_eq!(fetch(port).await, "B");
    }
    c2.cancel();
    eventually(|| async { TcpStream::connect(("127.0.0.1", port)).await.is_err() }).await;
    rig.stop.cancel();
}

#[tokio::test]
async fn group_key_port_and_udp_mismatches_are_rejected() {
    let rig = rig().await;
    let a = backend("A", false, 0).await;
    let (mut rx, c) = start(&rig, vec![proxy("one", "tcp", a, group("g", "k"))]);
    let port = started_port(&events(&mut rx, 1).await[0]);
    let mut wrong_port = proxy("p", "tcp", a, group("g", "k"));
    wrong_port.remote_port = if port == 1 { 2 } else { port - 1 };
    let (mut rx2, c2) = start(
        &rig,
        vec![
            proxy("k", "tcp", a, group("g", "other")),
            wrong_port,
            proxy("u", "udp", a, group("g", "k")),
            proxy(
                "uh",
                "udp",
                a,
                LbOpts {
                    health: Some(health("tcp", "")),
                    ..Default::default()
                },
            ),
        ],
    );
    let ev = events(&mut rx2, 4).await;
    let want = [
        "key doesn't match",
        "port not match",
        "load balancer",
        "health checks",
    ];
    for (e, w) in ev.iter().zip(want) {
        assert!(
            matches!(e, Event::ProxyFailed { error, .. } if error.contains(w)),
            "{e:?} !~ {w}"
        );
    }
    c.cancel();
    c2.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn http_and_https_groups_share_a_domain() {
    let rig = rig().await;
    let (a, b) = (backend("A", true, 200).await, backend("B", true, 200).await);
    let mk = |name: &str, port: u16, key: &str| {
        let mut p = proxy(name, "http", port, group("web", key));
        p.custom_domains = vec!["g.example.com".into()];
        p
    };
    let (mut rx, c) = start(&rig, vec![mk("a", a, "k"), mk("b", b, "k")]);
    for e in events(&mut rx, 2).await {
        assert!(matches!(e, Event::ProxyStarted { .. }), "{e:?}");
    }
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..6 {
        let r = http_get(rig.http, "g.example.com").await;
        seen.insert(r.rsplit("backend=").next().unwrap().to_string());
    }
    assert_eq!(seen, ["A".to_string(), "B".to_string()].into(), "{seen:?}");
    // 同域名但组密钥不同、或不在组里：冲突。
    let mut other = mk("c", a, "different");
    other.custom_domains = vec!["g.example.com".into()];
    let mut plain = proxy("d", "http", a, LbOpts::default());
    plain.custom_domains = vec!["g.example.com".into()];
    let (mut rx2, c2) = start(&rig, vec![other, plain]);
    let ev = events(&mut rx2, 2).await;
    assert!(
        matches!(&ev[0], Event::ProxyFailed { error, .. } if error.contains("key doesn't match"))
    );
    assert!(matches!(&ev[1], Event::ProxyFailed { error, .. } if error.contains("already used")));
    // https 组：按 SNI 透传到任一成员，端口一致。
    let tls = |name: &str, port: u16| {
        let mut p = proxy(name, "https", port, group("sec", "k"));
        p.custom_domains = vec!["s.example.com".into()];
        p
    };
    let (mut rx3, c3) = start(&rig, vec![tls("s1", a), tls("s2", b)]);
    for e in events(&mut rx3, 2).await {
        assert!(matches!(e, Event::ProxyStarted { .. }), "{e:?}");
    }
    assert!(rig.https != 0);
    c.cancel();
    c2.cancel();
    c3.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn tcp_health_check_gates_visitors_until_the_service_is_up() {
    let rig = rig().await;
    // 先占一个端口再释放：此刻本地服务不可用。
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = l.local_addr().unwrap().port();
    drop(l);
    let lb = LbOpts {
        health: Some(health("tcp", "")),
        ..Default::default()
    };
    let (mut rx, c) = start(&rig, vec![proxy("hc", "tcp", local, lb)]);
    let port = started_port(&events(&mut rx, 1).await[0]);
    // 不健康：访问者被断开，读不到数据。
    assert_eq!(fetch(port).await, "");
    // 服务起来：变健康。
    let stop = backend_on(
        TcpListener::bind(("127.0.0.1", local)).await.unwrap(),
        "UP",
        false,
        0,
    );
    eventually(|| async { fetch(port).await == "UP" }).await;
    // 服务消失：再次不健康。
    stop.cancel();
    eventually(|| async { fetch(port).await.is_empty() }).await;
    c.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn http_health_check_uses_the_status_and_removes_unhealthy_group_members() {
    let rig = rig().await;
    let good = backend("GOOD", true, 200).await;
    let bad = backend("BAD", true, 500).await;
    let mk = |name: &str, port: u16| {
        let mut p = proxy(
            name,
            "http",
            port,
            LbOpts {
                group: "g".into(),
                group_key: "k".into(),
                health: Some(health("http", "/healthz")),
            },
        );
        p.custom_domains = vec!["h.example.com".into()];
        p
    };
    let (mut rx, c) = start(&rig, vec![mk("good", good), mk("bad", bad)]);
    events(&mut rx, 2).await;
    // 只有通过检查（2xx）的成员接收请求。
    eventually(|| async {
        http_get(rig.http, "h.example.com")
            .await
            .contains("backend=GOOD")
    })
    .await;
    for _ in 0..6 {
        let r = http_get(rig.http, "h.example.com").await;
        assert!(r.contains("backend=GOOD"), "{r}");
    }
    c.cancel();
    rig.stop.cancel();
}
