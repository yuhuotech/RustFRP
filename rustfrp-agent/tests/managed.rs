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
            ..Default::default()
        },
        None,
        policy,
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
    }));
    let ev = until(&mut rig.events, |e| e.state == ProxyState::Stopped).await;
    assert_eq!(ev.name, "a");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(TcpStream::connect(("127.0.0.1", port_a)).await.is_err());

    // 5. 改远程端口 → 重启监听；失败（类型不支持 / 端口被占）会回报 Failed。
    let busy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    let mut udp = mp("u", 0, echo_a);
    udp.kind = "udp".into();
    rig.cfg.send_replace(Arc::new(ConfigSet {
        revision: 4,
        proxies: vec![mp("b", busy_port, echo_a), udp],
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
        &ClientMsg::NewProxy(ProxySpec {
            name: "x".into(),
            kind: "tcp".into(),
            remote_port: 0,
        }),
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

// ---------- 命令行入口：身份持久化与重连 ----------

mod cli_flow {
    use super::*;
    use rustfrp_agent::app::execute;
    use rustfrp_agent::cli::{Command, Mode, Options};
    use rustfrp_agent::managed::ManagedOpts;
    use rustfrp_core::frpcfg::{ConfigFs, MapEnv, MemFs};

    fn opts(port: u16, token: Option<&str>, state_dir: Option<&str>) -> Command {
        Command::Run(Options {
            mode: Mode::Client,
            config: String::new(),
            managed: Some(ManagedOpts {
                controller: format!("127.0.0.1:{port}"),
                enroll_token: token.map(str::to_string),
                name: Some("cli-box".into()),
                state_dir: state_dir.map(str::to_string),
                ca_file: None,
            }),
            strict: true,
            allow_unsafe: vec![],
        })
    }

    async fn run_for(cmd: Command, fs: &MemFs, env: &MapEnv, ms: u64) -> (i32, String) {
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
