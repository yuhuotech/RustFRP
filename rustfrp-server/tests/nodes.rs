//! 外置节点：独立 Edge 进程经节点端口接入 Controller；Controller 掉线时数据面继续工作（AT-05）。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, EnrollInfo, Event, ManagedCfg};
use rustfrp_agent::edge::EdgeConfig;
use rustfrp_agent::log::Logger;
use rustfrp_agent::node::{self, LinkCtx, LinkEvent, NodeRun, SnapshotStore};
use rustfrp_core::identity::DeviceKey;
use rustfrp_core::node::{NodeEnroll, NodeInfo};
use rustfrp_core::tls;
use rustfrp_server::app::{self, Server};
use rustfrp_server::auth::token_hash;
use rustfrp_server::config::Config;
use rustfrp_server::store::{self, NewTunnel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

fn db_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "rustfrp-nodes-{}-{}.db",
        std::process::id(),
        store::new_id()
    ))
}

fn cleanup(path: &std::path::Path) {
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
}

fn config(path: &std::path::Path, node_addr: &str) -> Config {
    Config {
        db_path: path.to_path_buf(),
        http_addr: "127.0.0.1:0".into(),
        edge_addr: "127.0.0.1:0".into(),
        node_addr: node_addr.into(),
        proxy_bind_addr: "127.0.0.1".into(),
        ..Config::default()
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

async fn wait_for<F, Fut>(mut f: F)
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

#[derive(Default)]
struct MemStore(std::sync::Mutex<Option<String>>);
impl SnapshotStore for MemStore {
    fn load(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, text: &str) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(text.into());
        Ok(())
    }
}

fn link(
    key: Arc<DeviceKey>,
    node_port: u16,
    node_id: Option<String>,
    token: Option<&str>,
) -> LinkCtx {
    LinkCtx {
        host: "127.0.0.1".into(),
        port: node_port,
        key: key.clone(),
        node_id,
        enroll: token.map(|t| NodeEnroll {
            token: t.into(),
            name: "edge-1".into(),
            pubkey: key.public_hex(),
        }),
        info: NodeInfo {
            public_host: "edge.example.com".into(),
            version: "9.9".into(),
            os: "linux".into(),
            ..Default::default()
        },
        connector: TlsConnector::from(tls::client_config(None).unwrap()),
        backoff: (Duration::from_millis(30), Duration::from_millis(150)),
        heartbeat: (Duration::from_millis(100), Duration::from_secs(5)),
        metrics: None,
        sample_every: Duration::from_millis(100),
        sweep_after: Duration::from_secs(300),
    }
}

fn edge_cfg() -> EdgeConfig {
    EdgeConfig {
        bind_addr: "127.0.0.1".into(),
        bind_port: 0,
        proxy_bind_addr: "127.0.0.1".into(),
        heartbeat_timeout: Duration::from_secs(30),
        ..Default::default()
    }
}

async fn start_node(
    node_port: u16,
    key: Arc<DeviceKey>,
    node_id: Option<String>,
    token: Option<&str>,
    store: Arc<MemStore>,
    cancel: CancellationToken,
) -> (node::NodeHandle, mpsc::Receiver<LinkEvent>) {
    let (events, rx) = mpsc::channel(32);
    let run = NodeRun {
        edge: edge_cfg(),
        identity: None,
        dashboard_identity: None,
        tcpmux_passthrough: false,
        link: link(key, node_port, node_id, token),
        store,
        events,
        reject_exit: false,
    };
    let handle = node::start_node(run, Logger::silent(), cancel)
        .await
        .unwrap();
    (handle, rx)
}

async fn next_event(rx: &mut mpsc::Receiver<LinkEvent>, want: fn(&LinkEvent) -> bool) -> LinkEvent {
    loop {
        let e = timeout(T, rx.recv())
            .await
            .expect("timeout")
            .expect("closed");
        if want(&e) {
            return e;
        }
    }
}

fn client_cfg(
    edge_port: u16,
    key: Arc<DeviceKey>,
    client_id: Option<&str>,
    token: Option<&str>,
) -> ClientConfig {
    ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: edge_port,
        managed: Some(ManagedCfg {
            key,
            client_id: client_id.map(str::to_string),
            enroll: token.map(|t| EnrollInfo {
                token: t.into(),
                name: "box".into(),
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

fn spawn_client(cfg: ClientConfig) -> (mpsc::Receiver<Event>, CancellationToken) {
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

async fn token(server: &Server, name: &str, kind: &str, node_id: &str) -> String {
    let token = format!("rfrp_nodetest_{name}_{}", store::new_id());
    let (hash, n, k, node) = (
        token_hash(&token),
        name.to_string(),
        kind.to_string(),
        node_id.to_string(),
    );
    server
        .db
        .call(move |c| {
            store::create_token_for(c, &hash, &n, "test", store::now_ms(), 60_000, &k, &node)
        })
        .await
        .unwrap();
    token
}

#[tokio::test]
async fn data_plane_survives_the_controller_and_the_node_reconnects() {
    let path = db_path();
    let server = app::start(config(&path, "127.0.0.1:0"), Logger::silent())
        .await
        .unwrap();
    let node_port = server.node_addr.unwrap().port();
    let cancel = CancellationToken::new();
    let node_key = Arc::new(DeviceKey::generate().unwrap());
    let store_cache = Arc::new(MemStore::default());

    // 1. 节点用一次性令牌接入，随后用签名身份登录。
    let node_token = token(&server, "edge-1", "node", "").await;
    let (handle, mut events) = start_node(
        node_port,
        node_key.clone(),
        None,
        Some(&node_token),
        store_cache.clone(),
        cancel.clone(),
    )
    .await;
    let LinkEvent::Enrolled(node_id) =
        next_event(&mut events, |e| matches!(e, LinkEvent::Enrolled(_))).await
    else {
        unreachable!()
    };
    next_event(&mut events, |e| matches!(e, LinkEvent::Connected)).await;
    let nodes = server.db.call(|c| store::list_nodes(c)).await.unwrap();
    assert_eq!(
        (
            nodes.len(),
            nodes[0].id.as_str(),
            nodes[0].public_host.as_str()
        ),
        (1, node_id.as_str(), "edge.example.com")
    );
    assert_eq!(nodes[0].edge_port, handle.edge_addr.port());
    assert_eq!(nodes[0].tls_fingerprint.len(), 64);
    assert!(server.hub.node_online(&node_id));
    // 节点令牌不能当设备令牌用，反之亦然。
    assert!(handle.registry.is_empty());

    // 2. 设备经节点首次接入（令牌指定了这个节点）。
    let device_token = token(&server, "laptop", "client", &node_id).await;
    let device_key = Arc::new(DeviceKey::generate().unwrap());
    let (mut crx, ccancel) = spawn_client(client_cfg(
        handle.edge_addr.port(),
        device_key.clone(),
        None,
        Some(&device_token),
    ));
    let Event::Enrolled { client_id } =
        until(&mut crx, |e| matches!(e, Event::Enrolled { .. })).await
    else {
        unreachable!()
    };
    let cid = client_id.clone();
    let (client, on_node) = server
        .db
        .call(move |c| {
            Ok((
                store::get_client(c, &cid)?,
                store::client_node_id_any(c, &cid)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(on_node, node_id);
    assert_eq!(client.node_id, node_id);
    wait_for(|| async { server.hub.is_online(&client_id) }).await;

    // 3. 在 Controller 上建隧道 → 经节点推送 → 节点开监听 → 状态回报。
    let port = echo().await;
    // 固定公网端口：设备重连后监听会重建，自动分配的端口会变。
    let fixed = {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        probe.local_addr().unwrap().port()
    };
    let cid = client_id.clone();
    let tunnel = server
        .db
        .call(move |c| {
            store::create_tunnel(
                c,
                &NewTunnel {
                    client_id: cid,
                    name: "echo".into(),
                    kind: "tcp".into(),
                    local_ip: "127.0.0.1".into(),
                    local_port: i64::from(port),
                    remote_port: i64::from(fixed),
                    enabled: true,
                    ..Default::default()
                },
                store::now_ms(),
            )
        })
        .await
        .unwrap();
    server.hub.refresh(&client_id).await.unwrap();
    let tid = tunnel.id.clone();
    wait_for(|| {
        let (db, tid) = (server.db.clone(), tid.clone());
        async move {
            db.call(move |c| store::get_tunnel(c, &tid))
                .await
                .unwrap()
                .state
                == "running"
        }
    })
    .await;
    let tid = tunnel.id.clone();
    let public = server
        .db
        .call(move |c| store::get_tunnel(c, &tid))
        .await
        .unwrap()
        .status_port;
    assert_eq!(public, fixed);
    roundtrip(public, b"through the node").await;
    // 节点把流量计数上报给 Controller，落到同一张分钟桶表里。
    wait_for(|| async {
        let totals = server
            .db
            .call(|c| rustfrp_server::traffic::last_day_totals(c, store::now_ms()))
            .await
            .unwrap();
        totals.0 >= 16 && totals.1 >= 16
    })
    .await;

    // 4. 杀掉 Controller：隧道照常转发；设备掉线重连也能靠节点缓存登录。
    let old_node_addr = server.node_addr.unwrap();
    server.stop().await;
    roundtrip(public, b"controller is gone").await;
    ccancel.cancel();
    let (mut crx2, ccancel2) = spawn_client(client_cfg(
        handle.edge_addr.port(),
        device_key.clone(),
        Some(&client_id),
        None,
    ));
    until(&mut crx2, |e| matches!(e, Event::ConfigApplied { .. })).await;
    wait_for(|| async { roundtrip_ok(public, b"after device reconnect").await }).await;
    assert!(
        store_cache.load().is_some_and(|s| s.contains(&client_id)),
        "device cache persisted"
    );

    // 5. Controller 重新启动（同一数据库、同一节点端口）：节点自动重连并对账。
    let server = app::start(config(&path, &old_node_addr.to_string()), Logger::silent())
        .await
        .unwrap();
    wait_for(|| async { server.hub.node_online(&node_id) }).await;
    wait_for(|| async { server.hub.is_online(&client_id) }).await;
    let tid = tunnel.id.clone();
    wait_for(|| {
        let (db, tid) = (server.db.clone(), tid.clone());
        async move {
            let t = db.call(move |c| store::get_tunnel(c, &tid)).await.unwrap();
            t.state == "running" && t.status_port == public
        }
    })
    .await;
    roundtrip(public, b"after controller restart").await;

    // 6. 停用设备：节点立刻断开它，且不能再登录。
    let cid = client_id.clone();
    server
        .db
        .call(move |c| store::update_client(c, &cid, None, Some(true), store::now_ms()))
        .await
        .unwrap();
    server.hub.refresh(&client_id).await.unwrap();
    wait_for(|| async { !roundtrip_ok(public, b"x").await }).await;
    wait_for(|| async { !server.hub.is_online(&client_id) }).await;

    ccancel2.cancel();
    cancel.cancel();
    handle.join().await;
    server.stop().await;
    cleanup(&path);
}

async fn roundtrip_ok(port: u16, msg: &[u8]) -> bool {
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

#[tokio::test]
async fn controller_can_run_without_the_builtin_edge() {
    let path = db_path();
    // 既关内置节点又不开节点端口：启动即报错，而不是起一个什么也做不了的 Controller。
    let mut cfg = config(&path, "");
    cfg.builtin_edge = false;
    let err = app::start(cfg, Logger::silent()).await.err().unwrap();
    assert!(err.contains("RUSTFRP_NODE_ADDR"), "{err}");
    // 只留节点端口：管理面正常，节点可以接入。
    let mut cfg = config(&path, "127.0.0.1:0");
    cfg.builtin_edge = false;
    let server = app::start(cfg, Logger::silent()).await.unwrap();
    assert_eq!(server.edge_addr.port(), 0);
    assert!(server.vhost_http_addr.is_none());
    let node_port = server.node_addr.unwrap().port();
    let cancel = CancellationToken::new();
    let node_key = Arc::new(DeviceKey::generate().unwrap());
    let tok = token(&server, "edge", "node", "").await;
    let (handle, mut events) = start_node(
        node_port,
        node_key,
        None,
        Some(&tok),
        Arc::new(MemStore::default()),
        cancel.clone(),
    )
    .await;
    next_event(&mut events, |e| matches!(e, LinkEvent::Connected)).await;
    cancel.cancel();
    handle.join().await;
    server.stop().await;
    cleanup(&path);
}
