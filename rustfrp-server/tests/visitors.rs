//! 全栈：两台受管设备经内置 Edge 私密互通——提供方的 stcp 隧道 + 使用方设备上的访问者。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, EnrollInfo, Event, ManagedCfg};
use rustfrp_agent::log::Logger;
use rustfrp_core::identity::DeviceKey;
use rustfrp_server::app;
use rustfrp_server::auth::token_hash;
use rustfrp_server::config::Config;
use rustfrp_server::store::{self, NewTunnel, NewVisitor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

async fn until(rx: &mut mpsc::Receiver<Event>, mut f: impl FnMut(&Event) -> bool) -> Event {
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

async fn enroll(
    server: &app::Server,
    name: &str,
    cancels: &mut Vec<CancellationToken>,
) -> (String, mpsc::Receiver<Event>) {
    let token = format!("rfrp_vis_{name}_{}", store::new_id());
    let (hash, n) = (token_hash(&token), name.to_string());
    server
        .db
        .call(move |c| {
            store::create_token_for(c, &hash, &n, "test", store::now_ms(), 60_000, "client", "")
        })
        .await
        .unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    cancels.push(cancel.clone());
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: server.edge_addr.port(),
        managed: Some(ManagedCfg {
            key: Arc::new(DeviceKey::generate().unwrap()),
            client_id: None,
            enroll: Some(EnrollInfo {
                token,
                name: name.into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                version: "1".into(),
            }),
        }),
        login_fail_exit: false,
        backoff: (Duration::from_millis(30), Duration::from_millis(100)),
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    };
    tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel));
    let Event::Enrolled { client_id } =
        until(&mut rx, |e| matches!(e, Event::Enrolled { .. })).await
    else {
        unreachable!()
    };
    (client_id, rx)
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

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn roundtrip(port: u16, msg: &[u8]) -> bool {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    if s.write_all(msg).await.is_err() {
        return false;
    }
    let mut got = vec![0u8; msg.len()];
    matches!(
        timeout(Duration::from_secs(2), s.read_exact(&mut got)).await,
        Ok(Ok(_))
    ) && got == msg
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
async fn managed_devices_reach_each_other_privately_through_a_visitor() {
    let path = std::env::temp_dir().join(format!(
        "rustfrp-vis-{}-{}.db",
        std::process::id(),
        store::new_id()
    ));
    let server = app::start(
        Config {
            db_path: path.clone(),
            http_addr: "127.0.0.1:0".into(),
            edge_addr: "127.0.0.1:0".into(),
            proxy_bind_addr: "127.0.0.1".into(),
            ..Config::default()
        },
        Logger::silent(),
    )
    .await
    .unwrap();
    let mut cancels = Vec::new();
    let (provider, mut prx) = enroll(&server, "provider", &mut cancels).await;
    let (consumer, mut crx) = enroll(&server, "consumer", &mut cancels).await;
    // Edge 看到的来源地址记在设备上（不取自客户端自报）。
    let p2 = provider.clone();
    let ip = server
        .db
        .call(move |c| Ok(store::get_client(c, &p2)?.last_ip))
        .await
        .unwrap();
    assert_eq!(ip, "127.0.0.1");
    let echo = echo_tcp().await;
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_port = udp.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut b = [0u8; 512];
        while let Ok((n, from)) = udp.recv_from(&mut b).await {
            let _ = udp.send_to(&b[..n], from).await;
        }
    });
    let (stcp_port, sudp_port) = (free_port(), free_port());
    let (p, c) = (provider.clone(), consumer.clone());
    let (tid, uid) = server
        .db
        .call(move |db| {
            let now = store::now_ms();
            let mk = |name: &str, kind: &str, port: i64| NewTunnel {
                client_id: p.clone(),
                name: name.into(),
                kind: kind.into(),
                local_ip: "127.0.0.1".into(),
                local_port: port,
                enabled: true,
                ..Default::default()
            };
            let t = store::create_tunnel(db, &mk("ssh", "stcp", i64::from(echo)), now)?;
            let u = store::create_tunnel(db, &mk("dns", "sudp", i64::from(udp_port)), now)?;
            for (name, tunnel, port) in [("v-ssh", &t.id, stcp_port), ("v-dns", &u.id, sudp_port)] {
                store::create_visitor(
                    db,
                    &NewVisitor {
                        client_id: c.clone(),
                        name: name.into(),
                        tunnel_id: tunnel.clone(),
                        bind_addr: "127.0.0.1".into(),
                        bind_port: i64::from(port),
                        enabled: true,
                        use_encryption: true,
                        use_compression: true,
                    },
                    now,
                )?;
            }
            Ok((t.id, u.id))
        })
        .await
        .unwrap();
    server.hub.refresh(&provider).await.unwrap();
    server.hub.refresh(&consumer).await.unwrap();
    until(&mut prx, |e| matches!(e, Event::ConfigApplied { .. })).await;
    // 访问者在使用方设备上监听；经 Edge 到达提供方的本地回显服务。
    eventually(|| async { roundtrip(stcp_port, b"private tcp").await }).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(b"private udp", ("127.0.0.1", sudp_port))
        .await
        .unwrap();
    let mut buf = [0u8; 32];
    let n = timeout(T, sock.recv(&mut buf)).await.unwrap().unwrap();
    assert_eq!(&buf[..n], b"private udp");

    // 隧道在 Controller 上的状态正常（私密隧道没有公网端口）。
    let tid2 = tid.clone();
    let state = server
        .db
        .call(move |db| store::get_tunnel(db, &tid2))
        .await
        .unwrap();
    assert_eq!((state.remote_port, state.status_port), (0, 0));

    // 停用访问者 → 本机端口关闭；停用目标隧道后也不可达。
    let vids = server
        .db
        .call(|db| store::list_visitors(db, None))
        .await
        .unwrap();
    let v0 = vids.iter().find(|v| v.name == "v-ssh").unwrap().id.clone();
    server
        .db
        .call(move |db| {
            store::update_visitor(
                db,
                &v0,
                &store::VisitorPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                store::now_ms(),
            )
        })
        .await
        .unwrap();
    server.hub.refresh(&consumer).await.unwrap();
    eventually(|| async { !roundtrip(stcp_port, b"x").await }).await;
    let uid2 = uid.clone();
    server
        .db
        .call(move |db| {
            store::update_tunnel(
                db,
                &uid2,
                &store::TunnelPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                store::now_ms(),
            )
        })
        .await
        .unwrap();
    server.hub.refresh(&provider).await.unwrap();
    server.hub.refresh(&consumer).await.unwrap();
    until(&mut crx, |e| matches!(e, Event::ConfigApplied { .. })).await;
    for c in cancels {
        c.cancel();
    }
    server.stop().await;
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
}
