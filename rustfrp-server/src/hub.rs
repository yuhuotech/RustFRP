//! Hub：在线设备、配置下发通道、Edge 回报的状态事件，以及对应的登录策略。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use rustfrp_agent::edge::{Admission, BoxFut, Managed, Policy, ProxyEvent, ProxyState};
use rustfrp_core::auth::unix_now;
use rustfrp_core::identity::{is_valid_public_hex, verify_login};
use rustfrp_core::node::{CtlMsg, ProxyReport};
use rustfrp_core::wire::{ConfigSet, Login};
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

/// 活跃会话与待处理下线事件共用的容量，防止重连风暴积累内存。
const MAX_SESSIONS: usize = 256;

struct Offline {
    client_id: String,
    _permit: OwnedSemaphorePermit,
}

use crate::auth::token_hash;
use crate::db::{Db, StoreError};
use crate::store;

type ConfigTx = watch::Sender<Arc<ConfigSet>>;

/// 一个外置节点的控制链路。
struct NodeLink {
    tx: mpsc::Sender<CtlMsg>,
    token: u64,
}

/// Hub 运行时状态。
pub struct Hub {
    db: Db,
    channels: Mutex<HashMap<String, ConfigTx>>,
    online: Mutex<HashMap<String, usize>>,
    node_links: Mutex<HashMap<String, NodeLink>>,
    node_clients: Mutex<HashMap<String, HashSet<String>>>,
    link_seq: std::sync::atomic::AtomicU64,
    proxy_tx: mpsc::Sender<ProxyEvent>,
    offline_tx: mpsc::Sender<Offline>,
    slots: Arc<Semaphore>,
    lifecycle: tokio::sync::Mutex<()>,
}

/// Hub 的事件接收端，交给 [`Hub::run_events`] 消费。
pub struct HubEvents {
    proxy_rx: mpsc::Receiver<ProxyEvent>,
    offline_rx: mpsc::Receiver<Offline>,
}

impl Hub {
    /// 创建 Hub 与其事件接收端。
    pub fn new(db: Db) -> (Arc<Hub>, HubEvents) {
        let (proxy_tx, proxy_rx) = mpsc::channel(256);
        let (offline_tx, offline_rx) = mpsc::channel(MAX_SESSIONS);
        let hub = Arc::new(Hub {
            db,
            channels: Mutex::default(),
            online: Mutex::default(),
            node_links: Mutex::default(),
            node_clients: Mutex::default(),
            link_seq: std::sync::atomic::AtomicU64::new(1),
            proxy_tx,
            offline_tx,
            slots: Arc::new(Semaphore::new(MAX_SESSIONS)),
            lifecycle: tokio::sync::Mutex::new(()),
        });
        (
            hub,
            HubEvents {
                proxy_rx,
                offline_rx,
            },
        )
    }

    /// 设备当前是否在线（内置节点的会话，或外置节点上报在线）。
    pub fn is_online(&self, client_id: &str) -> bool {
        self.online
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(client_id)
            .is_some_and(|n| *n > 0)
            || self
                .node_clients
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .values()
                .any(|set| set.contains(client_id))
    }

    /// 全部在线设备 ID。
    pub fn online_ids(&self) -> HashSet<String> {
        let mut ids: HashSet<String> = self
            .online
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(id, _)| id.clone())
            .collect();
        for set in self
            .node_clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
        {
            ids.extend(set.iter().cloned());
        }
        ids
    }

    // ───────────── 外置节点 ─────────────

    /// 登记节点的控制链路，返回用于注销的令牌；同一节点的旧链路被顶掉。
    pub fn node_attach(&self, node_id: &str, tx: mpsc::Sender<CtlMsg>) -> u64 {
        let token = self
            .link_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.node_links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(node_id.to_string(), NodeLink { tx, token });
        token
    }

    /// 注销链路（令牌不符说明已被新链路取代，什么也不做）。断开期间节点上的设备状态未知，视为离线。
    pub fn node_detach(&self, node_id: &str, token: u64) {
        let mut links = self
            .node_links
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if links.get(node_id).is_some_and(|l| l.token == token) {
            links.remove(node_id);
            self.node_clients
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(node_id);
        }
    }

    /// 主动断开节点链路（停用 / 删除节点时调用）；节点会重连，但登录会被拒绝。
    pub fn node_drop(&self, node_id: &str) {
        self.node_links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(node_id);
        self.node_clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(node_id);
    }

    /// 节点的控制链路是否在线。
    pub fn node_online(&self, node_id: &str) -> bool {
        self.node_links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(node_id)
    }

    fn node_send(&self, node_id: &str, msg: CtlMsg) {
        let tx = self
            .node_links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(node_id)
            .map(|l| l.tx.clone());
        if let Some(tx) = tx {
            // 队列满说明节点读得太慢：丢弃本条；节点重连后会拿到全量清单，不会永久不一致。
            let _ = tx.try_send(msg);
        }
    }

    /// 把设备的最新状态推给它所属的节点（设备不存在 / 已删除则通知节点移除）。
    pub async fn sync_device(&self, client_id: &str) {
        let id = client_id.to_string();
        let found = self
            .db
            .call(move |c| {
                let node = store::client_node_id_any(c, &id)?;
                let device = store::node_device(c, &id).ok();
                Ok((node, device))
            })
            .await;
        let Ok((node, device)) = found else { return };
        if node.is_empty() {
            return;
        }
        self.node_send(
            &node,
            match device {
                Some(d) => CtlMsg::Device(d),
                None => CtlMsg::Remove {
                    client_id: client_id.to_string(),
                },
            },
        );
    }

    /// 通知旧 Edge 清空代理并下发新 Edge 目标，然后更新新 Edge 的设备名册。
    pub async fn redirect_device(
        &self,
        client_id: &str,
        old_node_id: &str,
        target: rustfrp_core::wire::EdgeRedirect,
    ) {
        if old_node_id.is_empty() {
            let tx = self
                .channels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(client_id);
            if let Some(tx) = tx {
                let mut config = (**tx.borrow()).clone();
                config.proxies.clear();
                config.visitors.clear();
                config.redirect = Some(target.clone());
                tx.send_replace(Arc::new(config));
            }
        } else {
            self.node_send(
                old_node_id,
                CtlMsg::RedirectClient {
                    client_id: client_id.to_string(),
                    target: target.clone(),
                },
            );
        }
        self.sync_device(client_id).await;
    }

    /// 处理节点上报的设备上线 / 离线。
    pub async fn node_client_state(&self, node_id: &str, client_id: &str, online: bool, ip: &str) {
        {
            let mut map = self
                .node_clients
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let set = map.entry(node_id.to_string()).or_default();
            if online {
                set.insert(client_id.to_string());
            } else {
                set.remove(client_id);
            }
        }
        let (node, client, ip) = (node_id.to_string(), client_id.to_string(), ip.to_string());
        let _ = self
            .db
            .call(move |c| {
                store::touch_client_of_node(c, &node, &client, store::now_ms(), !online, &ip)
            })
            .await;
    }

    /// 处理节点上报的代理状态。
    pub async fn node_proxy(&self, node_id: &str, r: ProxyReport) {
        let node = node_id.to_string();
        let _ = self
            .db
            .call(move |c| {
                store::set_tunnel_status_of_node(
                    c,
                    &node,
                    &r.client_id,
                    &r.name,
                    &r.state,
                    r.port,
                    &r.error,
                )
            })
            .await;
    }

    /// 重连后的全量对账：`first` 为一轮对账的第一帧（先用它覆盖在线集合）。
    pub async fn node_resync(
        &self,
        node_id: &str,
        first: bool,
        online: Vec<String>,
        proxies: Vec<ProxyReport>,
    ) {
        if first {
            let previous = {
                let mut map = self
                    .node_clients
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let now: HashSet<String> = online.iter().cloned().collect();
                map.insert(node_id.to_string(), now).unwrap_or_default()
            };
            let now: HashSet<&String> = online.iter().collect();
            for gone in previous.iter().filter(|c| !now.contains(c)) {
                self.node_client_state(node_id, gone, false, "").await;
            }
            for id in &online {
                self.node_client_state(node_id, id, true, "").await;
            }
        }
        for r in proxies {
            self.node_proxy(node_id, r).await;
        }
    }

    fn mark_online(&self, client_id: &str) {
        *self
            .online
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(client_id.to_string())
            .or_insert(0) += 1;
    }

    /// 取（必要时创建）设备的配置通道。
    async fn subscribe(
        &self,
        client_id: &str,
    ) -> Result<watch::Receiver<Arc<ConfigSet>>, StoreError> {
        if let Some(tx) = self
            .channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(client_id)
        {
            return Ok(tx.subscribe());
        }
        let id = client_id.to_string();
        let cfg = self.db.call(move |c| store::managed_config(c, &id)).await?;
        let mut map = self.channels.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = map
            .entry(client_id.to_string())
            .or_insert_with(|| watch::channel(Arc::new(cfg)).0);
        Ok(tx.subscribe())
    }

    /// 重新计算并下发设备配置（隧道变更后调用）。设备不在线时无需下发。
    pub async fn refresh(&self, client_id: &str) -> Result<(), StoreError> {
        self.sync_device(client_id).await;
        let exists = self
            .channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(client_id);
        if !exists {
            return Ok(());
        }
        let id = client_id.to_string();
        let cfg = self.db.call(move |c| store::managed_config(c, &id)).await?;
        if let Some(tx) = self
            .channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(client_id)
        {
            tx.send_replace(Arc::new(cfg));
        }
        Ok(())
    }

    /// 强制断开设备：丢弃配置通道，Edge 侧会话随之结束。
    pub fn kick(&self, client_id: &str) {
        self.channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(client_id);
        let nodes: Vec<String> = self
            .node_clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, set)| set.contains(client_id))
            .map(|(n, _)| n.clone())
            .collect();
        for node in nodes {
            self.node_send(
                &node,
                CtlMsg::Kick {
                    client_id: client_id.to_string(),
                },
            );
        }
    }

    /// 消费 Edge 回报的事件并落库，直到所有发送端关闭。
    pub async fn run_events(self: Arc<Self>, mut ev: HubEvents) {
        loop {
            tokio::select! {
                p = ev.proxy_rx.recv() => match p {
                    Some(p) => self.on_proxy_event(p).await,
                    None => break,
                },
                o = ev.offline_rx.recv() => match o {
                    Some(event) => self.on_offline(event.client_id.clone()).await,
                    None => break,
                },
            }
        }
    }

    async fn on_proxy_event(&self, e: ProxyEvent) {
        let (state, port, err) = match e.state {
            ProxyState::Running(p) => ("running", p, String::new()),
            ProxyState::Failed(m) => ("error", 0, m),
            ProxyState::Stopped => ("stopped", 0, String::new()),
        };
        let name = e.name;
        let _ = self
            .db
            .call(move |c| store::set_tunnel_status(c, &name, state, port, &err))
            .await;
    }

    async fn on_offline(&self, client_id: String) {
        // 下线落库与新会话注册串行，旧清理不能移除重连后的配置通道。
        let _lifecycle = self.lifecycle.lock().await;
        let remaining = {
            let mut map = self.online.lock().unwrap_or_else(PoisonError::into_inner);
            let remaining = map.get(&client_id).copied().unwrap_or(1).saturating_sub(1);
            if remaining == 0 {
                map.remove(&client_id);
            } else {
                map.insert(client_id.clone(), remaining);
            }
            remaining
        };
        if remaining == 0 {
            // 设备下线后丢弃通道；下次登录会按最新数据重建。
            self.channels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&client_id);
            let id = client_id.clone();
            let _ = self
                .db
                .call(move |c| {
                    store::touch_client(c, &id, store::now_ms())?;
                    store::mark_client_tunnels_stopped(c, &id)
                })
                .await;
        }
    }
}

/// 会话存活凭据：丢弃即视为设备离线。
struct OnlineGuard {
    client_id: String,
    tx: mpsc::Sender<Offline>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Drop for OnlineGuard {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            // 每个排队事件也占用一个许可，因此队列不可能满；关闭时无需落库。
            let _ = self.tx.try_send(Offline {
                client_id: self.client_id.clone(),
                _permit: permit,
            });
        }
    }
}

/// Edge 记在登录消息里的来源地址。
fn remote_ip(login: &Login) -> String {
    login.metas.get("remote_ip").cloned().unwrap_or_default()
}

/// 基于数据库的登录策略：一次性令牌接入 + Ed25519 签名登录。
pub struct DbPolicy {
    /// Hub。
    pub hub: Arc<Hub>,
    /// 数据库。
    pub db: Db,
}

impl DbPolicy {
    async fn admit(
        &self,
        client_id: String,
        just_enrolled: bool,
        ip: String,
    ) -> Result<Admission, String> {
        let permit = self
            .hub
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "managed session capacity reached".to_string())?;
        let _lifecycle = self.hub.lifecycle.lock().await;
        let config = self
            .hub
            .subscribe(&client_id)
            .await
            .map_err(|e| e.to_string())?;
        self.hub.mark_online(&client_id);
        let guard = OnlineGuard {
            client_id: client_id.clone(),
            tx: self.hub.offline_tx.clone(),
            permit: Some(permit),
        };
        let id = client_id.clone();
        let _ = self
            .db
            .call(move |c| {
                store::touch_client(c, &id, store::now_ms())?;
                store::set_client_ip(c, &id, &ip)
            })
            .await;
        Ok(Admission::Managed(Managed {
            client_id: client_id.clone(),
            just_enrolled,
            config,
            events: self.hub.proxy_tx.clone(),
            guard: Box::new(guard),
        }))
    }
}

impl Policy for DbPolicy {
    fn login<'a>(&'a self, login: &'a Login) -> BoxFut<'a, Result<Admission, String>> {
        Box::pin(async move {
            if let Some(en) = &login.enroll {
                if !is_valid_public_hex(&en.pubkey) {
                    return Err("invalid public key".into());
                }
                let (en, hash) = (en.clone(), token_hash(&en.token));
                let created = self
                    .db
                    .call(move |c| {
                        let tx = c.transaction()?;
                        let now = store::now_ms();
                        let id = store::new_id();
                        let token = store::consume_enroll_token(&tx, &hash, &id, now)?;
                        if !token.node_id.is_empty() {
                            // 为外置节点签发的令牌：回滚，不消耗。
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
                        store::audit(
                            &tx,
                            "agent",
                            "client.enroll",
                            "client",
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
                return match created {
                    Ok(id) => self.admit(id, true, remote_ip(login)).await,
                    Err(StoreError::NotFound) => {
                        Err("invalid, used, revoked or expired enroll token".into())
                    }
                    Err(e) => Err(e.to_string()),
                };
            }
            let id = login.client_id.clone();
            let found = self
                .db
                .call(move |c| store::get_client_with_key(c, &id))
                .await;
            let (client, pubkey) = match found {
                Ok(v) => v,
                Err(StoreError::NotFound) => return Err("authentication failed".into()),
                Err(e) => return Err(e.to_string()),
            };
            if !verify_login(
                &pubkey,
                &login.client_id,
                login.timestamp,
                &login.auth,
                unix_now(),
            ) {
                return Err("authentication failed".into());
            }
            if client.disabled_at.is_some() {
                return Err("device is disabled".into());
            }
            if !client.node_id.is_empty() {
                let node_id = client.node_id.clone();
                let node = self
                    .db
                    .call(move |c| store::get_node(c, &node_id))
                    .await
                    .map_err(|_| "device is assigned to an unavailable node".to_string())?;
                if node.disabled_at.is_some()
                    || node.edge_port == 0
                    || node.tls_fingerprint.is_empty()
                {
                    return Err("device is assigned to an unavailable node".into());
                }
                return Ok(Admission::Redirect(rustfrp_core::wire::EdgeRedirect {
                    address: node.public_host,
                    port: node.edge_port,
                    tls_fingerprint: node.tls_fingerprint,
                }));
            }
            self.admit(client.id, false, remote_ip(login)).await
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn offline_backlog_retains_capacity_until_consumed() {
        let (hub, mut events) = Hub::new(Db::open_memory().unwrap());
        for n in 0..MAX_SESSIONS {
            let permit = hub.slots.clone().try_acquire_owned().unwrap();
            drop(OnlineGuard {
                client_id: n.to_string(),
                tx: hub.offline_tx.clone(),
                permit: Some(permit),
            });
        }
        assert_eq!(events.offline_rx.len(), MAX_SESSIONS);
        assert!(hub.slots.clone().try_acquire_owned().is_err());
        let policy = DbPolicy {
            hub: hub.clone(),
            db: hub.db.clone(),
        };
        assert!(
            matches!(policy.admit("missing".into(), false, String::new()).await, Err(e) if e == "managed session capacity reached")
        );
        let first = events.offline_rx.recv().await.unwrap();
        assert_eq!(first.client_id, "0");
        assert_eq!(hub.slots.available_permits(), 0);
        drop(first);
        assert_eq!(hub.slots.available_permits(), 1);
        drop(events);
        assert_eq!(hub.slots.available_permits(), MAX_SESSIONS);
        let permit = hub.slots.clone().try_acquire_owned().unwrap();
        drop(OnlineGuard {
            client_id: "closed".into(),
            tx: hub.offline_tx.clone(),
            permit: Some(permit),
        });
        assert_eq!(hub.slots.available_permits(), MAX_SESSIONS);
        drop(OnlineGuard {
            client_id: "empty".into(),
            tx: hub.offline_tx.clone(),
            permit: None,
        });
    }

    #[tokio::test]
    async fn final_disconnect_reclaims_roster_and_channel() {
        let (hub, _) = Hub::new(Db::open_memory().unwrap());
        let (tx, _) = watch::channel(Arc::new(ConfigSet {
            revision: 0,
            proxies: vec![],
            ..Default::default()
        }));
        hub.channels.lock().unwrap().insert("device".into(), tx);
        hub.mark_online("device");
        hub.mark_online("device");
        hub.on_offline("device".into()).await;
        assert!(hub.is_online("device"));
        assert_eq!(hub.channels.lock().unwrap().len(), 1);
        hub.on_offline("device".into()).await;
        assert!(!hub.is_online("device"));
        assert!(hub.online.lock().unwrap().is_empty());
        assert!(hub.channels.lock().unwrap().is_empty());
        hub.on_offline("unknown".into()).await;
        assert!(hub.online.lock().unwrap().is_empty());
        let policy = DbPolicy {
            hub: hub.clone(),
            db: hub.db.clone(),
        };
        assert!(policy
            .admit("missing".into(), false, String::new())
            .await
            .is_err());
        assert_eq!(hub.slots.available_permits(), MAX_SESSIONS);
    }
}
