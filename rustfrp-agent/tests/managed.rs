//! 受管模式：设备接入（Ed25519）、配置下发与热更新、状态回报、断线重连。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, EnrollInfo, Event, ManagedCfg};
use rustfrp_agent::edge::{
    Admission, BoxFut, Edge, EdgeConfig, Managed, Policy, ProxyEvent, ProxyState,
};
use rustfrp_agent::log::Logger;
use rustfrp_core::auth::unix_now;
use rustfrp_core::identity::{verify_login, DeviceKey};
use rustfrp_core::wire::{ConfigSet, Login, ManagedProxy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

struct TestPolicy {
    keys: Mutex<HashMap<String, String>>,
    cfg: watch::Sender<Arc<ConfigSet>>,
    events: mpsc::Sender<ProxyEvent>,
    online: Arc<AtomicUsize>,
}

struct Guard(Arc<AtomicUsize>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Policy for TestPolicy {
    fn login<'a>(&'a self, login: &'a Login) -> BoxFut<'a, Result<Admission, String>> {
        Box::pin(async move {
            let (id, just_enrolled) = if let Some(en) = &login.enroll {
                if en.token != "good" {
                    return Err("invalid enroll token".into());
                }
                self.keys
                    .lock()
                    .unwrap()
                    .insert("dev-1".into(), en.pubkey.clone());
                ("dev-1".to_string(), true)
            } else {
                let pubkey = self
                    .keys
                    .lock()
                    .unwrap()
                    .get(&login.client_id)
                    .cloned()
                    .ok_or("unknown device")?;
                if !verify_login(
                    &pubkey,
                    &login.client_id,
                    login.timestamp,
                    &login.auth,
                    unix_now(),
                ) {
                    return Err("bad signature".into());
                }
                (login.client_id.clone(), false)
            };
            self.online.fetch_add(1, Ordering::SeqCst);
            Ok(Admission::Managed(Managed {
                client_id: id,
                just_enrolled,
                config: self.cfg.subscribe(),
                events: self.events.clone(),
                guard: Box::new(Guard(self.online.clone())),
            }))
        })
    }
}

struct Rig {
    port: u16,
    stop: CancellationToken,
    cfg: watch::Sender<Arc<ConfigSet>>,
    events: mpsc::Receiver<ProxyEvent>,
    online: Arc<AtomicUsize>,
    http: std::net::SocketAddr,
}

async fn rig() -> Rig {
    let (cfg, _rx) = watch::channel(Arc::new(ConfigSet::default()));
    let (etx, events) = mpsc::channel(64);
    let online = Arc::new(AtomicUsize::new(0));
    let policy = Arc::new(TestPolicy {
        keys: Mutex::default(),
        cfg: cfg.clone(),
        events: etx,
        online: online.clone(),
    });
    let edge = Edge::bind_with_policy(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            vhost_http_port: Some(0),
            ..Default::default()
        },
        None,
        policy,
        Logger::silent(),
    )
    .await
    .unwrap();
    let port = edge.local_addr().unwrap().port();
    let http = edge.http_addr().unwrap();
    let stop = CancellationToken::new();
    tokio::spawn(edge.serve(stop.clone()));
    Rig {
        port,
        http,
        stop,
        cfg,
        events,
        online,
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

fn managed_client(
    port: u16,
    key: Arc<DeviceKey>,
    id: Option<&str>,
    enroll: Option<&str>,
) -> ClientConfig {
    ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: port,
        managed: Some(ManagedCfg {
            key,
            client_id: id.map(str::to_string),
            enroll: enroll.map(|t| EnrollInfo {
                token: t.into(),
                name: "test-box".into(),
                os: "test".into(),
                arch: "x".into(),
                version: "0".into(),
            }),
        }),
        login_fail_exit: false,
        backoff: (Duration::from_millis(30), Duration::from_millis(100)),
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    }
}

fn spawn_client(
    cfg: ClientConfig,
) -> (
    mpsc::Receiver<Event>,
    CancellationToken,
    tokio::task::JoinHandle<Result<(), client::ClientError>>,
) {
    let (tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let t = tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
    (rx, cancel, t)
}

async fn until<T>(rx: &mut mpsc::Receiver<T>, mut f: impl FnMut(&T) -> bool) -> T {
    loop {
        let v = timeout(T, rx.recv())
            .await
            .expect("timeout")
            .expect("closed");
        if f(&v) {
            return v;
        }
    }
}

fn mp(name: &str, remote: u16, local: u16) -> ManagedProxy {
    ManagedProxy {
        name: name.into(),
        kind: "tcp".into(),
        remote_port: remote,
        local_ip: "127.0.0.1".into(),
        local_port: local,
        ..Default::default()
    }
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
async fn enroll_push_config_hot_update_and_reconnect() {
    let mut rig = rig().await;
    let echo_a = echo().await;
    let echo_b = echo().await;
    let key = Arc::new(DeviceKey::generate().unwrap());

    // 1. 用一次性令牌接入：拿到设备 ID。
    let (mut rx, cancel, task) =
        spawn_client(managed_client(rig.port, key.clone(), None, Some("good")));
    let enrolled = until(&mut rx, |e| matches!(e, Event::Enrolled { .. })).await;
    assert_eq!(
        enrolled,
        Event::Enrolled {
            client_id: "dev-1".into()
        }
    );
    until(&mut rx, |e| matches!(e, Event::ConfigApplied { .. })).await;
    assert_eq!(rig.online.load(Ordering::SeqCst), 1);

    // 2. 下发配置：Edge 开始监听，客户端更新映射。
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 1,
        proxies: vec![mp("a", 0, echo_a)],
        ..Default::default()
    }));
    let ev = until(&mut rig.events, |e| {
        matches!(e.state, ProxyState::Running(_))
    })
    .await;
    assert_eq!((ev.client_id.as_str(), ev.name.as_str()), ("dev-1", "a"));
    let ProxyState::Running(port_a) = ev.state else {
        unreachable!()
    };
    until(&mut rx, |e| {
        matches!(
            e,
            Event::ConfigApplied {
                revision: 1,
                proxies: 1
            }
        )
    })
    .await;
    roundtrip(port_a, b"first").await;

    // 3. 热更新：只改本地目标 → 监听端口不变（不中断），转发到新目标；同时新增一个代理。
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 2,
        proxies: vec![mp("a", 0, echo_b), mp("b", 0, echo_a)],
        ..Default::default()
    }));
    let ev = until(&mut rig.events, |e| {
        e.name == "b" && matches!(e.state, ProxyState::Running(_))
    })
    .await;
    let ProxyState::Running(port_b) = ev.state else {
        unreachable!()
    };
    until(&mut rx, |e| {
        matches!(e, Event::ConfigApplied { revision: 2, .. })
    })
    .await;
    roundtrip(port_a, b"still the same port").await;
    roundtrip(port_b, b"second").await;

    // 4. 删除代理：监听被关闭，并回报 Stopped。
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 3,
        proxies: vec![mp("b", 0, echo_a)],
        ..Default::default()
    }));
    let ev = until(&mut rig.events, |e| e.state == ProxyState::Stopped).await;
    assert_eq!(ev.name, "a");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(TcpStream::connect(("127.0.0.1", port_a)).await.is_err());

    // 5. 改远程端口 → 重启监听；失败（类型不支持 / 端口被占）会回报 Failed。
    let busy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    let mut udp = mp("u", 0, echo_a);
    udp.kind = "stcp".into();
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 4,
        proxies: vec![mp("b", busy_port, echo_a), udp],
        ..Default::default()
    }));
    let mut failed = Vec::new();
    while failed.len() < 2 {
        let ev = until(&mut rig.events, |e| {
            matches!(e.state, ProxyState::Failed(_))
        })
        .await;
        failed.push(ev.name);
    }
    failed.sort();
    assert_eq!(failed, vec!["b", "u"]);

    // 6. 断线 → guard 被释放（设备标为离线）；用签名重新登录（不再需要令牌）。
    cancel.cancel();
    task.await.unwrap().unwrap();
    for _ in 0..50 {
        if rig.online.load(Ordering::SeqCst) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(rig.online.load(Ordering::SeqCst), 0);

    let (mut rx2, cancel2, _t) = spawn_client(managed_client(rig.port, key, Some("dev-1"), None));
    until(&mut rx2, |e| matches!(e, Event::Connected)).await;
    until(&mut rx2, |e| {
        matches!(e, Event::ConfigApplied { revision: 4, .. })
    })
    .await;
    cancel2.cancel();
    rig.stop.cancel();
}

#[tokio::test]
async fn bad_enroll_token_unknown_device_and_forged_signature_are_rejected() {
    let rig = rig().await;
    let key = Arc::new(DeviceKey::generate().unwrap());

    let mut cfg = managed_client(rig.port, key.clone(), None, Some("wrong"));
    cfg.login_fail_exit = true;
    let (_rx, _c, task) = spawn_client(cfg);
    let err = timeout(T, task).await.unwrap().unwrap().unwrap_err();
    assert!(err.to_string().contains("invalid enroll token"));

    // 未接入且未提供令牌：登录请求里没有任何凭据。
    let mut cfg = managed_client(rig.port, key.clone(), None, None);
    cfg.login_fail_exit = true;
    let (_rx, _c, task) = spawn_client(cfg);
    assert!(timeout(T, task).await.unwrap().unwrap().is_err());

    // 先正常接入一次，再用另一把密钥冒充同一设备 ID。
    let (mut rx, cancel, _t) = spawn_client(managed_client(rig.port, key, None, Some("good")));
    until(&mut rx, |e| matches!(e, Event::Enrolled { .. })).await;
    cancel.cancel();
    let forged = Arc::new(DeviceKey::generate().unwrap());
    let mut cfg = managed_client(rig.port, forged, Some("dev-1"), None);
    cfg.login_fail_exit = true;
    let (_rx, _c, task) = spawn_client(cfg);
    let err = timeout(T, task).await.unwrap().unwrap().unwrap_err();
    assert!(err.to_string().contains("bad signature"));

    let mut cfg = managed_client(
        rig.port,
        Arc::new(DeviceKey::generate().unwrap()),
        Some("ghost"),
        None,
    );
    cfg.login_fail_exit = true;
    let (_rx, _c, task) = spawn_client(cfg);
    let err = timeout(T, task).await.unwrap().unwrap().unwrap_err();
    assert!(err.to_string().contains("unknown device"));
    rig.stop.cancel();
}

#[tokio::test]
async fn managed_edge_rejects_clients_that_register_proxies_themselves() {
    use rustfrp_core::mux::{self, MuxConfig};
    use rustfrp_core::tls;
    use rustfrp_core::wire::{read_frame, write_frame, ClientMsg, ProxySpec, ServerMsg};
    use rustls::pki_types::ServerName;
    use tokio_rustls::TlsConnector;

    let rig = rig().await;
    let key = DeviceKey::generate().unwrap();
    let tcp = TcpStream::connect(("127.0.0.1", rig.port)).await.unwrap();
    let tls = TlsConnector::from(tls::client_config(None).unwrap())
        .connect(ServerName::try_from("127.0.0.1").unwrap(), tcp)
        .await
        .unwrap();
    let (session, _inbound, _d) = mux::spawn(tls, false, MuxConfig::default());
    let mut ctrl = session.open().await.unwrap();
    let login = Login {
        version: 1,
        user: String::new(),
        client_id: String::new(),
        timestamp: unix_now(),
        auth: String::new(),
        metas: Default::default(),
        enroll: Some(rustfrp_core::wire::Enroll {
            token: "good".into(),
            name: "n".into(),
            pubkey: key.public_hex(),
            os: String::new(),
            arch: String::new(),
            version: String::new(),
        }),
    };
    write_frame(&mut ctrl, &rustfrp_core::wire::ClientMsg::Login(login))
        .await
        .unwrap();
    assert!(matches!(
        read_frame::<_, ServerMsg>(&mut ctrl).await.unwrap(),
        ServerMsg::LoginResp {
            ok: true,
            managed: true,
            ..
        }
    ));
    assert!(matches!(
        read_frame::<_, ServerMsg>(&mut ctrl).await.unwrap(),
        ServerMsg::Config(_)
    ));
    write_frame(
        &mut ctrl,
        &ClientMsg::NewProxy(Box::new(ProxySpec {
            name: "x".into(),
            kind: "tcp".into(),
            remote_port: 0,
            ..Default::default()
        })),
    )
    .await
    .unwrap();
    let mut b = [0u8; 1];
    let n = timeout(T, ctrl.read(&mut b)).await.unwrap().unwrap_or(0);
    assert_eq!(n, 0, "edge closes the control stream");
    rig.stop.cancel();
}

#[tokio::test]
async fn disconnect_stops_running_proxies_and_names_are_unique_across_sessions() {
    let mut rig = rig().await;
    let echo_port = echo().await;
    let key = Arc::new(DeviceKey::generate().unwrap());
    let (mut rx, cancel, task) =
        spawn_client(managed_client(rig.port, key.clone(), None, Some("good")));
    until(&mut rx, |e| matches!(e, Event::ConfigApplied { .. })).await;

    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 1,
        proxies: vec![mp("same", 0, echo_port)],
        ..Default::default()
    }));
    let running = until(&mut rig.events, |e| {
        matches!(e.state, ProxyState::Running(_))
    })
    .await;
    let ProxyState::Running(port) = running.state else {
        unreachable!()
    };
    roundtrip(port, b"alive").await;

    // 同一设备的第二个会话拿到同样的配置：代理名已被占用，被拒绝并回报。
    let (mut rx2, cancel2, _t2) = spawn_client(managed_client(rig.port, key, Some("dev-1"), None));
    until(&mut rx2, |e| matches!(e, Event::ConfigApplied { .. })).await;
    let failed = until(&mut rig.events, |e| {
        matches!(e.state, ProxyState::Failed(_))
    })
    .await;
    assert!(matches!(&failed.state, ProxyState::Failed(m) if m.contains("already exists")));
    cancel2.cancel();
    // 第二个会话断开时，它没有运行的代理，不应影响第一个会话。
    tokio::time::sleep(Duration::from_millis(150)).await;
    roundtrip(port, b"still alive").await;

    // 第一个客户端断开：仍在运行的代理被停止并回报，监听端口随之关闭。
    cancel.cancel();
    task.await.unwrap().unwrap();
    let stopped = until(&mut rig.events, |e| e.state == ProxyState::Stopped).await;
    assert_eq!(stopped.name, "same");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    rig.stop.cancel();
}

#[tokio::test]
async fn managed_http_and_udp_proxies_follow_config_changes() {
    use tokio::net::UdpSocket;
    let mut rig = rig().await;
    let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = backend.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = backend.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 2048];
                let n = s.read(&mut b).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&b[..n]).to_string();
                let host = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("host:"))
                    .unwrap_or("")
                    .to_string();
                let body = format!("saw[{host}]");
                let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            });
        }
    });
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_port = udp.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut b = [0u8; 512];
        while let Ok((n, from)) = udp.recv_from(&mut b).await {
            let _ = udp.send_to(&b[..n], from).await;
        }
    });
    let key = Arc::new(DeviceKey::generate().unwrap());
    let (mut rx, cancel, _task) = spawn_client(managed_client(rig.port, key, None, Some("good")));
    until(&mut rx, |e| matches!(e, Event::ConfigApplied { .. })).await;

    let web = |domains: &[&str], rewrite: &str| ManagedProxy {
        name: "web".into(),
        kind: "http".into(),
        local_ip: "127.0.0.1".into(),
        local_port: backend_port,
        custom_domains: domains.iter().map(|d| d.to_string()).collect(),
        host_header_rewrite: rewrite.into(),
        ..Default::default()
    };
    let get = |host: &'static str| {
        let addr = rig.http;
        async move {
            let mut s = TcpStream::connect(addr).await.unwrap();
            s.write_all(format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut out = String::new();
            timeout(T, s.read_to_string(&mut out))
                .await
                .unwrap()
                .unwrap();
            out
        }
    };
    let mut dns = mp("dns", 0, udp_port);
    dns.kind = "udp".into();
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 1,
        proxies: vec![web(&["a.example.com"], ""), dns.clone()],
        ..Default::default()
    }));
    until(&mut rig.events, |e| {
        e.name == "web" && matches!(e.state, ProxyState::Running(_))
    })
    .await;
    let udp_pub = match until(&mut rig.events, |e| {
        e.name == "dns" && matches!(e.state, ProxyState::Running(_))
    })
    .await
    .state
    {
        ProxyState::Running(p) => p,
        _ => unreachable!(),
    };
    until(&mut rx, |e| {
        matches!(e, Event::ConfigApplied { revision: 1, .. })
    })
    .await;
    assert!(get("a.example.com")
        .await
        .contains("saw[Host: a.example.com]"));
    let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    c.send_to(b"hi", ("127.0.0.1", udp_pub)).await.unwrap();
    let mut buf = [0u8; 8];
    assert_eq!(timeout(T, c.recv(&mut buf)).await.unwrap().unwrap(), 2);

    // 改域名 → 旧域名失效、新域名生效；改 Host 改写 → 立即生效。
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 2,
        proxies: vec![web(&["b.example.com"], "internal.lan"), dns.clone()],
        ..Default::default()
    }));
    until(&mut rig.events, |e| {
        e.name == "web" && e.state == ProxyState::Stopped
    })
    .await;
    until(&mut rig.events, |e| {
        e.name == "web" && matches!(e.state, ProxyState::Running(_))
    })
    .await;
    assert!(get("a.example.com").await.starts_with("HTTP/1.1 404"));
    assert!(get("b.example.com")
        .await
        .contains("saw[Host: internal.lan]"));
    // 改 UDP 的远程端口 → 重启监听（不是只改映射）。
    let mut moved = dns.clone();
    moved.remote_port = 0;
    moved.local_port = udp_port;
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 3,
        proxies: vec![web(&["b.example.com"], "internal.lan"), moved],
        ..Default::default()
    }));
    until(&mut rx, |e| {
        matches!(e, Event::ConfigApplied { revision: 3, .. })
    })
    .await;
    cancel.cancel();
    rig.stop.cancel();
}

/// 连上就写 `tag` 的 tcp 服务。
async fn tagged(tag: &'static str) -> (u16, CancellationToken) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let stop = CancellationToken::new();
    let s2 = stop.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = s2.cancelled() => return,
                a = l.accept() => {
                    let Ok((mut s, _)) = a else { continue };
                    tokio::spawn(async move {
                        let _ = s.write_all(tag.as_bytes()).await;
                        let _ = s.shutdown().await;
                    });
                }
            }
        }
    });
    (port, stop)
}

async fn read_all(port: u16) -> String {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)).await else {
        return String::new();
    };
    let mut out = String::new();
    let _ = timeout(Duration::from_secs(2), s.read_to_string(&mut out)).await;
    out
}

#[tokio::test]
async fn managed_health_checks_report_state_and_groups_rotate_members() {
    use rustfrp_core::wire::{HealthSpec, LbOpts};
    let mut rig = rig().await;
    let key = Arc::new(DeviceKey::generate().unwrap());
    let (mut rx, cancel, _task) = spawn_client(managed_client(rig.port, key, None, Some("good")));
    until(&mut rx, |e| matches!(e, Event::ConfigApplied { .. })).await;
    let spec = HealthSpec {
        kind: "tcp".into(),
        timeout_secs: 1,
        max_failed: 1,
        interval_secs: 1,
        ..Default::default()
    };
    // 本地服务先不存在：代理要等第一次探测成功。
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = l.local_addr().unwrap().port();
    drop(l);
    let mut hc = mp("hc", 0, local);
    hc.lb = LbOpts {
        health: Some(spec.clone()),
        ..Default::default()
    };
    let (ga, stop_a) = tagged("A").await;
    let (gb, _stop_b) = tagged("B").await;
    let member = |name: &str, local: u16, remote: u16| {
        let mut m = mp(name, remote, local);
        m.lb = LbOpts {
            group: "g".into(),
            group_key: "k".into(),
            health: Some(spec.clone()),
        };
        m
    };
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 1,
        proxies: vec![hc.clone(), member("ga", ga, 0)],
        ..Default::default()
    }));
    let waiting = until(&mut rig.events, |e| e.name == "hc").await;
    assert!(
        matches!(&waiting.state, ProxyState::Failed(m) if m.contains("health check")),
        "{waiting:?}"
    );
    let group_port = match until(&mut rig.events, |e| {
        e.name == "ga" && matches!(e.state, ProxyState::Running(_))
    })
    .await
    .state
    {
        ProxyState::Running(p) => p,
        _ => unreachable!(),
    };
    // 服务出现 → 变健康并回报运行中。
    let svc = TcpListener::bind(("127.0.0.1", local)).await.unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = svc.accept().await {
            let _ = s.write_all(b"HC").await;
        }
    });
    let hc_port = match until(&mut rig.events, |e| {
        e.name == "hc" && matches!(e.state, ProxyState::Running(_))
    })
    .await
    .state
    {
        ProxyState::Running(p) => p,
        _ => unreachable!(),
    };
    assert_eq!(read_all(hc_port).await, "HC");
    // 第二个成员加入同一个端口：连接在两者之间轮转。
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 2,
        proxies: vec![hc, member("ga", ga, 0), member("gb", gb, group_port)],
        ..Default::default()
    }));
    until(&mut rig.events, |e| {
        e.name == "gb" && matches!(e.state, ProxyState::Running(_))
    })
    .await;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..8 {
        seen.insert(read_all(group_port).await);
    }
    assert_eq!(seen, ["A".to_string(), "B".to_string()].into(), "{seen:?}");
    // 成员 A 的服务停止：被探测为不健康并回报，流量全部转给 B。
    stop_a.cancel();
    until(&mut rig.events, |e| {
        e.name == "ga"
            && matches!(&e.state, ProxyState::Failed(m) if m.contains("health check failed"))
    })
    .await;
    for _ in 0..4 {
        assert_eq!(read_all(group_port).await, "B");
    }
    // 配置里去掉带健康检查的代理：监控任务一并停止，端口关闭。
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 3,
        proxies: vec![],
        ..Default::default()
    }));
    until(&mut rig.events, |e| {
        e.name == "hc" && e.state == ProxyState::Stopped
    })
    .await;
    until(&mut rx, |e| {
        matches!(e, Event::ConfigApplied { revision: 3, .. })
    })
    .await;
    assert!(read_all(hc_port).await.is_empty());
    cancel.cancel();
    rig.stop.cancel();
}

// ---------- 命令行入口：身份持久化与重连 ----------

mod cli_flow {
    use super::*;
    use rustfrp_agent::app::execute;
    use rustfrp_agent::cli::{Command, Mode, Options};
    use rustfrp_agent::managed::ManagedOpts;
    use rustfrp_core::frpcfg::{ConfigFs, MapEnv, MemFs};

    fn opts(port: u16, token: Option<&str>, state_dir: Option<&str>) -> Command {
        Command::Run(Options {
            local_control: None,
            mode: Mode::Client,
            config: String::new(),
            managed: Some(ManagedOpts {
                controller: format!("127.0.0.1:{port}"),
                enroll_token: token.map(str::to_string),
                name: Some("cli-box".into()),
                state_dir: state_dir.map(str::to_string),
                ca_file: None,
                tls_pin: None,
                public_host: None,
            }),
            strict: true,
            allow_unsafe: vec![],
        })
    }

    async fn run_for(cmd: Command, fs: &dyn ConfigFs, env: &MapEnv, ms: u64) -> (i32, String) {
        let cancel = CancellationToken::new();
        let stopper = {
            let c = cancel.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                c.cancel();
            }
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let go = execute(cmd, fs, env, &mut out, &mut err, cancel);
        let (code, _) = tokio::join!(go, stopper);
        (code, String::from_utf8(err).unwrap())
    }

    #[tokio::test]
    async fn enrolls_persists_identity_then_reconnects_without_a_token() {
        let rig = rig().await;
        let (fs, env) = (MemFs::new(), MapEnv::default());

        let (code, _) = run_for(opts(rig.port, Some("good"), Some("/s")), &fs, &env, 600).await;
        assert_eq!(code, 0);
        let saved = fs.read_to_string("/s/identity.json").unwrap();
        assert!(saved.contains("\"client_id\": \"dev-1\""), "{saved}");

        // 第二次运行：不带令牌，用保存的身份签名登录，设备在线。
        let probe = async {
            for _ in 0..100 {
                if rig.online.load(Ordering::SeqCst) == 1 {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            false
        };
        let (run, online) = tokio::join!(
            run_for(opts(rig.port, None, Some("/s")), &fs, &env, 1000),
            probe
        );
        assert_eq!(run.0, 0);
        assert!(online, "the saved identity logs in without an enroll token");
        rig.stop.cancel();
    }

    #[tokio::test]
    async fn a_restart_request_exits_with_the_restart_code_only_when_supervised() {
        for (supervised, want) in [(true, 75), (false, 0)] {
            let rig = rig().await;
            let (fs, env) = (
                MemFs::new(),
                if supervised {
                    MapEnv(
                        [("RUSTFRP_SUPERVISED".to_string(), "1".to_string())]
                            .into_iter()
                            .collect(),
                    )
                } else {
                    MapEnv::default()
                },
            );
            // 先接入一次，保存身份；第二次运行用签名登录。
            let (code, _) = run_for(opts(rig.port, Some("good"), Some("/s")), &fs, &env, 600).await;
            assert_eq!(code, 0);
            let cfg = rig.cfg.clone();
            let bump = async move {
                // 先让设备拿到基线，再下发更大的重启计数。
                tokio::time::sleep(Duration::from_millis(500)).await;
                cfg.send_replace(Arc::new(ConfigSet {
                    revision: 2,
                    restart_seq: 1,
                    ..Default::default()
                }));
            };
            let (run, _) = tokio::join!(
                run_for(opts(rig.port, None, Some("/s")), &fs, &env, 1500),
                bump
            );
            assert_eq!(run.0, want, "supervised={supervised}: {}", run.1);
            rig.stop.cancel();
        }
    }

    #[tokio::test]
    async fn local_control_tracks_real_connection_and_rejects_invalid_setup() {
        let rig = rig().await;
        let dir = std::env::temp_dir().join(format!(
            "rustfrp-control-{}",
            rustfrp_core::auth::random_id()
        ));
        let path = dir.join("control.json").to_string_lossy().into_owned();
        let Command::Run(mut o) = opts(rig.port, Some("good"), Some("/local")) else {
            panic!("run")
        };
        o.local_control = Some(path.clone());
        let probe = async {
            for _ in 0..100 {
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    let d: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    if let Ok(mut stream) =
                        TcpStream::connect(("127.0.0.1", d["port"].as_u64().unwrap() as u16)).await
                    {
                        let request = serde_json::json!({"token":d["token"],"command":"status"});
                        stream
                            .write_all(format!("{request}\n").as_bytes())
                            .await
                            .unwrap();
                        let mut text = String::new();
                        stream.read_to_string(&mut text).await.unwrap();
                        let status: serde_json::Value = serde_json::from_str(&text).unwrap();
                        if status["connected"] == true {
                            rig.stop.cancel();
                            for _ in 0..50 {
                                tokio::time::sleep(Duration::from_millis(5)).await;
                                let mut stream = TcpStream::connect((
                                    "127.0.0.1",
                                    d["port"].as_u64().unwrap() as u16,
                                ))
                                .await
                                .unwrap();
                                stream
                                    .write_all(format!("{request}\n").as_bytes())
                                    .await
                                    .unwrap();
                                let mut text = String::new();
                                stream.read_to_string(&mut text).await.unwrap();
                                let disconnected: serde_json::Value =
                                    serde_json::from_str(&text).unwrap();
                                if disconnected["connected"] == false {
                                    assert_eq!(disconnected["proxies"], 0);
                                    return status;
                                }
                            }
                            panic!("disconnect not reflected");
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("local agent never connected")
        };
        let (local_fs, local_env) = (MemFs::new(), MapEnv::default());
        let (run, status) = tokio::join!(
            run_for(Command::Run(o.clone()), &local_fs, &local_env, 600),
            probe
        );
        assert_eq!(run.0, 0, "{}", run.1);
        assert_eq!(status["version"], 1);
        assert_eq!(status["pid"], std::process::id());
        o.managed = None;
        let run = run_for(
            Command::Run(o.clone()),
            &MemFs::new(),
            &MapEnv::default(),
            1,
        )
        .await;
        assert!(run.0 == 1 && run.1.contains("requires managed"));
        o.managed = match opts(rig.port, Some("good"), Some("/local")) {
            Command::Run(o) => o.managed,
            _ => unreachable!(),
        };
        tokio::fs::write(&path, "bad").await.unwrap();
        let run = run_for(Command::Run(o), &MemFs::new(), &MapEnv::default(), 1).await;
        assert_eq!(run.0, 1);
        assert!(run.1.contains("local control"));
        tokio::fs::remove_dir_all(dir).await.unwrap();
        rig.stop.cancel();
    }

    struct FailSecondSave {
        fs: MemFs,
        writes: AtomicUsize,
    }
    impl ConfigFs for FailSecondSave {
        fn read_to_string(&self, p: &str) -> std::io::Result<String> {
            self.fs.read_to_string(p)
        }
        fn list_files(&self, p: &str) -> std::io::Result<Vec<String>> {
            self.fs.list_files(p)
        }
        fn dir_exists(&self, p: &str) -> bool {
            self.fs.dir_exists(p)
        }
        fn create_dir_all(&self, p: &str) -> std::io::Result<()> {
            self.fs.create_dir_all(p)
        }
        fn write_private(&self, p: &str, value: &str) -> std::io::Result<()> {
            if self.writes.fetch_add(1, Ordering::SeqCst) > 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "disk read-only",
                ));
            }
            self.fs.write_private(p, value)
        }
    }
    #[tokio::test]
    async fn failed_enrollment_persistence_is_fatal() {
        let rig = rig().await;
        let fs = FailSecondSave {
            fs: MemFs::new(),
            writes: AtomicUsize::new(0),
        };
        let result = run_for(
            opts(rig.port, Some("good"), Some("/state")),
            &fs,
            &MapEnv::default(),
            800,
        )
        .await;
        assert_eq!(result.0, 1);
        assert!(result.1.contains("managed identity") && result.1.contains("disk read-only"));
        assert_eq!(fs.writes.load(Ordering::SeqCst), 2);
        assert!(!fs
            .read_to_string("/state/identity.json")
            .unwrap()
            .contains("dev-1"));
        rig.stop.cancel();
    }

    #[tokio::test]
    async fn startup_problems_exit_with_an_error() {
        let rig = rig().await;
        let env = MapEnv::default();
        // 没有 HOME / APPDATA 也没给 --state-dir。
        let (code, e) = run_for(opts(rig.port, Some("good"), None), &MemFs::new(), &env, 100).await;
        assert!(code == 1 && e.contains("state directory"), "{e}");
        // 未接入且没有令牌。
        let (code, e) = run_for(
            opts(rig.port, None, Some("/fresh")),
            &MemFs::new(),
            &env,
            100,
        )
        .await;
        assert!(code == 1 && e.contains("--enroll-token"), "{e}");
        // 令牌无效：首次接入直接失败退出（不是无限重试）。
        let (code, e) = run_for(
            opts(rig.port, Some("bad"), Some("/s2")),
            &MemFs::new(),
            &env,
            3000,
        )
        .await;
        assert!(code == 1 && e.contains("invalid enroll token"), "{e}");
        rig.stop.cancel();
    }
}
