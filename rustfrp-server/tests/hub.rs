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
use rustfrp_core::wire::HttpOpts;
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
        vhost_http: Some(0),
        subdomain_host: "t.example.com".into(),
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
                        ..Default::default()
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

#[tokio::test]
async fn udp_and_http_tunnels_run_through_the_controller() {
    use tokio::net::UdpSocket;
    let (server, path) = start().await;
    let key = Arc::new(DeviceKey::generate().unwrap());
    let token = make_token(&server, "box", 60_000).await;
    let (mut rx, cancel) = spawn(agent(&server, key, None, Some(&token)));
    let Event::Enrolled { client_id } =
        until(&mut rx, |e| matches!(e, Event::Enrolled { .. })).await
    else {
        unreachable!()
    };

    // 本地 UDP 回显与一个简单的 HTTP 后端。
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_port = udp.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut b = [0u8; 1024];
        while let Ok((n, from)) = udp.recv_from(&mut b).await {
            let _ = udp.send_to(&b[..n], from).await;
        }
    });
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_port = http.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = http.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 2048];
                let _ = s.read(&mut b).await;
                let _ = s
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            });
        }
    });
    let cid = client_id.clone();
    let mk = move |name: &str, kind: &str, port: u16, domains: Vec<String>, sub: &str| NewTunnel {
        client_id: cid.clone(),
        name: name.into(),
        kind: kind.into(),
        local_ip: "127.0.0.1".into(),
        local_port: i64::from(port),
        enabled: true,
        custom_domains: domains,
        subdomain: sub.into(),
        ..Default::default()
    };
    let (udp_t, web_t) = server
        .db
        .call(move |c| {
            let u =
                store::create_tunnel(c, &mk("dns", "udp", udp_port, vec![], ""), store::now_ms())?;
            let w = store::create_tunnel(
                c,
                &mk(
                    "site",
                    "http",
                    http_port,
                    vec!["www.example.com".into()],
                    "blog",
                ),
                store::now_ms(),
            )?;
            Ok((u, w))
        })
        .await
        .unwrap();
    server.hub.refresh(&client_id).await.unwrap();
    for id in [udp_t.id.clone(), web_t.id.clone()] {
        wait_for(|| {
            let (db, id) = (server.db.clone(), id.clone());
            async move {
                db.call(move |c| store::get_tunnel(c, &id))
                    .await
                    .unwrap()
                    .state
                    == "running"
            }
        })
        .await;
    }
    let udp_port_pub = server
        .db
        .call({
            let id = udp_t.id.clone();
            move |c| store::get_tunnel(c, &id)
        })
        .await
        .unwrap()
        .status_port;
    let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    c.send_to(b"ping", ("127.0.0.1", udp_port_pub))
        .await
        .unwrap();
    let mut buf = [0u8; 16];
    let n = timeout(T, c.recv(&mut buf)).await.unwrap().unwrap();
    assert_eq!(&buf[..n], b"ping");

    let vhost = server.vhost_http_addr.unwrap();
    for host in ["www.example.com", "blog.t.example.com"] {
        let mut s = TcpStream::connect(vhost).await.unwrap();
        s.write_all(format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = String::new();
        timeout(T, s.read_to_string(&mut out))
            .await
            .unwrap()
            .unwrap();
        assert!(out.ends_with("ok"), "{host}: {out}");
    }
    cancel.cancel();
    server.stop().await;
    cleanup(&path);
}

#[tokio::test]
async fn http_basic_auth_and_locations_are_pushed_and_hot_updated() {
    let (server, path) = start().await;
    let key = Arc::new(DeviceKey::generate().unwrap());
    let token = make_token(&server, "box", 60_000).await;
    let (mut rx, cancel) = spawn(agent(&server, key, None, Some(&token)));
    let Event::Enrolled { client_id } =
        until(&mut rx, |e| matches!(e, Event::Enrolled { .. })).await
    else {
        unreachable!()
    };
    let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = backend.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = backend.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 4096];
                let n = s.read(&mut b).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&b[..n]).to_string();
                let tag = if head.contains("X-Env: prod") {
                    "tagged"
                } else {
                    "plain"
                };
                let _ = s
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{tag}",
                            tag.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    let cid = client_id.clone();
    let tunnel = server
        .db
        .call(move |c| {
            store::create_tunnel(
                c,
                &NewTunnel {
                    client_id: cid,
                    name: "private".into(),
                    kind: "http".into(),
                    local_ip: "127.0.0.1".into(),
                    local_port: i64::from(backend_port),
                    enabled: true,
                    custom_domains: vec!["app.example.com".into()],
                    http: HttpOpts {
                        locations: vec!["/admin".into()],
                        http_user: "alice".into(),
                        http_password: "first".into(),
                        request_headers: [("X-Env".to_string(), "prod".to_string())].into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                store::now_ms(),
            )
        })
        .await
        .unwrap();
    server.hub.refresh(&client_id).await.unwrap();
    let id = tunnel.id.clone();
    wait_for(|| {
        let (db, id) = (server.db.clone(), id.clone());
        async move {
            db.call(move |c| store::get_tunnel(c, &id))
                .await
                .unwrap()
                .state
                == "running"
        }
    })
    .await;
    let vhost = server.vhost_http_addr.unwrap();
    let get = |path: &'static str, auth: Option<&'static str>| async move {
        let mut s = TcpStream::connect(vhost).await.unwrap();
        let extra = auth.map_or(String::new(), |a| format!("Authorization: Basic {a}\r\n"));
        s.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: app.example.com\r\n{extra}\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut out = String::new();
        timeout(T, s.read_to_string(&mut out))
            .await
            .unwrap()
            .unwrap();
        out
    };
    // alice:first / alice:second
    let (first, second) = ("YWxpY2U6Zmlyc3Q=", "YWxpY2U6c2Vjb25k");
    assert!(get("/admin/x", None).await.starts_with("HTTP/1.1 401"));
    assert!(get("/admin/x", Some(first)).await.ends_with("tagged"));
    assert!(
        get("/other", Some(first)).await.starts_with("HTTP/1.1 404"),
        "outside the location"
    );

    // 热更新：改密码，旧密码立即失效，设备不用重连。
    let t2 = tunnel.id.clone();
    server
        .db
        .call(move |c| {
            store::update_tunnel(
                c,
                &t2,
                &TunnelPatch {
                    http_password: Some("second".into()),
                    ..Default::default()
                },
                store::now_ms(),
            )
        })
        .await
        .unwrap();
    server.hub.refresh(&client_id).await.unwrap();
    wait_for(|| async move { get("/admin/x", Some(second)).await.ends_with("tagged") }).await;
    assert!(get("/admin/x", Some(first))
        .await
        .starts_with("HTTP/1.1 401"));
    cancel.cancel();
    server.stop().await;
    cleanup(&path);
}

#[tokio::test]
async fn controller_accepts_explicit_edge_identity_and_rejects_bad_material() {
    let dir = std::env::temp_dir().join(format!("rfrp-cert-server-{}", store::new_id()));
    std::fs::create_dir(&dir).unwrap();
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    std::fs::write(
        &cert,
        include_bytes!("../../rustfrp-protocol/fixtures/edge-test-cert.pem"),
    )
    .unwrap();
    std::fs::write(
        &key,
        include_bytes!("../../rustfrp-protocol/fixtures/edge-test-key.pem"),
    )
    .unwrap();
    let cfg = rustfrp_server::config::Config {
        db_path: dir.join("test.db"),
        http_addr: "127.0.0.1:0".into(),
        edge_addr: "127.0.0.1:0".into(),
        edge_tls_cert: Some(cert.clone()),
        edge_tls_key: Some(key.clone()),
        ..Default::default()
    };
    let server = rustfrp_server::app::start(cfg.clone(), Logger::silent())
        .await
        .unwrap();
    assert_ne!(server.edge_addr.port(), 0);
    let token = make_token(&server, "trusted", 60_000).await;
    let mut client = agent(
        &server,
        Arc::new(DeviceKey::generate().unwrap()),
        None,
        Some(&token),
    );
    client.trusted_ca_pem =
        Some(include_bytes!("../../rustfrp-protocol/fixtures/edge-test-cert.pem").to_vec());
    let (mut events, stop) = spawn(client);
    until(&mut events, |e| matches!(e, Event::ConfigApplied { .. })).await;
    assert_eq!(server.hub.online_ids().len(), 1);
    stop.cancel();
    server.stop().await;
    std::fs::write(&key, "invalid").unwrap();
    assert!(rustfrp_server::app::start(cfg, Logger::silent())
        .await
        .is_err());
    std::fs::remove_dir_all(dir).unwrap();
}
