//! 节点端口：外置 Edge 经此接入 Controller（令牌首次接入、挑战签名登录、清单同步、状态回报）。

use std::sync::Arc;
use std::time::Duration;

use rustfrp_agent::log::Logger;
use rustfrp_core::auth::random_id;
use rustfrp_core::error::TunnelError;
use rustfrp_core::identity::{is_valid_public_hex, verify_challenge};
use rustfrp_core::node::{
    CtlMsg, NodeDevice, NodeEnroll, NodeHello, NodeInfo, NodeMsg, NODE_PROTOCOL_VERSION,
};
use rustfrp_core::wire::{read_frame, write_frame, Enroll};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Semaphore};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::auth::token_hash;
use crate::db::{Db, StoreError};
use crate::hub::Hub;
use crate::store;

/// 同时在线 / 握手中的节点连接上限。
const MAX_CONNECTIONS: usize = 64;
const HANDSHAKE: Duration = Duration::from_secs(10);

/// 节点端口的运行环境。
#[derive(Clone)]
pub struct NodeServer {
    /// 数据库。
    pub db: Db,
    /// Hub。
    pub hub: Arc<Hub>,
    /// 日志。
    pub log: Logger,
    /// 多久收不到节点任何消息就判定失联（节点每 20 秒一次心跳）。
    pub silence: Duration,
}

/// 默认的节点失联判定时间。
pub const DEFAULT_SILENCE: Duration = Duration::from_secs(70);

/// 接受节点连接直到 `cancel`。
pub async fn serve(
    ctx: NodeServer,
    listener: TcpListener,
    acceptor: TlsAcceptor,
    cancel: CancellationToken,
) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            accepted = listener.accept() => {
                let Ok((tcp, peer)) = accepted else { continue };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let (ctx, acceptor, cancel) = (ctx.clone(), acceptor.clone(), cancel.clone());
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = async {
                        let _ = tcp.set_nodelay(true);
                        let tls = timeout(HANDSHAKE, acceptor.accept(tcp))
                            .await
                            .map_err(|_| TunnelError::Timeout("tls handshake"))?
                            .map_err(|e| TunnelError::Tls(e.to_string()))?;
                        handle(&ctx, tls, &cancel).await
                    }
                    .await;
                    if let Err(e) = result {
                        ctx.log.debug(&format!("node {peer}: {e}"));
                    }
                });
            }
        }
    }
}

fn reject(error: &str) -> CtlMsg {
    CtlMsg::HelloResp {
        ok: false,
        error: error.into(),
        node_id: String::new(),
    }
}

/// 一个节点连接的完整生命周期。
pub async fn handle<S>(
    ctx: &NodeServer,
    stream: S,
    cancel: &CancellationToken,
) -> Result<(), TunnelError>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (mut rd, mut wr) = tokio::io::split(stream);
    let nonce = random_id();
    write_frame(
        &mut wr,
        &CtlMsg::Challenge {
            nonce: nonce.clone(),
        },
    )
    .await?;
    let first: NodeMsg = timeout(HANDSHAKE, read_frame(&mut rd))
        .await
        .map_err(|_| TunnelError::Timeout("hello"))??;
    let NodeMsg::Hello(hello) = first else {
        return Err(TunnelError::Wire("expected hello".into()));
    };
    if hello.version != NODE_PROTOCOL_VERSION {
        write_frame(&mut wr, &reject("unsupported node protocol version")).await?;
        return Err(TunnelError::Auth("protocol version".into()));
    }
    if let Some(en) = hello.enroll.clone() {
        let reply = match enroll_node(ctx, &en, &hello.info).await {
            Ok(id) => CtlMsg::HelloResp {
                ok: true,
                error: String::new(),
                node_id: id,
            },
            Err(why) => reject(&why),
        };
        write_frame(&mut wr, &reply).await?;
        return Ok(());
    }
    let node_id = match authenticate(ctx, &hello, &nonce).await {
        Ok(id) => id,
        Err(why) => {
            write_frame(&mut wr, &reject(&why)).await?;
            return Err(TunnelError::Auth(why));
        }
    };
    write_frame(
        &mut wr,
        &CtlMsg::HelloResp {
            ok: true,
            error: String::new(),
            node_id: node_id.clone(),
        },
    )
    .await?;

    // 先登记链路再取清单：登记之后产生的增量一定会被推送，清单之内已包含之前的变更。
    let (tx, mut rx) = mpsc::channel::<CtlMsg>(256);
    let token = ctx.hub.node_attach(&node_id, tx);
    ctx.log.info(&format!(
        "node [{node_id}] connected ({})",
        hello.info.public_host
    ));
    // 读帧不是取消安全的：放到独立任务里，主循环只看通道。
    let (frames_tx, frames) = mpsc::channel::<Result<NodeMsg, TunnelError>>(8);
    let reader = tokio::spawn(async move {
        loop {
            let m = read_frame::<_, NodeMsg>(&mut rd).await;
            let stop = m.is_err();
            if frames_tx.send(m).await.is_err() || stop {
                break;
            }
        }
    });
    let result = session(ctx, &node_id, &mut wr, &mut rx, frames, cancel).await;
    reader.abort();
    ctx.hub.node_detach(&node_id, token);
    let id = node_id.clone();
    let _ = ctx
        .db
        .call(move |c| store::touch_node(c, &id, store::now_ms()))
        .await;
    ctx.log.info(&format!("node [{node_id}] disconnected"));
    result
}

async fn session<W>(
    ctx: &NodeServer,
    node_id: &str,
    wr: &mut W,
    rx: &mut mpsc::Receiver<CtlMsg>,
    mut frames: mpsc::Receiver<Result<NodeMsg, TunnelError>>,
    cancel: &CancellationToken,
) -> Result<(), TunnelError>
where
    W: AsyncWrite + Send + Unpin,
{
    let id = node_id.to_string();
    let devices = ctx
        .db
        .call(move |c| store::list_node_devices(c, &id))
        .await
        .map_err(|e| TunnelError::Wire(e.to_string()))?;
    write_frame(wr, &CtlMsg::SnapshotBegin).await?;
    for d in devices {
        write_frame(wr, &CtlMsg::Device(d)).await?;
    }
    write_frame(wr, &CtlMsg::SnapshotEnd).await?;

    let mut in_resync = false;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            out = rx.recv() => match out {
                Some(msg) => write_frame(wr, &msg).await?,
                // 链路被同一节点的新连接取代。
                None => return Ok(()),
            },
            got = timeout(ctx.silence, frames.recv()) => {
                let msg = got
                    .map_err(|_| TunnelError::Timeout("node heartbeat"))?
                    .ok_or(TunnelError::Closed)??;
                on_message(ctx, node_id, msg, wr, &mut in_resync).await?;
            }
        }
    }
}

async fn on_message<W>(
    ctx: &NodeServer,
    node_id: &str,
    msg: NodeMsg,
    wr: &mut W,
    in_resync: &mut bool,
) -> Result<(), TunnelError>
where
    W: AsyncWrite + Send + Unpin,
{
    match msg {
        NodeMsg::Ping => write_frame(wr, &CtlMsg::Pong).await?,
        NodeMsg::Online { client_id } => ctx.hub.node_client_state(node_id, &client_id, true).await,
        NodeMsg::Offline { client_id } => {
            ctx.hub.node_client_state(node_id, &client_id, false).await;
        }
        NodeMsg::Proxy(report) => ctx.hub.node_proxy(node_id, report).await,
        NodeMsg::Resync {
            online,
            proxies,
            more,
        } => {
            ctx.hub
                .node_resync(node_id, !*in_resync, online, proxies)
                .await;
            *in_resync = more;
        }
        NodeMsg::Enroll { req_id, enroll } => {
            let reply = match enroll_device(ctx, node_id, &enroll).await {
                Ok(device) => CtlMsg::EnrollResp {
                    req_id,
                    ok: true,
                    error: String::new(),
                    device: Some(device),
                },
                Err(error) => CtlMsg::EnrollResp {
                    req_id,
                    ok: false,
                    error,
                    device: None,
                },
            };
            write_frame(wr, &reply).await?;
        }
        NodeMsg::Traffic {
            seq,
            epoch,
            samples,
        } => {
            if epoch.is_empty() || epoch.len() > 40 || samples.len() > 200 {
                return Err(TunnelError::Wire("invalid traffic batch".into()));
            }
            // 游标键 ≤ 64 字符：节点 ID 与纪元各取前 24 个字符（二者都是随机十六进制）。
            let short = |v: &str| v.chars().take(24).collect::<String>();
            let (node, key) = (
                node_id.to_string(),
                format!("n{}:{}", short(node_id), short(&epoch)),
            );
            let saved = ctx
                .db
                .call(move |c| store::persist_node_traffic(c, &node, &key, &samples))
                .await;
            match saved {
                Ok(()) => write_frame(wr, &CtlMsg::TrafficAck { seq }).await?,
                // 不确认：节点保留条目，下一轮重新采样（累计值，不会丢量）。
                Err(e) => ctx.log.warn(&format!("node [{node_id}] traffic: {e}")),
            }
        }
        NodeMsg::Hello(_) => return Err(TunnelError::Wire("duplicate hello".into())),
    }
    Ok(())
}

async fn authenticate(ctx: &NodeServer, hello: &NodeHello, nonce: &str) -> Result<String, String> {
    let id = hello.node_id.clone();
    let found = ctx.db.call(move |c| store::get_node_with_key(c, &id)).await;
    let (node, pubkey) = match found {
        Ok(v) => v,
        Err(StoreError::NotFound) => return Err("authentication failed".into()),
        Err(e) => return Err(e.to_string()),
    };
    if !verify_challenge(&pubkey, &hello.node_id, nonce, &hello.signature) {
        return Err("authentication failed".into());
    }
    if node.disabled_at.is_some() {
        return Err("node is disabled".into());
    }
    let (id, info) = (node.id.clone(), hello.info.clone());
    ctx.db
        .call(move |c| store::update_node_info(c, &id, &info, store::now_ms()))
        .await
        .map_err(|e| e.to_string())?;
    Ok(node.id)
}

async fn enroll_node(ctx: &NodeServer, en: &NodeEnroll, info: &NodeInfo) -> Result<String, String> {
    if !is_valid_public_hex(&en.pubkey) {
        return Err("invalid public key".into());
    }
    let (en, info, hash) = (en.clone(), info.clone(), token_hash(&en.token));
    let created = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let id = store::new_id();
            let token = store::consume_token_of(&tx, &hash, &id, now, "node")?;
            let name = if en.name.trim().is_empty() {
                token.name.clone()
            } else {
                en.name.clone()
            };
            store::create_node(&tx, &id, &name, &en.pubkey, &info, now)?;
            store::audit(
                &tx,
                "node",
                "node.enroll",
                "node",
                &id,
                &format!("enroll token {}", token.id),
                "ok",
                "",
                now,
            )?;
            tx.commit()?;
            Ok(id)
        })
        .await;
    match created {
        Ok(id) => Ok(id),
        Err(StoreError::NotFound) => Err("invalid, used, revoked or expired enroll token".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// 节点替一台设备首次接入：令牌必须是为这个节点签发的。
pub async fn enroll_device(
    ctx: &NodeServer,
    node_id: &str,
    en: &Enroll,
) -> Result<NodeDevice, String> {
    if !is_valid_public_hex(&en.pubkey) {
        return Err("invalid public key".into());
    }
    let (en, node, hash) = (en.clone(), node_id.to_string(), token_hash(&en.token));
    let created = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = store::now_ms();
            let id = store::new_id();
            let token = store::consume_enroll_token(&tx, &hash, &id, now)?;
            if token.node_id != node {
                // 令牌属于别的节点（或内置节点）：回滚，不消耗。
                return Err(StoreError::Conflict(
                    "this token was issued for another node".into(),
                ));
            }
            let name = if en.name.trim().is_empty() {
                token.name.clone()
            } else {
                en.name.clone()
            };
            store::create_client(
                &tx,
                &id,
                &name,
                &en.pubkey,
                &en.os,
                &en.arch,
                &en.version,
                now,
            )?;
            store::set_client_node(&tx, &id, &node)?;
            store::audit(
                &tx,
                "agent",
                "client.enroll",
                "client",
                &id,
                &format!("enroll token {} via node {node}", token.id),
                "ok",
                "",
                now,
            )?;
            let device = store::node_device(&tx, &id)?;
            tx.commit()?;
            Ok(device)
        })
        .await;
    match created {
        Ok(d) => Ok(d),
        Err(StoreError::NotFound) => Err("invalid, used, revoked or expired enroll token".into()),
        Err(e) => Err(e.to_string()),
    }
}
