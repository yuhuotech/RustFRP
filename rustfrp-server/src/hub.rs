//! Hub：在线设备、配置下发通道、Edge 回报的状态事件，以及对应的登录策略。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use rustfrp_agent::edge::{Admission, BoxFut, Managed, Policy, ProxyEvent, ProxyState};
use rustfrp_core::auth::unix_now;
use rustfrp_core::identity::{is_valid_public_hex, verify_login};
use rustfrp_core::wire::{ConfigSet, Login};
use tokio::sync::{mpsc, watch};

use crate::auth::token_hash;
use crate::db::{Db, StoreError};
use crate::store;

type ConfigTx = watch::Sender<Arc<ConfigSet>>;

/// Hub 运行时状态。
pub struct Hub {
    db: Db,
    channels: Mutex<HashMap<String, ConfigTx>>,
    online: Mutex<HashMap<String, usize>>,
    proxy_tx: mpsc::Sender<ProxyEvent>,
    offline_tx: mpsc::UnboundedSender<String>,
}

/// Hub 的事件接收端，交给 [`Hub::run_events`] 消费。
pub struct HubEvents {
    proxy_rx: mpsc::Receiver<ProxyEvent>,
    offline_rx: mpsc::UnboundedReceiver<String>,
}

impl Hub {
    /// 创建 Hub 与其事件接收端。
    pub fn new(db: Db) -> (Arc<Hub>, HubEvents) {
        let (proxy_tx, proxy_rx) = mpsc::channel(256);
        let (offline_tx, offline_rx) = mpsc::unbounded_channel();
        let hub = Arc::new(Hub {
            db,
            channels: Mutex::default(),
            online: Mutex::default(),
            proxy_tx,
            offline_tx,
        });
        (
            hub,
            HubEvents {
                proxy_rx,
                offline_rx,
            },
        )
    }

    /// 设备当前是否在线。
    pub fn is_online(&self, client_id: &str) -> bool {
        self.online
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(client_id)
            .is_some_and(|n| *n > 0)
    }

    /// 全部在线设备 ID。
    pub fn online_ids(&self) -> HashSet<String> {
        self.online
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(id, _)| id.clone())
            .collect()
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
                    Some(id) => self.on_offline(id).await,
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
        let remaining = {
            let mut map = self.online.lock().unwrap_or_else(PoisonError::into_inner);
            let n = map.entry(client_id.clone()).or_insert(1);
            *n = n.saturating_sub(1);
            *n
        };
        if remaining == 0 {
            let id = client_id.clone();
            let _ = self
                .db
                .call(move |c| {
                    store::touch_client(c, &id, store::now_ms())?;
                    store::mark_client_tunnels_stopped(c, &id)
                })
                .await;
            // 设备下线后丢弃通道；下次登录会按最新数据重建。
            self.channels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&client_id);
        }
    }
}

/// 会话存活凭据：丢弃即视为设备离线。
struct OnlineGuard {
    client_id: String,
    tx: mpsc::UnboundedSender<String>,
}

impl Drop for OnlineGuard {
    fn drop(&mut self) {
        let _ = self.tx.send(self.client_id.clone());
    }
}

/// 基于数据库的登录策略：一次性令牌接入 + Ed25519 签名登录。
pub struct DbPolicy {
    /// Hub。
    pub hub: Arc<Hub>,
    /// 数据库。
    pub db: Db,
}

impl DbPolicy {
    async fn admit(&self, client_id: String, just_enrolled: bool) -> Result<Admission, String> {
        let config = self
            .hub
            .subscribe(&client_id)
            .await
            .map_err(|e| e.to_string())?;
        self.hub.mark_online(&client_id);
        let id = client_id.clone();
        let _ = self
            .db
            .call(move |c| store::touch_client(c, &id, store::now_ms()))
            .await;
        Ok(Admission::Managed(Managed {
            client_id: client_id.clone(),
            just_enrolled,
            config,
            events: self.hub.proxy_tx.clone(),
            guard: Box::new(OnlineGuard {
                client_id,
                tx: self.hub.offline_tx.clone(),
            }),
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
                    Ok(id) => self.admit(id, true).await,
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
            self.admit(client.id, false).await
        })
    }
}
