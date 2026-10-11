//! Controller 全链路：真实 SQLite + 真实内置 Edge + 真实 Agent（受管模式）。
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
use rustfrp_server::app::{self, Server};
use rustfrp_server::auth::token_hash;
use rustfrp_server::config::Config;
use rustfrp_server::store::{self, NewTunnel, TunnelPatch};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

async fn start() -> (Server, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "rustfrp-hub-{}-{}.db",
        std::process::id(),
        store::new_id()
    ));
    let cfg = Config {
        db_path: path.clone(),
        http_addr: "127.0.0.1:0".into(),
        edge_addr: "127.0.0.1:0".into(),
        proxy_bind_addr: "127.0.0.1".into(),
        ..Config::default()
    };
    (app::start(cfg, Logger::silent()).await.unwrap(), path)
}

fn cleanup(path: &std::path::Path) {
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
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

async fn make_token(s: &Server, name: &str, ttl_ms: i64) -> String {
    let token = format!("rfrp_test_{name}_{}", store::new_id());
    let (th, n) = (token_hash(&token), name.to_string());
    s.db.call(move |c| store::create_enroll_token(c, &th, &n, "test", store::now_ms(), ttl_ms))
        .await
        .unwrap();
    token
}

fn agent(
    server: &Server,
    key: Arc<DeviceKey>,
    client_id: Option<&str>,
    token: Option<&str>,
) -> ClientConfig {
    ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: server.edge_addr.port(),
        managed: Some(ManagedCfg {
            key,
            client_id: client_id.map(str::to_string),
            enroll: token.map(|t| EnrollInfo {
                token: t.into(),
                name: "".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                version: "9.9".into(),
            }),
        }),
        login_fail_exit: false,
        backoff: (Duration::from_millis(30), Duration::from_millis(100)),
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    }
}

fn spawn(cfg: ClientConfig) -> (mpsc::Receiver<Event>, CancellationToken) {
    let (tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
    (rx, cancel)
}

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

async fn wait_for<F, Fut>(mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition not reached in time");
}

async fn roundtrip(port: u16, msg: &[u8]) {
    let mut s = timeout(T, TcpStream::connect(("127.0.0.1", port)))
        .await
        .unwrap()
        .unwrap();
    s.write_all(msg).await.unwrap();
    let mut got = vec![0u8; msg.len()];
    timeout(T, s.read_exact(&mut got)).await.unwrap().unwrap();
    assert_eq!(got, msg);
}

#[tokio::test]
async fn enroll_manage_tunnels_and_go_offline() {
    let (server, path) = start().await;
    let echo_port = echo().await;
    let key = Arc::new(DeviceKey::generate().unwrap());
    let token = make_token(&server, "office-pc", 60_000).await;

    // 1. 接入：设备入库、令牌被消耗、设备在线。
    let (mut rx, cancel) = spawn(agent(&server, key.clone(), None, Some(&token)));
    let Event::Enrolled { client_id } =
        until(&mut rx, |e| matches!(e, Event::Enrolled { .. })).await
    else {
        unreachable!()
    };
    until(&mut rx, |e| matches!(e, Event::ConfigApplied { .. })).await;
    let cl = server
        .db
        .call({
            let id = client_id.clone();
            move |c| store::get_client(c, &id)
        })
        .await
        .unwrap();
    assert_eq!(
        (cl.name.as_str(), cl.os.as_str(), cl.version.as_str()),
        ("office-pc", "linux", "9.9"),
        "falls back to the token's preset name"
    );
    assert!(server.hub.is_online(&client_id));
    let tokens = server
        .db
        .call(|c| store::list_enroll_tokens(c))
        .await
        .unwrap();
    assert_eq!(
        (tokens[0].used_at.is_some(), tokens[0].client_id.as_deref()),
        (true, Some(client_id.as_str()))
    );

    // 2. 建隧道 → 下发 → Edge 监听 → 状态回写为 running，流量可通。
    let tid = server
        .db
        .call({
            let id = client_id.clone();
            move |c| {
                store::create_tunnel(
                    c,
                    &NewTunnel {
                        client_id: id,
                        name: "echo".into(),
                        kind: "tcp".into(),
                        local_ip: "127.0.0.1".into(),
                        local_port: i64::from(echo_port),
                        remote_port: 0,
                        enabled: true,
                    },
                    store::now_ms(),
                )
            }
        })
        .await
        .unwrap()
        .id;
    server.hub.refresh(&client_id).await.unwrap();
    let t = tid.clone();
    wait_for(|| {
        let (db, t) = (server.db.clone(), t.clone());
        async move {
            db.call(move |c| store::get_tunnel(c, &t))
                .await
                .unwrap()
                .state
                == "running"
        }
    })
    .await;
    let running = server
        .db
        .call({
            let t = tid.clone();
            move |c| store::get_tunnel(c, &t)
        })
        .await
        .unwrap();
    assert_ne!(running.status_port, 0);
    roundtrip(running.status_port, b"through the controller").await;

    // 3. 停用隧道 → 监听关闭，状态为 stopped。
    server
        .db
        .call({
            let t = tid.clone();
            move |c| {
                store::update_tunnel(
                    c,
                    &t,
                    &TunnelPatch {
                        enabled: Some(false),
                        ..Default::default()
                    },
                    store::now_ms(),
                )
            }
        })
        .await
        .unwrap();
    server.hub.refresh(&client_id).await.unwrap();
    let t = tid.clone();
    wait_for(|| {
        let (db, t) = (server.db.clone(), t.clone());
        async move {
            db.call(move |c| store::get_tunnel(c, &t))
                .await
                .unwrap()
                .state
                == "stopped"
        }
    })
    .await;
    assert!(TcpStream::connect(("127.0.0.1", running.status_port))
        .await
        .is_err());

    // 4. 端口被占用 → error 状态带原因。
    let busy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let busy_port = i64::from(busy.local_addr().unwrap().port());
    server
        .db
        .call({
            let t = tid.clone();
            move |c| {
                store::update_tunnel(
                    c,
                    &t,
                    &TunnelPatch {
                        enabled: Some(true),
                        remote_port: Some(busy_port),
                        ..Default::default()
                    },
                    store::now_ms(),
                )
            }
        })
        .await
        .unwrap();
    server.hub.refresh(&client_id).await.unwrap();
    let t = tid.clone();
    wait_for(|| {
        let (db, t) = (server.db.clone(), t.clone());
        async move {
            db.call(move |c| store::get_tunnel(c, &t))
                .await
                .unwrap()
                .state
                == "error"
        }
    })
    .await;
    let failed = server
        .db
        .call({
            let t = tid.clone();
            move |c| store::get_tunnel(c, &t)
        })
        .await
        .unwrap();
    assert!(failed.status_error.contains("listen on port"));

    // 5. 断线 → 设备离线、隧道标为 stopped；用签名重新登录（无需令牌）。
    cancel.cancel();
    wait_for(|| {
        let h = server.hub.clone();
        let id = client_id.clone();
        async move { !h.is_online(&id) }
    })
    .await;
    let (mut rx2, cancel2) = spawn(agent(&server, key, Some(&client_id), None));
    until(&mut rx2, |e| matches!(e, Event::Connected)).await;
    wait_for(|| {
        let h = server.hub.clone();
        let id = client_id.clone();
        async move { h.is_online(&id) }
    })
    .await;
    assert_eq!(server.hub.online_ids().len(), 1);
    cancel2.cancel();
    server.stop().await;
    cleanup(&path);
}

#[tokio::test]
async fn enrollment_rejections() {
    let (server, path) = start().await;
    let key = Arc::new(DeviceKey::generate().unwrap());
    let rejected = |cfg: ClientConfig| async move {
        let mut cfg = cfg;
        cfg.login_fail_exit = true;
        let (_rx, _c) = {
            let (tx, rx) = mpsc::channel(8);
            let cancel = CancellationToken::new();
            let h = tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
            ((rx, h), cancel)
        };
        let ((_, handle), _) = (_rx, _c);
        timeout(T, handle)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .to_string()
    };

    // 未知令牌 / 已过期 / 已撤销 / 已使用。
    let e = rejected(agent(&server, key.clone(), None, Some("rfrp_unknown"))).await;
    assert!(
        e.contains("invalid, used, revoked or expired enroll token"),
        "{e}"
    );
    let expired = make_token(&server, "old", 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(rejected(agent(&server, key.clone(), None, Some(&expired)))
        .await
        .contains("expired"));
    let revoked = make_token(&server, "rev", 60_000).await;
    let th = token_hash(&revoked);
    server
        .db
        .call(move |c| {
            Ok(c.execute(
                "UPDATE enroll_tokens SET revoked_at = 1 WHERE token_hash = ?1",
                [th],
            )
            .map(|_| ())?)
        })
        .await
        .unwrap();
    assert!(rejected(agent(&server, key.clone(), None, Some(&revoked)))
        .await
        .contains("revoked"));

    let good = make_token(&server, "once", 60_000).await;
    let (mut rx, cancel) = spawn(agent(&server, key.clone(), None, Some(&good)));
    let Event::Enrolled { client_id } =
        until(&mut rx, |e| matches!(e, Event::Enrolled { .. })).await
    else {
        unreachable!()
    };
    cancel.cancel();
    wait_for(|| {
        let h = server.hub.clone();
        let id = client_id.clone();
        async move { !h.is_online(&id) }
    })
    .await;
    let other = Arc::new(DeviceKey::generate().unwrap());
    assert!(
        rejected(agent(&server, other, None, Some(&good)))
            .await
            .contains("expired"),
        "single use"
    );

    // 冒充 / 未知设备统一报 authentication failed。
    let forged = Arc::new(DeviceKey::generate().unwrap());
    assert!(rejected(agent(&server, forged, Some(&client_id), None))
        .await
        .contains("authentication failed"));
    assert!(rejected(agent(&server, key.clone(), Some("ghost"), None))
        .await
        .contains("authentication failed"));

    // 停用设备：无法再登录；删除设备：同样。
    server
        .db
        .call({
            let id = client_id.clone();
            move |c| store::update_client(c, &id, None, Some(true), 5)
        })
        .await
        .unwrap();
    assert!(
        rejected(agent(&server, key.clone(), Some(&client_id), None))
            .await
            .contains("device is disabled")
    );
    server
        .db
        .call({
            let id = client_id.clone();
            move |c| store::delete_client(c, &id, 6)
        })
        .await
        .unwrap();
    assert!(rejected(agent(&server, key, Some(&client_id), None))
        .await
        .contains("authentication failed"));
    server.stop().await;
    cleanup(&path);
}

#[tokio::test]
async fn kick_disconnects_and_the_device_comes_back() {
    let (server, path) = start().await;
    let key = Arc::new(DeviceKey::generate().unwrap());
    let token = make_token(&server, "box", 60_000).await;
    let (mut rx, cancel) = spawn(agent(&server, key, None, Some(&token)));
    let Event::Enrolled { client_id } =
        until(&mut rx, |e| matches!(e, Event::Enrolled { .. })).await
    else {
        unreachable!()
    };
    wait_for(|| {
        let h = server.hub.clone();
        let id = client_id.clone();
        async move { h.is_online(&id) }
    })
    .await;

    server.hub.kick(&client_id);
    let e = until(&mut rx, |e| {
        eprintln!("EVENT {e:?}");
        matches!(e, Event::Disconnected(_))
    })
    .await;
    eprintln!("got {e:?}");
    // 自动重连成功（被踢只是断开一次，没有被禁用）。
    until(&mut rx, |e| matches!(e, Event::Connected)).await;
    wait_for(|| {
        let h = server.hub.clone();
        let id = client_id.clone();
        async move { h.is_online(&id) }
    })
    .await;
    cancel.cancel();
    server.stop().await;
    cleanup(&path);
}

#[tokio::test]
async fn startup_errors() {
    let busy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let busy_addr = busy.local_addr().unwrap().to_string();
    let base = || Config {
        db_path: std::env::temp_dir().join(format!(
            "rustfrp-err-{}-{}.db",
            std::process::id(),
            store::new_id()
        )),
        http_addr: "127.0.0.1:0".into(),
        edge_addr: "127.0.0.1:0".into(),
        ..Config::default()
    };
    let e = app::start(
        Config {
            edge_addr: busy_addr.clone(),
            ..base()
        },
        Logger::silent(),
    )
    .await
    .err()
    .unwrap();
    assert!(e.contains("start edge"), "{e}");
    let e = app::start(
        Config {
            http_addr: busy_addr,
            ..base()
        },
        Logger::silent(),
    )
    .await
    .err()
    .unwrap();
    assert!(e.contains("listen on"), "{e}");
    let e = app::start(
        Config {
            edge_addr: "bogus".into(),
            ..base()
        },
        Logger::silent(),
    )
    .await
    .err()
    .unwrap();
    assert!(e.contains("invalid address"), "{e}");
    let e = app::start(
        Config {
            db_path: "/nonexistent-dir/x/y.db".into(),
            ..base()
        },
        Logger::silent(),
    )
    .await
    .err()
    .unwrap();
    assert!(e.contains("open database"), "{e}");
}
