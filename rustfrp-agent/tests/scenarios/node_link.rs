// 受管 Edge 的链路场景：脚本化的假 Controller + 真实的 Edge / 客户端。
//
// 同一份场景同时被集成测试（`tests/node_link.rs`）和库内单元测试（`src/node.rs`）包含，
// 这样库的两份编译实例都被覆盖到。只使用公开接口，路径统一写 `rustfrp_agent::…`。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, EnrollInfo, Event, ManagedCfg};
use rustfrp_agent::edge::EdgeConfig;
use rustfrp_agent::log::Logger;
use rustfrp_agent::node::{self, LinkCtx, LinkEvent, NodeRun, SnapshotStore};
use rustfrp_core::identity::{verify_challenge, DeviceKey};
use rustfrp_core::node::{CtlMsg, NodeDevice, NodeEnroll, NodeInfo, NodeMsg};
use rustfrp_core::tls;
use rustfrp_core::wire::{read_frame, write_frame, ConfigSet, ManagedProxy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

/// 假 Controller 对下一条连接的行为。
#[derive(Clone)]
pub enum Mode {
    /// 拒绝登录。
    Reject(String),
    /// 正常会话：下发设备清单；之后转发 `push` 里的消息，并把节点发来的消息记录下来。
    Normal(Vec<NodeDevice>),
    /// 登录成功并下发清单，收到节点的首帧对账后断开。
    DropAfterResync(Vec<NodeDevice>),
    /// 登录成功后一言不发（节点应因心跳超时而重连）。
    Silent,
    /// 首次接入：签发节点 ID。
    EnrollOk(String),
    /// TCP 接上就断开（TLS 握手失败）。
    Garbage,
    /// 第一帧不是挑战。
    NotAChallenge,
    /// 发出挑战后立刻断开。
    DropAfterChallenge,
    /// 对登录回了不对的消息。
    BadHelloReply,
    /// 读到登录后直接断开。
    DropAfterHello,
}

pub struct Fake {
    pub port: u16,
    pub seen: mpsc::UnboundedReceiver<NodeMsg>,
    pub hellos: Arc<Mutex<Vec<rustfrp_core::node::NodeHello>>>,
    pub push: mpsc::UnboundedSender<CtlMsg>,
    pub connections: Arc<std::sync::atomic::AtomicUsize>,
    modes: Arc<Mutex<Vec<Mode>>>,
}

impl Fake {
    /// 追加后续连接的行为（按顺序消费，用完后沿用最后一个）。
    pub fn then(&self, m: Mode) {
        self.modes.lock().unwrap().push(m);
    }
}

pub async fn fake_controller(first: Mode, node_pubkey: String) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(
        tls::server_config(tls::self_signed(vec!["rustfrp".into()]).unwrap()).unwrap(),
    );
    let (seen_tx, seen) = mpsc::unbounded_channel();
    let (push, push_rx) = mpsc::unbounded_channel::<CtlMsg>();
    let push_rx = Arc::new(tokio::sync::Mutex::new(push_rx));
    let hellos: Arc<Mutex<Vec<rustfrp_core::node::NodeHello>>> = Arc::default();
    let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let modes = Arc::new(Mutex::new(vec![first]));
    let (h2, c2, m2) = (hellos.clone(), connections.clone(), modes.clone());
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let n = c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mode = {
                let m = m2.lock().unwrap();
                m.get(n).or(m.last()).cloned().unwrap()
            };
            if matches!(mode, Mode::Garbage) {
                drop(tcp);
                continue;
            }
            let Ok(tls) = acceptor.accept(tcp).await else {
                continue;
            };
            let (seen_tx, push_rx, h2, pk) = (
                seen_tx.clone(),
                push_rx.clone(),
                h2.clone(),
                node_pubkey.clone(),
            );
            tokio::spawn(async move {
                let (mut rd, mut wr) = tokio::io::split(tls);
                write_frame(
                    &mut wr,
                    &CtlMsg::Challenge {
                        nonce: "nonce".into(),
                    },
                )
                .await
                .unwrap();
                let Ok(NodeMsg::Hello(hello)) = read_frame::<_, NodeMsg>(&mut rd).await else {
                    return;
                };
                h2.lock().unwrap().push(hello.clone());
                let signed = hello.enroll.is_none()
                    && verify_challenge(&pk, &hello.node_id, "nonce", &hello.signature);
                let devices = match &mode {
                    Mode::Reject(why) => {
                        let r = CtlMsg::HelloResp {
                            ok: false,
                            error: why.clone(),
                            node_id: String::new(),
                        };
                        let _ = write_frame(&mut wr, &r).await;
                        return;
                    }
                    Mode::EnrollOk(id) => {
                        let r = CtlMsg::HelloResp {
                            ok: true,
                            error: String::new(),
                            node_id: id.clone(),
                        };
                        let _ = write_frame(&mut wr, &r).await;
                        return;
                    }
                    Mode::Normal(d) | Mode::DropAfterResync(d) => d.clone(),
                    Mode::Silent => Vec::new(),
                    Mode::BadHelloReply => {
                        let _ = write_frame(&mut wr, &CtlMsg::Pong).await;
                        return;
                    }
                    Mode::DropAfterHello => return,
                    Mode::Garbage | Mode::NotAChallenge | Mode::DropAfterChallenge => return,
                };
                if !signed && hello.enroll.is_none() {
                    let r = CtlMsg::HelloResp {
                        ok: false,
                        error: "bad signature".into(),
                        node_id: String::new(),
                    };
                    let _ = write_frame(&mut wr, &r).await;
                    return;
                }
                let ok = CtlMsg::HelloResp {
                    ok: true,
                    error: String::new(),
                    node_id: hello.node_id.clone(),
                };
                if write_frame(&mut wr, &ok).await.is_err() {
                    return;
                }
                if matches!(mode, Mode::Silent) {
                    // 保持连接但不再读写。
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    return;
                }
                write_frame(&mut wr, &CtlMsg::SnapshotBegin).await.unwrap();
                for d in devices {
                    write_frame(&mut wr, &CtlMsg::Device(d)).await.unwrap();
                }
                write_frame(&mut wr, &CtlMsg::SnapshotEnd).await.unwrap();
                if matches!(mode, Mode::DropAfterResync(_)) {
                    let Ok(NodeMsg::Resync { .. }) = read_frame::<_, NodeMsg>(&mut rd).await else {
                        return;
                    };
                    return;
                }
                let mut push_rx = push_rx.lock().await;
                loop {
                    tokio::select! {
                        got = read_frame::<_, NodeMsg>(&mut rd) => match got {
                            Ok(NodeMsg::Ping) => {
                                let _ = write_frame(&mut wr, &CtlMsg::Pong).await;
                            }
                            Ok(msg) => {
                                if let NodeMsg::Traffic { seq, .. } = &msg {
                                    let _ = write_frame(&mut wr, &CtlMsg::TrafficAck { seq: *seq }).await;
                                }
                                let _ = seen_tx.send(msg);
                            }
                            Err(_) => return,
                        },
                        out = push_rx.recv() => match out {
                            Some(m) => {
                                if write_frame(&mut wr, &m).await.is_err() {
                                    return;
                                }
                            }
                            None => return,
                        },
                    }
                }
            });
        }
    });
    Fake {
        port,
        seen,
        hellos,
        push,
        connections,
        modes,
    }
}

// ───────────── 测试夹具 ─────────────

#[derive(Default)]
pub struct MemStore(pub Mutex<Option<String>>);
impl SnapshotStore for MemStore {
    fn load(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, text: &str) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(text.into());
        Ok(())
    }
}

pub async fn echo() -> u16 {
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

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn device(id: &str, key: &DeviceKey, rev: u64, proxies: Vec<ManagedProxy>) -> NodeDevice {
    NodeDevice {
        id: id.into(),
        pubkey: key.public_hex(),
        disabled: false,
        config: ConfigSet {
            revision: rev,
            proxies,
            ..Default::default()
        },
    }
}

pub fn tcp_proxy(name: &str, local: u16, remote: u16) -> ManagedProxy {
    ManagedProxy {
        name: name.into(),
        kind: "tcp".into(),
        remote_port: remote,
        local_ip: "127.0.0.1".into(),
        local_port: local,
        ..Default::default()
    }
}

pub struct Node {
    pub handle: node::NodeHandle,
    pub events: mpsc::Receiver<LinkEvent>,
    pub cancel: CancellationToken,
    pub store: Arc<MemStore>,
}

pub async fn start(
    ctl_port: u16,
    key: Arc<DeviceKey>,
    node_id: Option<&str>,
    enroll: Option<&str>,
    reject_exit: bool,
    heartbeat: (Duration, Duration),
) -> Node {
    let (events, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let store = Arc::new(MemStore::default());
    let run = NodeRun {
        edge: EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            heartbeat_timeout: Duration::from_secs(30),
            ..Default::default()
        },
        identity: None,
        dashboard_identity: None,
        tcpmux_passthrough: false,
        link: LinkCtx {
            host: "127.0.0.1".into(),
            port: ctl_port,
            key: key.clone(),
            node_id: node_id.map(str::to_string),
            enroll: enroll.map(|t| NodeEnroll {
                token: t.into(),
                name: "edge".into(),
                pubkey: key.public_hex(),
            }),
            info: NodeInfo {
                public_host: "edge.test".into(),
                ..Default::default()
            },
            connector: TlsConnector::from(tls::client_config(None).unwrap()),
            backoff: (Duration::from_millis(30), Duration::from_millis(120)),
            heartbeat,
            metrics: None,
            sample_every: Duration::from_millis(80),
            sweep_after: Duration::from_millis(100),
        },
        store: store.clone(),
        events,
        reject_exit,
    };
    let handle = node::start_node(run, Logger::silent(), cancel.clone())
        .await
        .unwrap();
    Node {
        handle,
        events: rx,
        cancel,
        store,
    }
}

pub fn client(
    edge_port: u16,
    key: Arc<DeviceKey>,
    id: Option<&str>,
    token: Option<&str>,
) -> ClientConfig {
    ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: edge_port,
        managed: Some(ManagedCfg {
            key,
            client_id: id.map(str::to_string),
            enroll: token.map(|t| EnrollInfo {
                token: t.into(),
                name: "box".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                version: "0".into(),
            }),
        }),
        login_fail_exit: false,
        backoff: (Duration::from_millis(30), Duration::from_millis(100)),
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    }
}

pub fn spawn_client(cfg: ClientConfig) -> (mpsc::Receiver<Event>, CancellationToken) {
    let (tx, rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
    (rx, cancel)
}

pub async fn until<E>(rx: &mut mpsc::Receiver<E>, mut f: impl FnMut(&E) -> bool) -> E {
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

pub async fn next_msg(fake: &mut Fake, mut f: impl FnMut(&NodeMsg) -> bool) -> NodeMsg {
    loop {
        let m = timeout(T, fake.seen.recv())
            .await
            .expect("timeout")
            .expect("closed");
        if f(&m) {
            return m;
        }
    }
}

pub async fn wait_for<F, Fut>(mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..300 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition not reached in time");
}

pub async fn roundtrip_ok(port: u16, msg: &[u8]) -> bool {
    let Ok(Ok(mut s)) = timeout(
        Duration::from_millis(300),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    else {
        return false;
    };
    if s.write_all(msg).await.is_err() {
        return false;
    }
    let mut got = vec![0u8; msg.len()];
    matches!(
        timeout(Duration::from_millis(500), s.read_exact(&mut got)).await,
        Ok(Ok(_))
    ) && got == msg
}

// ───────────── 场景 ─────────────

/// 设备经节点连上、隧道生效；节点把上线 / 代理状态 / 流量回报给 Controller，并响应推送的变更。
pub async fn reports_state_traffic_and_obeys_pushes() {
    let node_key = Arc::new(DeviceKey::generate().unwrap());
    let dev_key = Arc::new(DeviceKey::generate().unwrap());
    let backend = echo().await;
    let public = free_port();
    let dev = device("dev-1", &dev_key, 1, vec![tcp_proxy("t1", backend, public)]);
    let mut fake = fake_controller(Mode::Normal(vec![dev.clone()]), node_key.public_hex()).await;
    let mut n = start(
        fake.port,
        node_key.clone(),
        Some("node-1"),
        None,
        false,
        (Duration::from_millis(100), Duration::from_secs(5)),
    )
    .await;
    until(&mut n.events, |e| matches!(e, LinkEvent::Connected)).await;
    {
        let h = fake.hellos.lock().unwrap();
        assert_eq!(h[0].node_id, "node-1");
        assert_eq!(
            h[0].info.edge_port,
            n.handle.edge_addr.port(),
            "actual port is reported"
        );
        assert_eq!(h[0].info.tls_fingerprint.len(), 64);
    }
    // 先收下的对账帧：启动时还没有设备在线。
    let NodeMsg::Resync { online, .. } =
        next_msg(&mut fake, |m| matches!(m, NodeMsg::Resync { .. })).await
    else {
        unreachable!()
    };
    assert!(online.is_empty());

    let (mut crx, ccancel) = spawn_client(client(
        n.handle.edge_addr.port(),
        dev_key.clone(),
        Some("dev-1"),
        None,
    ));
    until(&mut crx, |e| matches!(e, Event::ConfigApplied { .. })).await;
    next_msg(
        &mut fake,
        |m| matches!(m, NodeMsg::Online { client_id, .. } if client_id == "dev-1"),
    )
    .await;
    let NodeMsg::Proxy(r) = next_msg(
        &mut fake,
        |m| matches!(m, NodeMsg::Proxy(p) if p.state == "running"),
    )
    .await
    else {
        unreachable!()
    };
    assert_eq!(
        (r.client_id.as_str(), r.name.as_str(), r.port),
        ("dev-1", "t1", public)
    );
    assert!(roundtrip_ok(public, b"hello through node").await);
    // 流量批次：累计字节出现在样本里，并被确认。
    let NodeMsg::Traffic { epoch, samples, .. } = next_msg(
        &mut fake,
        |m| matches!(m, NodeMsg::Traffic { samples, .. } if samples.iter().any(|s| s.bytes_in > 0)),
    )
    .await
    else {
        unreachable!()
    };
    assert!(!epoch.is_empty() && samples.iter().any(|s| s.name == "t1" && s.bytes_out > 0));

    // 推送变更：配置更新 → 代理被替换；Kick → 会话重连；Remove → 无法再登录。
    let moved = free_port();
    fake.push
        .send(CtlMsg::Device(device(
            "dev-1",
            &dev_key,
            2,
            vec![tcp_proxy("t1", backend, moved)],
        )))
        .unwrap();
    wait_for(|| async { roundtrip_ok(moved, b"moved").await }).await;
    fake.push
        .send(CtlMsg::Kick {
            client_id: "dev-1".into(),
        })
        .unwrap();
    next_msg(
        &mut fake,
        |m| matches!(m, NodeMsg::Offline { client_id } if client_id == "dev-1"),
    )
    .await;
    until(&mut crx, |e| matches!(e, Event::ConfigApplied { .. })).await;
    wait_for(|| async { roundtrip_ok(moved, b"back").await }).await;
    // 设备缓存已持久化。
    wait_for(|| async { n.store.load().is_some_and(|s| s.contains("dev-1")) }).await;
    fake.push
        .send(CtlMsg::Remove {
            client_id: "dev-1".into(),
        })
        .unwrap();
    wait_for(|| async { !roundtrip_ok(moved, b"gone").await }).await;
    wait_for(|| async { n.handle.registry.is_empty() }).await;
    ccancel.cancel();
    n.cancel.cancel();
    n.handle.join().await;
}

/// 链路断开重连后，节点用对账帧把「此刻在线的设备和代理状态」补报给 Controller；数据面全程不中断。
pub async fn reconnect_resyncs_and_the_data_plane_keeps_running() {
    let node_key = Arc::new(DeviceKey::generate().unwrap());
    let dev_key = Arc::new(DeviceKey::generate().unwrap());
    let backend = echo().await;
    let public = free_port();
    let dev = device("dev-1", &dev_key, 1, vec![tcp_proxy("t1", backend, public)]);
    let mut fake = fake_controller(
        Mode::DropAfterResync(vec![dev.clone()]),
        node_key.public_hex(),
    )
    .await;
    fake.then(Mode::DropAfterResync(vec![dev.clone()]));
    let mut n = start(
        fake.port,
        node_key.clone(),
        Some("node-1"),
        None,
        false,
        (Duration::from_millis(100), Duration::from_secs(5)),
    )
    .await;
    // 首条控制连接完成对账后断开；随后设备经节点本地名册登录。
    until(&mut n.events, |e| matches!(e, LinkEvent::Connected)).await;
    until(&mut n.events, |e| matches!(e, LinkEvent::Disconnected(_))).await;
    wait_for(|| async { n.handle.registry.len() == 1 }).await;
    let (mut crx, ccancel) = spawn_client(client(
        n.handle.edge_addr.port(),
        dev_key.clone(),
        Some("dev-1"),
        None,
    ));
    until(&mut crx, |e| matches!(e, Event::ConfigApplied { .. })).await;
    wait_for(|| async { roundtrip_ok(public, b"while the link flaps").await }).await;
    // 之后的连接保持：节点补报在线设备与运行中的代理。
    fake.then(Mode::Normal(vec![dev]));
    wait_for(|| async { fake.connections.load(std::sync::atomic::Ordering::SeqCst) >= 3 }).await;
    let NodeMsg::Resync {
        online,
        proxies,
        more,
    } = next_msg(
        &mut fake,
        |m| matches!(m, NodeMsg::Resync { online, .. } if online.contains(&"dev-1".to_string())),
    )
    .await
    else {
        unreachable!()
    };
    assert!(!more && online == vec!["dev-1".to_string()]);
    assert!(proxies
        .iter()
        .any(|p| p.name == "t1" && p.state == "running" && p.port == public));
    ccancel.cancel();
    n.cancel.cancel();
    n.handle.join().await;
}

/// 登录被拒：已接入的节点持续重试；首次接入则直接结束。控制端不应答时按心跳超时重连。
pub async fn rejection_and_silence_are_handled() {
    let key = Arc::new(DeviceKey::generate().unwrap());
    let mut fake = fake_controller(Mode::Reject("node is disabled".into()), key.public_hex()).await;
    let mut known = start(
        fake.port,
        key.clone(),
        Some("node-1"),
        None,
        false,
        (Duration::from_millis(50), Duration::from_millis(300)),
    )
    .await;
    let LinkEvent::Rejected(why) =
        until(&mut known.events, |e| matches!(e, LinkEvent::Rejected(_))).await
    else {
        unreachable!()
    };
    assert_eq!(why, "node is disabled");
    wait_for(|| async { fake.connections.load(std::sync::atomic::Ordering::SeqCst) >= 3 }).await;
    // 恢复后登录成功；随后控制端沉默，节点按心跳超时断开并重连。
    fake.then(Mode::Silent);
    until(&mut known.events, |e| matches!(e, LinkEvent::Connected)).await;
    until(
        &mut known.events,
        |e| matches!(e, LinkEvent::Disconnected(r) if r.contains("heartbeat")),
    )
    .await;
    known.cancel.cancel();
    let registry = known.handle.registry.clone();
    known.handle.join().await;

    // 注册表里没有设备时快照也能序列化。
    assert_eq!(registry.snapshot_json(), "[]");

    // 首次接入被拒：事件 + 退出（reject_exit），任务自行结束。
    let rejecting = fake_controller(Mode::Reject("invalid token".into()), key.public_hex()).await;
    let mut first = start(
        rejecting.port,
        key.clone(),
        None,
        Some("bad-token"),
        true,
        (Duration::from_millis(50), Duration::from_secs(1)),
    )
    .await;
    let LinkEvent::Rejected(why) =
        until(&mut first.events, |e| matches!(e, LinkEvent::Rejected(_))).await
    else {
        unreachable!()
    };
    assert_eq!(why, "invalid token");
    assert_eq!(
        rejecting
            .connections
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "no retries after a rejected enrollment"
    );
    first.cancel.cancel();
    first.handle.join().await;
    let _ = fake.seen.try_recv();
}

/// 设备经节点首次接入：节点把接入请求转给 Controller；被拒时设备拿到原因。
pub async fn device_enrollment_is_relayed() {
    let node_key = Arc::new(DeviceKey::generate().unwrap());
    let dev_key = Arc::new(DeviceKey::generate().unwrap());
    let fake = fake_controller(Mode::Normal(vec![]), node_key.public_hex()).await;
    let mut n = start(
        fake.port,
        node_key.clone(),
        Some("node-1"),
        None,
        false,
        (Duration::from_millis(100), Duration::from_secs(5)),
    )
    .await;
    let mut fake = fake;
    until(&mut n.events, |e| matches!(e, LinkEvent::Connected)).await;
    // 设备带着令牌来：节点发 Enroll，Controller 回复设备条目。
    let (mut crx, ccancel) = spawn_client(client(
        n.handle.edge_addr.port(),
        dev_key.clone(),
        None,
        Some("good"),
    ));
    let NodeMsg::Enroll { req_id, enroll } =
        next_msg(&mut fake, |m| matches!(m, NodeMsg::Enroll { .. })).await
    else {
        unreachable!()
    };
    assert_eq!((enroll.token.as_str(), enroll.pubkey.len()), ("good", 64));
    fake.push
        .send(CtlMsg::EnrollResp {
            req_id,
            ok: true,
            error: String::new(),
            device: Some(device("dev-new", &dev_key, 1, vec![])),
        })
        .unwrap();
    let Event::Enrolled { client_id } =
        until(&mut crx, |e| matches!(e, Event::Enrolled { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(client_id, "dev-new");
    ccancel.cancel();
    n.cancel.cancel();
    n.handle.join().await;
}

/// 各种协议层故障（TLS 失败、没有挑战、登录应答异常、对端中途断开）都只会让节点退避重试，最终连上。
pub async fn protocol_failures_only_cause_retries() {
    let key = Arc::new(DeviceKey::generate().unwrap());
    let fake = fake_controller(Mode::Garbage, key.public_hex()).await;
    for m in [
        Mode::NotAChallenge,
        Mode::DropAfterChallenge,
        Mode::BadHelloReply,
        Mode::DropAfterHello,
        Mode::Normal(vec![]),
    ] {
        fake.then(m);
    }
    let mut n = start(
        fake.port,
        key.clone(),
        Some("node-1"),
        None,
        false,
        (Duration::from_millis(100), Duration::from_secs(5)),
    )
    .await;
    until(&mut n.events, |e| matches!(e, LinkEvent::Connected)).await;
    assert!(fake.connections.load(std::sync::atomic::Ordering::SeqCst) >= 6);
    n.cancel.cancel();
    n.handle.join().await;
}

/// 名册持久化失败只记日志，不影响服务；退出时仍会再尝试保存。
pub async fn cache_write_failures_do_not_stop_the_node() {
    struct Failing(std::sync::atomic::AtomicUsize);
    impl SnapshotStore for Failing {
        fn load(&self) -> Option<String> {
            None
        }
        fn save(&self, _: &str) -> Result<(), String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("disk full".into())
        }
    }
    let (registry, _out, _events) = node::Registry::new();
    let store = Arc::new(Failing(std::sync::atomic::AtomicUsize::new(0)));
    let cancel = CancellationToken::new();
    let task = tokio::spawn(node::persist_loop(
        registry.clone(),
        store.clone(),
        Logger::silent(),
        cancel.clone(),
    ));
    let key = DeviceKey::generate().unwrap();
    assert!(registry.upsert(device("d", &key, 1, vec![])));
    wait_for(|| async { store.0.load(std::sync::atomic::Ordering::SeqCst) >= 1 }).await;
    cancel.cancel();
    task.await.unwrap();
    assert!(
        store.0.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "final save attempt on shutdown"
    );
}

/// 与 Controller 断开很久后，已停止代理的计数条目会被回收（不会因永远等不到确认而占满注册表）。
pub async fn stale_counters_are_swept_while_the_controller_is_away() {
    let key = Arc::new(DeviceKey::generate().unwrap());
    let dev_key = Arc::new(DeviceKey::generate().unwrap());
    let fake = fake_controller(Mode::Reject("maintenance".into()), key.public_hex()).await;
    let n = start(
        fake.port,
        key.clone(),
        Some("node-1"),
        None,
        false,
        (Duration::from_millis(100), Duration::from_secs(5)),
    )
    .await;
    let backend = echo().await;
    assert!(n.handle.registry.upsert(device(
        "dev-1",
        &dev_key,
        1,
        vec![tcp_proxy("t1", backend, free_port())]
    )));
    let (mut crx, ccancel) = spawn_client(client(
        n.handle.edge_addr.port(),
        dev_key.clone(),
        Some("dev-1"),
        None,
    ));
    until(&mut crx, |e| matches!(e, Event::ConfigApplied { .. })).await;
    wait_for(|| async { n.handle.metrics.snapshot().len() == 1 }).await;
    ccancel.cancel();
    wait_for(|| async { n.handle.metrics.snapshot().is_empty() }).await;
    n.cancel.cancel();
    n.handle.join().await;
}
