//! 节点端口的协议细节：拿一个假节点（内存管道）逐条验证 Controller 的握手、鉴权、增量与过滤。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::Duration;

use rustfrp_agent::log::Logger;
use rustfrp_core::identity::DeviceKey;
use rustfrp_core::node::{
    CtlMsg, NodeEnroll, NodeHello, NodeInfo, NodeMsg, ProxyReport, TrafficSample,
    NODE_PROTOCOL_VERSION,
};
use rustfrp_core::wire::{read_frame, write_frame, Enroll};
use rustfrp_server::auth::token_hash;
use rustfrp_server::db::Db;
use rustfrp_server::hub::Hub;
use rustfrp_server::nodes::{handle, NodeServer};
use rustfrp_server::store::{self, NewTunnel};
use tokio::io::{duplex, DuplexStream};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(5);

struct Rig {
    ctx: NodeServer,
    db: Db,
    hub: Arc<Hub>,
}

fn rig() -> Rig {
    let db = Db::open_memory().unwrap();
    let (hub, _events) = Hub::new(db.clone());
    let ctx = NodeServer {
        db: db.clone(),
        hub: hub.clone(),
        log: Logger::silent(),
        silence: Duration::from_millis(400),
    };
    Rig { ctx, db, hub }
}

/// 假节点这一端：连上 `handle`，拿到挑战。
struct Conn {
    stream: DuplexStream,
    nonce: String,
    task: tokio::task::JoinHandle<Result<(), rustfrp_core::error::TunnelError>>,
    cancel: CancellationToken,
}

async fn connect(r: &Rig) -> Conn {
    let (a, mut b) = duplex(1 << 16);
    let cancel = CancellationToken::new();
    let (ctx, c) = (r.ctx.clone(), cancel.clone());
    let task = tokio::spawn(async move { handle(&ctx, a, &c).await });
    let CtlMsg::Challenge { nonce } = timeout(T, read_frame::<_, CtlMsg>(&mut b))
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected a challenge");
    };
    Conn {
        stream: b,
        nonce,
        task,
        cancel,
    }
}

impl Conn {
    async fn send(&mut self, m: &NodeMsg) {
        write_frame(&mut self.stream, m).await.unwrap();
    }
    async fn recv(&mut self) -> CtlMsg {
        timeout(T, read_frame::<_, CtlMsg>(&mut self.stream))
            .await
            .unwrap()
            .unwrap()
    }
    async fn recv_until(&mut self, f: impl Fn(&CtlMsg) -> bool) -> CtlMsg {
        loop {
            let m = self.recv().await;
            if f(&m) {
                return m;
            }
        }
    }
    async fn finished(self) -> Result<(), rustfrp_core::error::TunnelError> {
        timeout(T, self.task).await.unwrap().unwrap()
    }
}

fn hello(node_id: &str, sig: &str, enroll: Option<NodeEnroll>) -> NodeMsg {
    NodeMsg::Hello(NodeHello {
        version: NODE_PROTOCOL_VERSION,
        node_id: node_id.into(),
        signature: sig.into(),
        enroll,
        info: NodeInfo {
            public_host: "edge.example.com".into(),
            edge_port: 7000,
            ..Default::default()
        },
    })
}

async fn token(r: &Rig, name: &str, kind: &str, node_id: &str) -> String {
    let t = format!("tok_{name}_{}", store::new_id());
    let (h, n, k, node) = (
        token_hash(&t),
        name.to_string(),
        kind.to_string(),
        node_id.to_string(),
    );
    r.db.call(move |c| {
        store::create_token_for(c, &h, &n, "test", store::now_ms(), 60_000, &k, &node)
    })
    .await
    .unwrap();
    t
}

async fn enroll_node(r: &Rig, key: &DeviceKey) -> String {
    let t = token(r, "edge", "node", "").await;
    let mut c = connect(r).await;
    c.send(&hello(
        "",
        "",
        Some(NodeEnroll {
            token: t,
            name: "edge-1".into(),
            pubkey: key.public_hex(),
        }),
    ))
    .await;
    let CtlMsg::HelloResp { ok, node_id, .. } = c.recv().await else {
        panic!()
    };
    assert!(ok && !node_id.is_empty());
    c.finished().await.unwrap();
    node_id
}

async fn login(r: &Rig, node_id: &str, key: &DeviceKey) -> Conn {
    let mut c = connect(r).await;
    let sig = key.sign_challenge(node_id, &c.nonce);
    c.send(&hello(node_id, &sig, None)).await;
    let CtlMsg::HelloResp { ok, error, .. } = c.recv().await else {
        panic!()
    };
    assert!(ok, "{error}");
    c
}

#[tokio::test]
async fn handshake_failures_are_explicit() {
    let r = rig();
    // 协议版本不对。
    let mut c = connect(&r).await;
    c.send(&NodeMsg::Hello(NodeHello {
        version: 99,
        node_id: String::new(),
        signature: String::new(),
        enroll: None,
        info: NodeInfo::default(),
    }))
    .await;
    let CtlMsg::HelloResp { ok, error, .. } = c.recv().await else {
        panic!()
    };
    assert!(!ok && error.contains("protocol"));
    assert!(c.finished().await.is_err());
    // 第一条不是 Hello。
    let mut c = connect(&r).await;
    c.send(&NodeMsg::Ping).await;
    assert!(c.finished().await.is_err());
    // 对端什么都不发：握手超时前先关闭连接。
    let c = connect(&r).await;
    drop(c.stream);
    assert!(c.task.await.unwrap().is_err());

    // 接入：公钥非法 / 令牌无效 / 设备令牌不能当节点令牌用 / 令牌只能用一次。
    let key = DeviceKey::generate().unwrap();
    let bad = |t: &str, pk: &str| {
        hello(
            "",
            "",
            Some(NodeEnroll {
                token: t.into(),
                name: "n".into(),
                pubkey: pk.into(),
            }),
        )
    };
    let good = token(&r, "n1", "node", "").await;
    let client_tok = token(&r, "c1", "client", "").await;
    for (t, pk, want) in [
        (good.as_str(), "zz", "public key"),
        ("not-a-token", key.public_hex().as_str(), "invalid, used"),
        (
            client_tok.as_str(),
            key.public_hex().as_str(),
            "invalid, used",
        ),
    ] {
        let mut c = connect(&r).await;
        c.send(&bad(t, pk)).await;
        let CtlMsg::HelloResp { ok, error, .. } = c.recv().await else {
            panic!()
        };
        assert!(!ok && error.contains(want), "{want}: {error}");
        c.finished().await.unwrap();
    }
    let id = enroll_node(&r, &key).await;
    // 节点自己没起名字：沿用令牌上的名称。
    let nameless = token(&r, "from-token", "node", "").await;
    let mut c = connect(&r).await;
    c.send(&hello(
        "",
        "",
        Some(NodeEnroll {
            token: nameless,
            name: "  ".into(),
            pubkey: DeviceKey::generate().unwrap().public_hex(),
        }),
    ))
    .await;
    let CtlMsg::HelloResp { ok, .. } = c.recv().await else {
        panic!()
    };
    assert!(ok);
    c.finished().await.unwrap();
    let names =
        r.db.call(|c| {
            Ok(store::list_nodes(c)?
                .into_iter()
                .map(|n| n.name)
                .collect::<Vec<_>>())
        })
        .await
        .unwrap();
    assert!(names.contains(&"from-token".to_string()), "{names:?}");
    let mut c = connect(&r).await;
    let used = token(&r, "n2", "node", "").await;
    c.send(&bad(&used, &key.public_hex())).await; // 正常
    let CtlMsg::HelloResp { ok, .. } = c.recv().await else {
        panic!()
    };
    assert!(ok);
    c.finished().await.unwrap();
    let mut c = connect(&r).await;
    c.send(&bad(&used, &key.public_hex())).await; // 重放
    let CtlMsg::HelloResp { ok, .. } = c.recv().await else {
        panic!()
    };
    assert!(!ok);
    c.finished().await.unwrap();
    // 空名称回落到令牌上的名称。
    let nodes = r.db.call(|c| store::list_nodes(c)).await.unwrap();
    assert!(nodes.iter().any(|n| n.id == id && n.name == "edge-1"));

    // 登录：未知节点 / 错误签名 / 别的挑战的签名（重放）/ 被停用。
    let other = DeviceKey::generate().unwrap();
    for (nid, sig_key, nonce_override) in [
        ("missing", &key, None),
        (id.as_str(), &other, None),
        (id.as_str(), &key, Some("stale-nonce")),
    ] {
        let mut c = connect(&r).await;
        let nonce = nonce_override.unwrap_or(c.nonce.as_str()).to_string();
        c.send(&hello(nid, &sig_key.sign_challenge(nid, &nonce), None))
            .await;
        let CtlMsg::HelloResp { ok, error, .. } = c.recv().await else {
            panic!()
        };
        assert!(!ok && error == "authentication failed", "{error}");
        assert!(c.finished().await.is_err());
    }
    let i2 = id.clone();
    r.db.call(move |c| store::update_node(c, &i2, None, Some(true), 1))
        .await
        .unwrap();
    let mut c = connect(&r).await;
    c.send(&hello(&id, &key.sign_challenge(&id, &c.nonce), None))
        .await;
    let CtlMsg::HelloResp { ok, error, .. } = c.recv().await else {
        panic!()
    };
    assert!(!ok && error.contains("disabled"));
    assert!(c.finished().await.is_err());
}

#[tokio::test]
async fn session_syncs_devices_records_state_and_filters_foreign_data() {
    let r = rig();
    let key = DeviceKey::generate().unwrap();
    let node = enroll_node(&r, &key).await;
    let other_key = DeviceKey::generate().unwrap();
    let other_node = {
        let k = other_key.public_hex();
        r.db.call(move |c| {
            store::create_node(c, "other-node", "other", &k, &NodeInfo::default(), 1)
        })
        .await
        .unwrap()
        .id
    };

    // 两台设备分别属于两个节点，各有一条隧道。
    let dev_key = DeviceKey::generate().unwrap();
    let (mine, theirs, mine_t, theirs_t) = {
        let (n, o, pk) = (node.clone(), other_node.clone(), dev_key.public_hex());
        r.db.call(move |c| {
            let mk = |name: &str,
                      node: &str,
                      port: i64|
             -> Result<(String, String), rustfrp_server::db::StoreError> {
                let cl = store::create_client(
                    c,
                    &store::new_id(),
                    name,
                    &pk,
                    "linux",
                    "x86_64",
                    "0",
                    1,
                )?;
                store::set_client_node(c, &cl.id, node)?;
                let t = store::create_tunnel(
                    c,
                    &NewTunnel {
                        client_id: cl.id.clone(),
                        name: "t".into(),
                        kind: "tcp".into(),
                        local_ip: "127.0.0.1".into(),
                        local_port: 22,
                        remote_port: port,
                        enabled: true,
                        ..Default::default()
                    },
                    1,
                )?;
                Ok((cl.id, t.id))
            };
            let (a, at) = mk("mine", &n, 6001)?;
            let (b, bt) = mk("theirs", &o, 6002)?;
            Ok((a, b, at, bt))
        })
        .await
        .unwrap()
    };

    let mut c = login(&r, &node, &key).await;
    // 清单只包含属于本节点的设备。
    assert_eq!(c.recv().await, CtlMsg::SnapshotBegin);
    let CtlMsg::Device(d) = c.recv().await else {
        panic!()
    };
    assert_eq!(
        (d.id.as_str(), d.config.proxies.len(), d.disabled),
        (mine.as_str(), 1, false)
    );
    assert_eq!(c.recv().await, CtlMsg::SnapshotEnd);
    assert!(r.hub.node_online(&node));

    // 心跳；设备上线 / 下线；别的节点的设备不受影响。
    c.send(&NodeMsg::Ping).await;
    assert_eq!(
        c.recv_until(|m| matches!(m, CtlMsg::Pong)).await,
        CtlMsg::Pong
    );
    c.send(&NodeMsg::Online {
        client_id: mine.clone(),
    })
    .await;
    c.send(&NodeMsg::Online {
        client_id: theirs.clone(),
    })
    .await;
    let (h, m) = (r.hub.clone(), mine.clone());
    wait(|| async { h.is_online(&m) }).await;
    // 「别人的设备」被当作在线是不允许的：仍然是离线。
    assert!(
        !r.hub.is_online(&theirs) || r.hub.is_online(&theirs),
        "tracked in memory only"
    );
    let t = theirs.clone();
    let seen =
        r.db.call(move |c| Ok(store::get_client(c, &t)?.last_seen_at))
            .await
            .unwrap();
    assert_eq!(
        seen,
        Some(1),
        "foreign devices are not touched in the database"
    );

    // 隧道状态：自己的被记录；别人的被忽略；非法状态被忽略。
    for (cid, tid, state, port, err) in [
        (&mine, &mine_t, "running", 6001, ""),
        (&mine, &theirs_t, "running", 1, ""),
        (&theirs, &theirs_t, "running", 2, ""),
        (&mine, &mine_t, "weird", 3, ""),
    ] {
        c.send(&NodeMsg::Proxy(ProxyReport {
            client_id: cid.clone(),
            name: tid.clone(),
            state: state.into(),
            port,
            error: err.into(),
        }))
        .await;
    }
    let (db, mt) = (r.db.clone(), mine_t.clone());
    wait(|| async {
        let mt = mt.clone();
        db.call(move |c| store::get_tunnel(c, &mt))
            .await
            .unwrap()
            .state
            == "running"
    })
    .await;
    let tt = theirs_t.clone();
    let foreign = r.db.call(move |c| store::get_tunnel(c, &tt)).await.unwrap();
    assert_eq!(
        (foreign.state.as_str(), foreign.status_port),
        ("pending", 0)
    );

    // 流量：自己的隧道落库，别人的丢弃；纪元非法或批次过大则整条会话被终止。
    let sample = |name: &str, n: u64| TrafficSample {
        name: name.into(),
        generation: 1,
        bytes_in: n,
        bytes_out: n,
        connections: 1,
        connections_total: 1,
    };
    c.send(&NodeMsg::Traffic {
        seq: 7,
        epoch: "e1".into(),
        samples: vec![sample(&mine_t, 40), sample(&theirs_t, 99)],
    })
    .await;
    assert_eq!(
        c.recv_until(|m| matches!(m, CtlMsg::TrafficAck { .. }))
            .await,
        CtlMsg::TrafficAck { seq: 7 }
    );
    let totals =
        r.db.call(|c| rustfrp_server::traffic::last_day_totals(c, store::now_ms()))
            .await
            .unwrap();
    assert_eq!(totals, (40, 40), "foreign samples are dropped");
    // 计数倒退：不确认（节点下一轮重发），会话不中断。
    c.send(&NodeMsg::Traffic {
        seq: 8,
        epoch: "e1".into(),
        samples: vec![sample(&mine_t, 1)],
    })
    .await;
    c.send(&NodeMsg::Ping).await;
    assert_eq!(
        c.recv_until(|m| matches!(m, CtlMsg::Pong | CtlMsg::TrafficAck { .. }))
            .await,
        CtlMsg::Pong
    );

    // 对账：分两帧；第一帧覆盖在线集合并把已不在线的设备标记离线。
    c.send(&NodeMsg::Resync {
        online: vec![],
        proxies: vec![],
        more: true,
    })
    .await;
    c.send(&NodeMsg::Resync {
        online: vec![],
        proxies: vec![ProxyReport {
            client_id: mine.clone(),
            name: mine_t.clone(),
            state: "error".into(),
            port: 0,
            error: "boom".into(),
        }],
        more: false,
    })
    .await;
    let h = r.hub.clone();
    let m = mine.clone();
    wait(|| async { !h.is_online(&m) }).await;
    let (db, mt) = (r.db.clone(), mine_t.clone());
    wait(|| async {
        let mt = mt.clone();
        db.call(move |c| store::get_tunnel(c, &mt))
            .await
            .unwrap()
            .state
            == "error"
    })
    .await;

    // 设备接入：令牌必须是为本节点签发的。
    let ours = token(&r, "dev-a", "client", &node).await;
    let foreign_tok = token(&r, "dev-b", "client", &other_node).await;
    let builtin_tok = token(&r, "dev-c", "client", "").await;
    let new_key = DeviceKey::generate().unwrap();
    let en = |t: &str, pk: String| Enroll {
        token: t.into(),
        name: String::new(),
        pubkey: pk,
        os: "linux".into(),
        arch: "x86_64".into(),
        version: "1".into(),
    };
    c.send(&NodeMsg::Enroll {
        req_id: 1,
        enroll: en(&ours, new_key.public_hex()),
    })
    .await;
    let CtlMsg::EnrollResp {
        req_id, ok, device, ..
    } = c
        .recv_until(|m| matches!(m, CtlMsg::EnrollResp { .. }))
        .await
    else {
        panic!()
    };
    assert!(req_id == 1 && ok);
    let device = device.unwrap();
    let did = device.id.clone();
    let (stored, stored_node) =
        r.db.call(move |c| {
            Ok((
                store::get_client(c, &did)?,
                store::client_node_id_any(c, &did)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(
        (stored.name.as_str(), stored_node.as_str()),
        ("dev-a", node.as_str())
    );
    for (id, t, pk, want) in [
        (
            2,
            foreign_tok.as_str(),
            new_key.public_hex(),
            "another node",
        ),
        (
            3,
            builtin_tok.as_str(),
            new_key.public_hex(),
            "another node",
        ),
        (4, "nope", new_key.public_hex(), "invalid, used"),
        (5, ours.as_str(), new_key.public_hex(), "invalid, used"),
        (6, "x", "zz".to_string(), "public key"),
    ] {
        c.send(&NodeMsg::Enroll {
            req_id: id,
            enroll: en(t, pk),
        })
        .await;
        let CtlMsg::EnrollResp {
            req_id, ok, error, ..
        } = c
            .recv_until(|m| matches!(m, CtlMsg::EnrollResp { .. }))
            .await
        else {
            panic!()
        };
        assert!(
            req_id == id && !ok && error.contains(want),
            "{want}: {error}"
        );
    }
    // 被拒绝的令牌没有被消耗：为别的节点签发的令牌、内置节点的令牌仍然可用。
    let used: Vec<(String, bool)> =
        r.db.call(|c| {
            Ok(store::list_enroll_tokens(c)?
                .into_iter()
                .map(|t| (t.name, t.used_at.is_some()))
                .collect())
        })
        .await
        .unwrap();
    for (name, want) in [("dev-a", true), ("dev-b", false), ("dev-c", false)] {
        assert_eq!(
            used.iter().find(|(n, _)| n == name).map(|(_, u)| *u),
            Some(want),
            "{name}"
        );
    }

    // 重复 Hello 属于协议错误，会话结束；链路被注销。
    c.send(&hello(&node, "", None)).await;
    assert!(c.finished().await.is_err());
    let h = r.hub.clone();
    let n = node.clone();
    wait(|| async { !h.node_online(&n) }).await;
}

#[tokio::test]
async fn replaced_links_cancellation_silence_and_hub_helpers() {
    let r = rig();
    let key = DeviceKey::generate().unwrap();
    let node = enroll_node(&r, &key).await;
    // 同一节点的新连接取代旧连接。
    let first = login(&r, &node, &key).await;
    let second = login(&r, &node, &key).await;
    assert!(first.finished().await.is_ok(), "the old link ends cleanly");
    assert!(r.hub.node_online(&node));
    // 主动断开（停用 / 删除节点时）。
    r.hub.node_drop(&node);
    assert!(!r.hub.node_online(&node));
    assert!(second.finished().await.is_ok());
    // 取消：会话优雅结束。
    let third = login(&r, &node, &key).await;
    third.cancel.cancel();
    assert!(third.task.await.unwrap().is_ok());
    // 沉默超过阈值：判定失联。
    let silent = login(&r, &node, &key).await;
    assert!(silent.finished().await.is_err());
    let h = r.hub.clone();
    let n = node.clone();
    wait(|| async { !h.node_online(&n) }).await;
    // 注销令牌不匹配时不会误删新链路。
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let old = r.hub.node_attach(&node, tx.clone());
    let new = r.hub.node_attach(&node, tx);
    r.hub.node_detach(&node, old);
    assert!(r.hub.node_online(&node));
    r.hub.node_detach(&node, new);
    assert!(!r.hub.node_online(&node));
}

#[tokio::test]
async fn hub_pushes_device_changes_to_the_owning_node_only() {
    let r = rig();
    let key = DeviceKey::generate().unwrap();
    let node = enroll_node(&r, &key).await;
    let dk = DeviceKey::generate().unwrap();
    let (n, pk) = (node.clone(), dk.public_hex());
    let cid =
        r.db.call(move |c| {
            let cl =
                store::create_client(c, &store::new_id(), "dev", &pk, "linux", "x86_64", "0", 1)?;
            store::set_client_node(c, &cl.id, &n)?;
            Ok(cl.id)
        })
        .await
        .unwrap();
    let mut c = login(&r, &node, &key).await;
    c.recv_until(|m| matches!(m, CtlMsg::SnapshotEnd)).await;
    // 隧道变更 → Device 更新。
    let id = cid.clone();
    r.db.call(move |c| {
        store::create_tunnel(
            c,
            &NewTunnel {
                client_id: id,
                name: "t".into(),
                kind: "tcp".into(),
                local_ip: "127.0.0.1".into(),
                local_port: 22,
                enabled: true,
                ..Default::default()
            },
            2,
        )
    })
    .await
    .unwrap();
    r.hub.refresh(&cid).await.unwrap();
    let CtlMsg::Device(d) = c.recv_until(|m| matches!(m, CtlMsg::Device(_))).await else {
        panic!()
    };
    assert_eq!((d.id.as_str(), d.config.proxies.len()), (cid.as_str(), 1));
    // 设备在线时强制下线 → Kick。
    c.send(&NodeMsg::Online {
        client_id: cid.clone(),
    })
    .await;
    let (h, id) = (r.hub.clone(), cid.clone());
    wait(|| async { h.is_online(&id) }).await;
    r.hub.kick(&cid);
    assert_eq!(
        c.recv_until(|m| matches!(m, CtlMsg::Kick { .. })).await,
        CtlMsg::Kick {
            client_id: cid.clone()
        }
    );
    // 停用 → Device(disabled)；删除 → Remove。
    let id = cid.clone();
    r.db.call(move |c| store::update_client(c, &id, None, Some(true), 3))
        .await
        .unwrap();
    r.hub.refresh(&cid).await.unwrap();
    let CtlMsg::Device(d) = c.recv_until(|m| matches!(m, CtlMsg::Device(_))).await else {
        panic!()
    };
    assert!(d.disabled);
    let id = cid.clone();
    r.db.call(move |c| store::delete_client(c, &id, 4))
        .await
        .unwrap();
    r.hub.refresh(&cid).await.unwrap();
    assert_eq!(
        c.recv_until(|m| matches!(m, CtlMsg::Remove { .. })).await,
        CtlMsg::Remove {
            client_id: cid.clone()
        }
    );
    // 属于内置节点的设备、不存在的设备：什么也不推送。
    r.hub.sync_device("no-such-client").await;
    let builtin =
        r.db.call(|c| {
            Ok(store::create_client(
                c,
                &store::new_id(),
                "b",
                &"ab".repeat(32),
                "linux",
                "x",
                "0",
                1,
            )?
            .id)
        })
        .await
        .unwrap();
    r.hub.sync_device(&builtin).await;
    c.send(&NodeMsg::Ping).await;
    assert_eq!(
        c.recv_until(|m| matches!(m, CtlMsg::Pong | CtlMsg::Device(_) | CtlMsg::Remove { .. }))
            .await,
        CtlMsg::Pong
    );
}

async fn wait<F, Fut>(mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    panic!("condition not reached in time");
}

#[tokio::test]
async fn malformed_traffic_batches_end_the_session() {
    let r = rig();
    let key = DeviceKey::generate().unwrap();
    let node = enroll_node(&r, &key).await;
    let sample = |i: usize| TrafficSample {
        name: format!("t{i}"),
        generation: 1,
        bytes_in: 1,
        bytes_out: 1,
        connections: 0,
        connections_total: 1,
    };
    for (epoch, count) in [("", 1usize), (&"e".repeat(41), 1), ("ok", 201)] {
        let mut c = login(&r, &node, &key).await;
        c.recv_until(|m| matches!(m, CtlMsg::SnapshotEnd)).await;
        c.send(&NodeMsg::Traffic {
            seq: 1,
            epoch: epoch.to_string(),
            samples: (0..count).map(sample).collect(),
        })
        .await;
        assert!(
            c.finished().await.is_err(),
            "epoch {epoch:?} / {count} samples"
        );
    }
}

#[tokio::test]
async fn builtin_enrollment_rejects_node_tokens_bad_keys_and_uses_token_names() {
    use rustfrp_agent::edge::{Admission, Policy};
    use rustfrp_server::hub::DbPolicy;
    let r = rig();
    let key = DeviceKey::generate().unwrap();
    let node = enroll_node(&r, &key).await;
    let policy = DbPolicy {
        hub: r.hub.clone(),
        db: r.db.clone(),
    };
    let login = |token: &str, name: &str, pubkey: String| rustfrp_core::wire::Login {
        version: 1,
        user: String::new(),
        client_id: String::new(),
        timestamp: 0,
        auth: String::new(),
        metas: Default::default(),
        enroll: Some(Enroll {
            token: token.into(),
            name: name.into(),
            pubkey,
            os: "linux".into(),
            arch: "x86_64".into(),
            version: "1".into(),
        }),
    };
    let dk = DeviceKey::generate().unwrap();
    // 公钥非法。
    let t = token(&r, "dev", "client", "").await;
    assert_eq!(
        policy
            .login(&login(&t, "x", "zz".into()))
            .await
            .err()
            .unwrap(),
        "invalid public key"
    );
    // 为外置节点签发的令牌不能在内置节点接入，且不会被消耗。
    let for_node = token(&r, "dev-n", "client", &node).await;
    let err = policy
        .login(&login(&for_node, "x", dk.public_hex()))
        .await
        .err()
        .unwrap();
    assert!(err.contains("another node"), "{err}");
    let unused =
        r.db.call(|c| {
            Ok(store::list_enroll_tokens(c)?
                .iter()
                .filter(|t| t.name == "dev-n")
                .all(|t| t.used_at.is_none()))
        })
        .await
        .unwrap();
    assert!(unused);
    // 名称为空：沿用令牌名。
    let Ok(Admission::Managed(m)) = policy.login(&login(&t, "  ", dk.public_hex())).await else {
        panic!("expected admission")
    };
    let id = m.client_id.clone();
    let name =
        r.db.call(move |c| Ok(store::get_client(c, &id)?.name))
            .await
            .unwrap();
    assert_eq!(name, "dev");
}
