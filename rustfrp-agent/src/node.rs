//! 受管 Edge（节点）：用一次性令牌接入 Controller，之后即使 Controller 不在线也能继续服务客户端。
//!
//! * [`Registry`] 是节点的本地设备名册：设备公钥 + 每台设备应运行的隧道配置。它实现 [`Policy`]，
//!   所以客户端登录只查本地名册，不依赖 Controller 在线。
//! * [`run_link`] 维持到 Controller 的控制连接：收清单 / 增量、回报设备上下线与隧道状态，断线自动重连。
//! * 名册会持久化到状态目录（[`SnapshotStore`]），节点重启时 Controller 不可用也能立刻按缓存服务。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustfrp_core::error::TunnelError;
use rustfrp_core::identity::{is_valid_public_hex, verify_login, DeviceKey};
use rustfrp_core::node::{
    CtlMsg, NodeDevice, NodeEnroll, NodeHello, NodeInfo, NodeMsg, ProxyReport, TrafficSample,
    NODE_PROTOCOL_VERSION,
};
use rustfrp_core::wire::{read_frame, write_frame, ConfigSet, Login};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio::time::timeout;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

use crate::edge::{Admission, BoxFut, Managed, Policy, ProxyEvent, ProxyState};
use crate::log::Logger;

/// 最多缓存的设备数（防止被攻陷的 Controller 撑爆节点内存）。
pub const MAX_DEVICES: usize = 4096;
/// 单次对账最多携带的代理状态条数（保证单帧远小于 64 KiB）。
const RESYNC_CHUNK: usize = 200;
/// 等 Controller 处理设备首次接入的默认最长时间（毫秒）。
const ENROLL_TIMEOUT_MS: u64 = 15_000;

/// 名册持久化介质。
pub trait SnapshotStore: Send + Sync {
    /// 读取上次保存的内容。
    fn load(&self) -> Option<String>;
    /// 覆盖保存。
    fn save(&self, text: &str) -> Result<(), String>;
}

/// 文件持久化：先写临时文件再改名，权限仅当前用户。
pub struct FileStore {
    /// 文件路径。
    pub path: std::path::PathBuf,
}

impl SnapshotStore for FileStore {
    fn load(&self) -> Option<String> {
        std::fs::read_to_string(&self.path).ok()
    }
    fn save(&self, text: &str) -> Result<(), String> {
        use std::io::Write;
        let tmp = self.path.with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(io_text)?;
        file.write_all(text.as_bytes()).map_err(io_text)?;
        file.sync_all().map_err(io_text)?;
        std::fs::rename(&tmp, &self.path).map_err(io_text)
    }
}

fn io_text(e: std::io::Error) -> String {
    e.to_string()
}

struct Entry {
    pubkey: String,
    tx: watch::Sender<Arc<ConfigSet>>,
}

#[derive(Default)]
struct Live {
    online: HashMap<String, usize>,
    proxies: HashMap<(String, String), ProxyReport>,
}

/// 节点本地设备名册与登录策略。
pub struct Registry {
    devices: Mutex<HashMap<String, Entry>>,
    live: Mutex<Live>,
    /// 到 Controller 的发送通道；链路断开时不排队（重连后用 Resync 补齐）。
    out: mpsc::Sender<NodeMsg>,
    link_up: AtomicBool,
    dirty: Notify,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<NodeDevice, String>>>>,
    next_req: AtomicU64,
    enroll_timeout_ms: AtomicU64,
    events: mpsc::Sender<ProxyEvent>,
}

/// 会话存活凭据：丢弃即视为设备离线。
struct OnlineGuard {
    registry: Arc<Registry>,
    client_id: String,
}

impl Drop for OnlineGuard {
    fn drop(&mut self) {
        self.registry.went_offline(&self.client_id);
    }
}

impl Registry {
    /// 创建名册。返回名册与「需要发给 Controller 的消息」接收端。
    pub fn new() -> (
        Arc<Registry>,
        mpsc::Receiver<NodeMsg>,
        mpsc::Receiver<ProxyEvent>,
    ) {
        let (out, out_rx) = mpsc::channel(1024);
        let (events, events_rx) = mpsc::channel(256);
        let registry = Arc::new(Registry {
            devices: Mutex::default(),
            live: Mutex::default(),
            out,
            link_up: AtomicBool::new(false),
            dirty: Notify::new(),
            pending: Mutex::default(),
            next_req: AtomicU64::new(1),
            enroll_timeout_ms: AtomicU64::new(ENROLL_TIMEOUT_MS),
            events,
        });
        (registry, out_rx, events_rx)
    }

    fn send(&self, msg: NodeMsg) {
        if self.link_up.load(Ordering::Relaxed) {
            let _ = self.out.try_send(msg);
        }
    }

    /// 设备数。
    pub fn len(&self) -> usize {
        self.devices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// 名册是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 新增或更新设备。公钥变化、被停用都会让该设备的现有会话结束。
    pub fn upsert(&self, d: NodeDevice) -> bool {
        if d.disabled || !is_valid_public_hex(&d.pubkey) {
            self.remove(&d.id);
            return false;
        }
        let mut map = self.devices.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(e) = map.get(&d.id) {
            if e.pubkey == d.pubkey {
                e.tx.send_replace(Arc::new(d.config));
                drop(map);
                self.dirty.notify_one();
                return true;
            }
        }
        if !map.contains_key(&d.id) && map.len() >= MAX_DEVICES {
            return false;
        }
        // 换公钥 = 旧 `tx` 被丢弃，旧会话随之结束。
        let (tx, _) = watch::channel(Arc::new(d.config));
        map.insert(
            d.id,
            Entry {
                pubkey: d.pubkey,
                tx,
            },
        );
        drop(map);
        self.dirty.notify_one();
        true
    }

    /// 移除设备并断开其会话。
    pub fn remove(&self, id: &str) {
        let removed = self
            .devices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
            .is_some();
        if removed {
            self.dirty.notify_one();
        }
    }

    /// 让设备的现有会话结束（设备会自动重连），名册条目保留。
    pub fn kick(&self, id: &str) {
        let mut map = self.devices.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(e) = map.get_mut(id) {
            let current = (**e.tx.borrow()).clone();
            // 旧发送端被丢弃，持有它订阅的会话随之结束。
            e.tx = watch::channel(Arc::new(current)).0;
        }
    }

    /// 名册里全部设备 ID。
    fn ids(&self) -> Vec<String> {
        self.devices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect()
    }

    /// 序列化为持久化内容。
    pub fn snapshot_json(&self) -> String {
        let map = self.devices.lock().unwrap_or_else(PoisonError::into_inner);
        let devices: Vec<NodeDevice> = map
            .iter()
            .map(|(id, e)| NodeDevice {
                id: id.clone(),
                pubkey: e.pubkey.clone(),
                disabled: false,
                config: (**e.tx.borrow()).clone(),
            })
            .collect();
        // 序列化纯数据结构不会失败；退化为空串时 `restore` 会忽略它。
        serde_json::to_string(&devices).unwrap_or_default()
    }

    /// 从持久化内容恢复（格式损坏则忽略，由 Controller 重新下发）。
    pub fn restore(&self, text: &str) -> usize {
        let devices: Vec<NodeDevice> = serde_json::from_str(text).unwrap_or_default();
        devices
            .into_iter()
            .filter(|d| self.upsert(d.clone()))
            .count()
    }

    fn went_online(&self, id: &str, ip: &str) {
        let first = {
            let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
            let n = live.online.entry(id.to_string()).or_insert(0);
            *n += 1;
            *n == 1
        };
        if first {
            self.send(NodeMsg::Online {
                client_id: id.into(),
                ip: ip.into(),
            });
        }
    }

    fn went_offline(&self, id: &str) {
        let last = {
            let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
            let last = match live.online.get_mut(id) {
                Some(n) if *n > 1 => {
                    *n -= 1;
                    false
                }
                _ => true,
            };
            if last {
                live.online.remove(id);
                live.proxies.retain(|(client, _), _| client != id);
            }
            last
        };
        if last {
            self.send(NodeMsg::Offline {
                client_id: id.into(),
            });
        }
    }

    /// 记录并转发一条代理状态。
    fn proxy_event(&self, e: ProxyEvent) {
        let (state, port, error) = match e.state {
            ProxyState::Running(p) => ("running", p, String::new()),
            ProxyState::Failed(m) => ("error", 0, m),
            ProxyState::Stopped => ("stopped", 0, String::new()),
        };
        let report = ProxyReport {
            client_id: e.client_id,
            name: e.name,
            state: state.into(),
            port,
            error,
        };
        {
            let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
            let key = (report.client_id.clone(), report.name.clone());
            if report.state == "stopped" {
                live.proxies.remove(&key);
            } else {
                live.proxies.insert(key, report.clone());
            }
        }
        self.send(NodeMsg::Proxy(report));
    }

    /// 重连后的全量对账帧。
    fn resync_frames(&self) -> Vec<NodeMsg> {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        let online: Vec<String> = live.online.keys().cloned().collect();
        let proxies: Vec<ProxyReport> = live.proxies.values().cloned().collect();
        let mut chunks: Vec<Vec<ProxyReport>> = proxies
            .chunks(RESYNC_CHUNK)
            .map(<[ProxyReport]>::to_vec)
            .collect();
        if chunks.is_empty() {
            chunks.push(Vec::new());
        }
        let last = chunks.len() - 1;
        chunks
            .into_iter()
            .enumerate()
            .map(|(i, proxies)| NodeMsg::Resync {
                online: if i == 0 { online.clone() } else { Vec::new() },
                proxies,
                more: i != last,
            })
            .collect()
    }

    /// 处理 Controller 发来的清单 / 增量消息。`seen` 用于全量清单的差集清理。
    fn apply(&self, msg: CtlMsg, seen: &mut Option<HashSet<String>>) {
        match msg {
            CtlMsg::SnapshotBegin => *seen = Some(HashSet::new()),
            CtlMsg::Device(d) => {
                if let Some(set) = seen.as_mut() {
                    set.insert(d.id.clone());
                }
                self.upsert(d);
            }
            CtlMsg::SnapshotEnd => {
                if let Some(set) = seen.take() {
                    for id in self.ids() {
                        if !set.contains(&id) {
                            self.remove(&id);
                        }
                    }
                }
            }
            CtlMsg::Remove { client_id } => self.remove(&client_id),
            CtlMsg::Kick { client_id } => self.kick(&client_id),
            CtlMsg::RedirectClient { client_id, target } => {
                let mut devices = self.devices.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(entry) = devices.get_mut(&client_id) {
                    let mut config = (**entry.tx.borrow()).clone();
                    config.proxies.clear();
                    config.visitors.clear();
                    config.redirect = Some(target);
                    entry.tx.send_replace(Arc::new(config));
                }
                drop(devices);
                self.dirty.notify_one();
            }
            CtlMsg::EnrollResp {
                req_id,
                ok,
                error,
                device,
            } => {
                let waiter = self
                    .pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&req_id);
                if let Some(w) = waiter {
                    let _ = w.send(match (ok, device) {
                        (true, Some(d)) => Ok(d),
                        (_, _) => Err(if error.is_empty() {
                            "enrollment rejected".into()
                        } else {
                            error
                        }),
                    });
                }
            }
            CtlMsg::Challenge { .. }
            | CtlMsg::HelloResp { .. }
            | CtlMsg::TrafficAck { .. }
            | CtlMsg::Pong => {}
        }
    }

    fn admit(
        self: &Arc<Self>,
        id: &str,
        just_enrolled: bool,
        ip: &str,
    ) -> Result<Admission, String> {
        let config = self
            .devices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .map(|e| e.tx.subscribe())
            .ok_or("authentication failed")?;
        self.went_online(id, ip);
        Ok(Admission::Managed(Managed {
            client_id: id.into(),
            just_enrolled,
            config,
            events: self.events.clone(),
            guard: Box::new(OnlineGuard {
                registry: self.clone(),
                client_id: id.into(),
            }),
        }))
    }

    async fn enroll(self: &Arc<Self>, login: &Login) -> Result<Admission, String> {
        let Some(en) = &login.enroll else {
            return Err("authentication failed".into());
        };
        if !self.link_up.load(Ordering::Relaxed) {
            return Err("controller is unreachable; enrollment needs the controller".into());
        }
        let req_id = self.next_req.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(req_id, tx);
        let sent = self.out.try_send(NodeMsg::Enroll {
            req_id,
            enroll: en.clone(),
        });
        let outcome = match sent {
            Ok(()) => timeout(
                Duration::from_millis(self.enroll_timeout_ms.load(Ordering::Relaxed)),
                rx,
            )
            .await
            .map_err(|_| "controller did not answer in time".to_string())
            .and_then(|r| r.map_err(|_| "controller connection lost".to_string()))
            .and_then(|r| r),
            Err(_) => Err("controller connection is busy".into()),
        };
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&req_id);
        let device = outcome?;
        let id = device.id.clone();
        if !self.upsert(device) {
            return Err("device rejected by the node".into());
        }
        self.admit(
            &id,
            true,
            login.metas.get("remote_ip").map_or("", String::as_str),
        )
    }
}

impl Policy for Arc<Registry> {
    fn login<'a>(&'a self, login: &'a Login) -> BoxFut<'a, Result<Admission, String>> {
        Box::pin(async move {
            if login.enroll.is_some() {
                return self.enroll(login).await;
            }
            let pubkey = self
                .devices
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&login.client_id)
                .map(|e| e.pubkey.clone());
            let ok = pubkey.is_some_and(|k| {
                verify_login(
                    &k,
                    &login.client_id,
                    login.timestamp,
                    &login.auth,
                    rustfrp_core::auth::unix_now(),
                )
            });
            if !ok {
                return Err("authentication failed".into());
            }
            self.admit(
                &login.client_id,
                false,
                login.metas.get("remote_ip").map_or("", String::as_str),
            )
        })
    }
}

/// 把 Edge 回报的代理状态记入名册并转发给 Controller，直到 `cancel`。
/// （名册自己持有事件发送端，所以不能靠通道关闭来判断结束。）
pub async fn pump_events(
    registry: Arc<Registry>,
    mut rx: mpsc::Receiver<ProxyEvent>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            got = rx.recv() => match got {
                Some(e) => registry.proxy_event(e),
                None => return,
            },
        }
    }
}

/// 名册变化后（去抖）写入持久化介质。
pub async fn persist_loop(
    registry: Arc<Registry>,
    store: Arc<dyn SnapshotStore>,
    log: Logger,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = registry.dirty.notified() => {}
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Err(e) = store.save(&registry.snapshot_json()) {
            log.warn(&format!("cannot save the device cache: {e}"));
        }
    }
    let _ = store.save(&registry.snapshot_json());
}

// ───────────── 到 Controller 的链路 ─────────────

/// 链路参数。
pub struct LinkCtx {
    /// Controller 节点端口地址。
    pub host: String,
    /// Controller 节点端口。
    pub port: u16,
    /// 节点身份密钥。
    pub key: Arc<DeviceKey>,
    /// 已签发的节点 ID（首次接入前为空）。
    pub node_id: Option<String>,
    /// 首次接入请求。
    pub enroll: Option<NodeEnroll>,
    /// 节点自述信息。
    pub info: NodeInfo,
    /// TLS 连接器（含 CA / 指纹固定）。
    pub connector: TlsConnector,
    /// 重连退避区间。
    pub backoff: (Duration, Duration),
    /// 心跳间隔 / 无响应超时。
    pub heartbeat: (Duration, Duration),
    /// 本节点的流量计数注册表；`None` 则不上报流量（测试用）。
    pub metrics: Option<Arc<crate::metrics::Metrics>>,
    /// 流量采样间隔。
    pub sample_every: Duration,
    /// 与 Controller 断开超过这么久，就回收已停止代理的计数条目（默认 5 分钟）。
    pub sweep_after: Duration,
}

/// 同时等待确认的流量批次上限（Controller 处理不过来时暂停采样，避免堆积）。
const MAX_PENDING_BATCHES: usize = 8;
/// 每个流量批次的样本数上限（保证单帧远小于 64 KiB）。
const SAMPLES_PER_BATCH: usize = 200;

/// 链路事件（由调用方持久化节点 ID 等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// 首次接入成功，签发了节点 ID。
    Enrolled(String),
    /// 已与 Controller 建立控制连接。
    Connected,
    /// 控制连接断开。
    Disconnected(String),
    /// 登录被拒绝（令牌无效、节点被停用等）。
    Rejected(String),
}

fn spawn_reader(
    mut rd: ReadHalf<tokio_rustls::client::TlsStream<TcpStream>>,
) -> mpsc::Receiver<Result<CtlMsg, TunnelError>> {
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        loop {
            let m = read_frame::<_, CtlMsg>(&mut rd).await;
            let stop = m.is_err();
            if tx.send(m).await.is_err() || stop {
                break;
            }
        }
    });
    rx
}

type Wr = WriteHalf<tokio_rustls::client::TlsStream<TcpStream>>;

/// 保持到 Controller 的连接，断线按指数退避加抖动重连，直到 `cancel`。
/// `reject_exit`：登录被拒绝时是否直接结束（首次接入失败应报错退出；已接入的节点则一直重试）。
pub async fn run_link(
    registry: Arc<Registry>,
    mut out_rx: mpsc::Receiver<NodeMsg>,
    mut ctx: LinkCtx,
    events: mpsc::Sender<LinkEvent>,
    log: Logger,
    cancel: CancellationToken,
    reject_exit: bool,
) {
    let mut delay = ctx.backoff.0;
    // 进程纪元：重启后计数从零开始，Controller 据此另起累计游标。
    let epoch = rustfrp_core::auth::random_id();
    let mut down_since = tokio::time::Instant::now();
    loop {
        let outcome = link_once(&registry, &mut out_rx, &ctx, &events, &log, &cancel, &epoch).await;
        registry.link_up.store(false, Ordering::Relaxed);
        // 与 Controller 断开很久：已停止代理的计数不会再被确认，回收掉以免占满注册表。
        if let Some(m) = &ctx.metrics {
            if down_since.elapsed() > ctx.sweep_after {
                m.sweep_stopped();
                down_since = tokio::time::Instant::now();
            }
        }
        match outcome {
            Outcome::Stopped => return,
            Outcome::Rejected(why) => {
                let _ = events.send(LinkEvent::Rejected(why.clone())).await;
                if reject_exit {
                    return;
                }
                log.warn(&format!("controller rejected this node: {why}"));
            }
            Outcome::Enrolled(id) => {
                ctx.node_id = Some(id.clone());
                ctx.enroll = None;
                let _ = events.send(LinkEvent::Enrolled(id)).await;
                delay = ctx.backoff.0;
                continue;
            }
            Outcome::Lost { reason, was_up } => {
                if was_up {
                    down_since = tokio::time::Instant::now();
                    delay = ctx.backoff.0;
                    let _ = events.send(LinkEvent::Disconnected(reason.clone())).await;
                }
                log.warn(&format!("controller link: {reason}; retrying"));
            }
        }
        // 抖动取当前纳秒，避免多个节点同时重连。
        let jitter = Duration::from_millis(u64::from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
                % 500,
        ));
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(delay + jitter) => {}
        }
        delay = (delay * 2).min(ctx.backoff.1);
    }
}

fn to_wire(s: &crate::metrics::Sample) -> TrafficSample {
    TrafficSample {
        name: s.name.clone(),
        generation: s.generation,
        bytes_in: s.traffic.bytes_in,
        bytes_out: s.traffic.bytes_out,
        connections: s.traffic.connections,
        connections_total: s.traffic.connections_total,
    }
}

fn from_wire(s: &TrafficSample) -> crate::metrics::Sample {
    crate::metrics::Sample {
        name: s.name.clone(),
        kind: String::new(),
        active: true,
        generation: s.generation,
        traffic: rustfrp_core::pipe::TrafficSnapshot {
            bytes_in: s.bytes_in,
            bytes_out: s.bytes_out,
            connections: s.connections,
            connections_total: s.connections_total,
        },
    }
}

enum Outcome {
    Stopped,
    Rejected(String),
    Enrolled(String),
    Lost { reason: String, was_up: bool },
}

async fn link_once(
    registry: &Arc<Registry>,
    out_rx: &mut mpsc::Receiver<NodeMsg>,
    ctx: &LinkCtx,
    events: &mpsc::Sender<LinkEvent>,
    log: &Logger,
    cancel: &CancellationToken,
    epoch: &str,
) -> Outcome {
    let lost = |reason: String| Outcome::Lost {
        reason,
        was_up: false,
    };
    let step = Duration::from_secs(10);
    let tcp = match timeout(step, TcpStream::connect((ctx.host.as_str(), ctx.port))).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return lost(format!("dial: {e}")),
        Err(_) => return lost("dial: timeout".into()),
    };
    let _ = tcp.set_nodelay(true);
    let Ok(name) = ServerName::try_from(ctx.host.clone()) else {
        return lost("invalid controller address for TLS".into());
    };
    let tls = match timeout(step, ctx.connector.clone().connect(name, tcp)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return lost(format!("tls: {e}")),
        Err(_) => return lost("tls: timeout".into()),
    };
    let (mut rd, mut wr): (_, Wr) = tokio::io::split(tls);
    // 登录挑战 → 签名应答。
    let nonce = match timeout(step, read_frame::<_, CtlMsg>(&mut rd)).await {
        Ok(Ok(CtlMsg::Challenge { nonce })) => nonce,
        Ok(Ok(_)) => return lost("controller did not send a challenge".into()),
        Ok(Err(e)) => return lost(e.to_string()),
        Err(_) => return lost("challenge: timeout".into()),
    };
    let node_id = ctx.node_id.clone().unwrap_or_default();
    let hello = NodeHello {
        version: NODE_PROTOCOL_VERSION,
        signature: if ctx.enroll.is_some() {
            String::new()
        } else {
            ctx.key.sign_challenge(&node_id, &nonce)
        },
        node_id: node_id.clone(),
        enroll: ctx.enroll.clone(),
        info: ctx.info.clone(),
    };
    if let Err(e) = write_frame(&mut wr, &NodeMsg::Hello(hello)).await {
        return lost(e.to_string());
    }
    let issued = match timeout(step, read_frame::<_, CtlMsg>(&mut rd)).await {
        Ok(Ok(CtlMsg::HelloResp {
            ok: true, node_id, ..
        })) => node_id,
        Ok(Ok(CtlMsg::HelloResp { error, .. })) => return Outcome::Rejected(error),
        Ok(Ok(_)) => return lost("unexpected reply to hello".into()),
        Ok(Err(e)) => return lost(e.to_string()),
        Err(_) => return lost("hello: timeout".into()),
    };
    if ctx.enroll.is_some() {
        // 首次接入：保存节点 ID 后用新身份重新登录，保证走正常路径。
        return Outcome::Enrolled(issued);
    }

    registry.link_up.store(true, Ordering::Relaxed);
    // 清掉链路断开期间遗留的旧消息，再用对账帧补齐。
    while out_rx.try_recv().is_ok() {}
    for frame in registry.resync_frames() {
        if let Err(e) = write_frame(&mut wr, &frame).await {
            return lost(e.to_string());
        }
    }
    let _ = events.send(LinkEvent::Connected).await;
    log.info("connected to the controller");

    let mut reader = spawn_reader(rd);
    let mut seen: Option<HashSet<String>> = None;
    let mut ping = tokio::time::interval(ctx.heartbeat.0);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = tokio::time::Instant::now();
    let up = |reason: String| Outcome::Lost {
        reason,
        was_up: true,
    };
    let mut sampler = tokio::time::interval(ctx.sample_every);
    sampler.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pending: HashMap<u64, Vec<TrafficSample>> = HashMap::new();
    let mut next_seq = 1u64;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Outcome::Stopped,
            _ = sampler.tick(), if ctx.metrics.is_some() => {
                if let Some(m) = ctx.metrics.as_ref().filter(|_| pending.len() < MAX_PENDING_BATCHES) {
                    let all = m.samples();
                    for chunk in all.chunks(SAMPLES_PER_BATCH) {
                        let batch: Vec<TrafficSample> = chunk.iter().map(to_wire).collect();
                        let seq = next_seq;
                        next_seq += 1;
                        let msg = NodeMsg::Traffic { seq, epoch: epoch.into(), samples: batch.clone() };
                        if let Err(e) = write_frame(&mut wr, &msg).await {
                            return up(e.to_string());
                        }
                        pending.insert(seq, batch);
                    }
                }
            }
            _ = ping.tick() => {
                if last_rx.elapsed() > ctx.heartbeat.1 {
                    return up("heartbeat timeout".into());
                }
                if let Err(e) = write_frame(&mut wr, &NodeMsg::Ping).await {
                    return up(e.to_string());
                }
            }
            msg = out_rx.recv() => match msg {
                Some(m) => {
                    if let Err(e) = write_frame(&mut wr, &m).await {
                        return up(e.to_string());
                    }
                }
                None => return Outcome::Stopped,
            },
            got = reader.recv() => match got {
                Some(Ok(CtlMsg::TrafficAck { seq })) => {
                    last_rx = tokio::time::Instant::now();
                    if let (Some(m), Some(batch)) = (ctx.metrics.as_ref(), pending.remove(&seq)) {
                        for s in &batch {
                            m.acknowledge_sample(&from_wire(s));
                        }
                    }
                }
                Some(Ok(m)) => {
                    last_rx = tokio::time::Instant::now();
                    registry.apply(m, &mut seen);
                }
                Some(Err(e)) => return up(e.to_string()),
                None => return up("connection closed".into()),
            },
        }
    }
}

// ───────────── 组装 ─────────────

/// 启动一个受管 Edge 所需的全部输入。
pub struct NodeRun {
    /// Edge 监听参数。
    pub edge: crate::edge::EdgeConfig,
    /// 给客户端用的 TLS 证书（`None` = 临时自签名）。
    pub identity: Option<rustfrp_core::tls::Identity>,
    /// frps dashboard 的独立 TLS 身份。
    pub dashboard_identity: Option<rustfrp_core::tls::Identity>,
    /// Whether CONNECT requests are forwarded unchanged to the selected Client.
    pub tcpmux_passthrough: bool,
    /// 到 Controller 的链路参数。
    pub link: LinkCtx,
    /// 设备名册的持久化介质。
    pub store: Arc<dyn SnapshotStore>,
    /// 链路事件（保存节点 ID 等）。
    pub events: mpsc::Sender<LinkEvent>,
    /// 首次接入失败是否直接结束。
    pub reject_exit: bool,
}

/// 运行中的受管 Edge。
pub struct NodeHandle {
    /// 客户端接入地址。
    pub edge_addr: std::net::SocketAddr,
    /// HTTP 虚拟主机地址。
    pub http_addr: Option<std::net::SocketAddr>,
    /// 设备名册（测试与诊断用）。
    pub registry: Arc<Registry>,
    /// 代理流量计数注册表（测试与诊断用）。
    pub metrics: Arc<crate::metrics::Metrics>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl NodeHandle {
    /// 等全部后台任务结束（调用方先取消 `cancel`）。
    pub async fn join(self) {
        for t in self.tasks {
            let _ = t.await;
        }
    }
}

fn start_error(e: rustfrp_core::error::TunnelError) -> String {
    format!("start edge: {e}")
}

/// 绑定端口并启动 Edge、名册持久化与到 Controller 的链路。
pub async fn start_node(
    mut run: NodeRun,
    log: Logger,
    cancel: CancellationToken,
) -> Result<NodeHandle, String> {
    let (registry, out_rx, events_rx) = Registry::new();
    if let Some(text) = run.store.load() {
        let n = registry.restore(&text);
        log.info(&format!("restored {n} device(s) from the local cache"));
    }
    let policy: Arc<dyn Policy> = Arc::new(registry.clone());
    let edge = crate::edge::Edge::bind_with_dashboard(
        run.edge.clone(),
        run.identity.take(),
        run.dashboard_identity.take(),
        policy,
        log.clone(),
    )
    .await
    .map_err(start_error)?
    .retain_metrics(true)
    .with_tcpmux_passthrough(run.tcpmux_passthrough);
    let metrics = edge.metrics();
    run.link.metrics = Some(metrics.clone());
    let edge_addr = edge.local_addr().map_err(io_text)?;
    let http_addr = edge.http_addr();
    run.link.info.edge_port = edge_addr.port();
    run.link.info.tls_fingerprint = edge.certificate_fingerprint().to_string();
    run.link.info.vhost_http = http_addr.map(|a| a.port());
    run.link.info.vhost_https = edge.https_addr().map(|a| a.port());
    run.link.info.tcpmux_http_connect_port = edge.tcpmux_addr().map(|a| a.port());
    run.link.info.subdomain_host = run.edge.sub_domain_host.clone();
    run.link.info.allow_ports = run.edge.allow_ports.clone();
    log.info(&format!("rustfrp node listening on {edge_addr}"));
    let tasks = vec![
        tokio::spawn(edge.serve(cancel.clone())),
        tokio::spawn(pump_events(registry.clone(), events_rx, cancel.clone())),
        tokio::spawn(persist_loop(
            registry.clone(),
            run.store.clone(),
            log.clone(),
            cancel.clone(),
        )),
        tokio::spawn(run_link(
            registry.clone(),
            out_rx,
            run.link,
            run.events,
            log,
            cancel,
            run.reject_exit,
        )),
    ];
    Ok(NodeHandle {
        edge_addr,
        http_addr,
        registry,
        metrics,
        tasks,
    })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use rustfrp_core::wire::ManagedProxy;

    fn device(id: &str, key: &DeviceKey, rev: u64) -> NodeDevice {
        NodeDevice {
            id: id.into(),
            pubkey: key.public_hex(),
            disabled: false,
            config: ConfigSet {
                revision: rev,
                proxies: vec![ManagedProxy {
                    name: format!("t-{id}"),
                    kind: "tcp".into(),
                    remote_port: 6000,
                    local_ip: "127.0.0.1".into(),
                    local_port: 22,
                    ..Default::default()
                }],
                ..Default::default()
            },
        }
    }

    fn login(id: &str, key: &DeviceKey) -> Login {
        let ts = rustfrp_core::auth::unix_now();
        Login {
            version: 1,
            user: String::new(),
            client_id: id.into(),
            timestamp: ts,
            auth: key.sign_login(id, ts),
            metas: Default::default(),
            enroll: None,
        }
    }

    #[tokio::test]
    async fn local_registry_authenticates_devices_without_the_controller() {
        let (r, mut out, _events) = Registry::new();
        let k = DeviceKey::generate().unwrap();
        assert!(r.is_empty());
        assert!(r.upsert(device("c1", &k, 1)));
        assert_eq!(r.len(), 1);
        // 合法登录：返回受管准入，上线事件在链路断开时不排队。
        let Ok(Admission::Managed(m)) = r.login(&login("c1", &k)).await else {
            panic!("expected admission");
        };
        assert_eq!(m.config.borrow().revision, 1);
        assert!(out.try_recv().is_err(), "link is down: nothing queued");
        // 错误的密钥 / 未知设备 / 过期签名。
        let other = DeviceKey::generate().unwrap();
        assert!(r.login(&login("c1", &other)).await.is_err());
        assert!(r.login(&login("nope", &k)).await.is_err());
        let mut stale = login("c1", &k);
        stale.timestamp -= 10_000;
        stale.auth = k.sign_login("c1", stale.timestamp);
        assert!(r.login(&stale).await.is_err());
        // 增量更新通过 watch 推给在线会话，不需要重连。
        let mut cfg = m.config.clone();
        assert!(r.upsert(device("c1", &k, 2)));
        cfg.changed().await.unwrap();
        assert_eq!(cfg.borrow().revision, 2);
        // 踢下线：会话结束，但设备仍可重新登录。
        r.kick("c1");
        r.kick("unknown");
        assert!(cfg.changed().await.is_err());
        assert!(r.login(&login("c1", &k)).await.is_ok());
        let Ok(Admission::Managed(m)) = r.login(&login("c1", &k)).await else {
            panic!("expected admission");
        };
        let mut cfg = m.config.clone();
        // 公钥变化 / 移除：现有会话的通道关闭。
        let k2 = DeviceKey::generate().unwrap();
        assert!(r.upsert(device("c1", &k2, 3)));
        assert!(cfg.changed().await.is_err(), "old session is dropped");
        assert!(
            r.login(&login("c1", &k)).await.is_err(),
            "old key no longer works"
        );
        r.remove("c1");
        r.remove("c1");
        assert!(r.is_empty());
        // 停用 / 非法公钥 / 容量上限。
        let mut dis = device("c2", &k, 1);
        dis.disabled = true;
        assert!(!r.upsert(dis));
        let mut bad = device("c3", &k, 1);
        bad.pubkey = "zz".into();
        assert!(!r.upsert(bad));
        for i in 0..MAX_DEVICES {
            assert!(r.upsert(device(&format!("d{i}"), &k, 1)));
        }
        assert!(!r.upsert(device("overflow", &k, 1)));
        assert!(
            r.upsert(device("d0", &k, 9)),
            "updating an existing device still works at capacity"
        );
    }

    #[tokio::test]
    async fn redirect_command_clears_proxy_snapshot_and_survives_later_login() {
        let (registry, _out, _events) = Registry::new();
        let key = DeviceKey::generate().unwrap();
        assert!(registry.upsert(device("moved", &key, 9)));
        let target = rustfrp_core::wire::EdgeRedirect {
            address: "new.example.test".into(),
            port: 7443,
            tls_fingerprint: "ab".repeat(32),
        };
        let mut seen = None;
        registry.apply(
            CtlMsg::RedirectClient {
                client_id: "moved".into(),
                target: target.clone(),
            },
            &mut seen,
        );
        let Ok(Admission::Managed(session)) = registry.login(&login("moved", &key)).await else {
            panic!("redirect snapshot must remain an authenticated managed entry");
        };
        let config = session.config.borrow();
        assert!(config.proxies.is_empty());
        assert_eq!(config.redirect.as_ref(), Some(&target));
    }

    #[tokio::test]
    async fn online_offline_and_proxy_state_are_reported_and_resynced() {
        let (r, mut out, _events) = Registry::new();
        let k = DeviceKey::generate().unwrap();
        r.upsert(device("c1", &k, 1));
        r.link_up.store(true, Ordering::Relaxed);
        let mut first = login("c1", &k);
        first
            .metas
            .insert("remote_ip".into(), "198.51.100.7".into());
        let Ok(Admission::Managed(a)) = r.login(&first).await else {
            panic!()
        };
        let Ok(Admission::Managed(b)) = r.login(&login("c1", &k)).await else {
            panic!()
        };
        assert_eq!(
            out.recv().await,
            Some(NodeMsg::Online {
                client_id: "c1".into(),
                ip: "198.51.100.7".into()
            })
        );
        assert!(
            out.try_recv().is_err(),
            "second session of the same device is not a new online event"
        );
        for (state, name) in [
            (ProxyState::Running(6000), "t1"),
            (ProxyState::Failed("boom".into()), "t2"),
            (ProxyState::Running(6001), "t3"),
            (ProxyState::Stopped, "t3"),
        ] {
            r.proxy_event(ProxyEvent {
                client_id: "c1".into(),
                name: name.into(),
                state,
            });
        }
        let mut sent = Vec::new();
        while let Ok(m) = out.try_recv() {
            sent.push(m);
        }
        assert_eq!(sent.len(), 4);
        let frames = r.resync_frames();
        assert_eq!(frames.len(), 1);
        let NodeMsg::Resync {
            online,
            proxies,
            more,
        } = &frames[0]
        else {
            panic!()
        };
        assert_eq!(
            (online.clone(), proxies.len(), *more),
            (vec!["c1".to_string()], 2, false)
        );
        drop(a);
        assert!(out.try_recv().is_err());
        drop(b);
        assert_eq!(
            out.recv().await,
            Some(NodeMsg::Offline {
                client_id: "c1".into()
            })
        );
        // 设备离线后它的代理状态一并清理。
        let NodeMsg::Resync {
            online, proxies, ..
        } = &r.resync_frames()[0]
        else {
            panic!()
        };
        assert!(online.is_empty() && proxies.is_empty());
        // 大量代理状态分帧，只有第一帧带在线列表。
        r.upsert(device("c2", &k, 1));
        let Ok(Admission::Managed(_keep)) = r.login(&login("c2", &k)).await else {
            panic!()
        };
        for i in 0..(RESYNC_CHUNK * 2 + 5) {
            r.proxy_event(ProxyEvent {
                client_id: "c2".into(),
                name: format!("p{i}"),
                state: ProxyState::Running(1),
            });
        }
        let frames = r.resync_frames();
        assert_eq!(frames.len(), 3);
        let flags: Vec<bool> = frames
            .iter()
            .map(|f| matches!(f, NodeMsg::Resync { more: true, .. }))
            .collect();
        assert_eq!(flags, [true, true, false]);
        assert!(matches!(&frames[1], NodeMsg::Resync { online, .. } if online.is_empty()));
    }

    #[test]
    fn controller_messages_update_the_registry_and_snapshots_prune() {
        let (r, _out, _events) = Registry::new();
        let k = DeviceKey::generate().unwrap();
        let mut seen = None;
        r.apply(CtlMsg::Device(device("a", &k, 1)), &mut seen);
        r.apply(CtlMsg::Device(device("b", &k, 1)), &mut seen);
        assert_eq!(r.len(), 2);
        r.apply(
            CtlMsg::RedirectClient {
                client_id: "missing".into(),
                target: rustfrp_core::wire::EdgeRedirect {
                    address: "edge.example.test".into(),
                    port: 7000,
                    tls_fingerprint: "ab".repeat(32),
                },
            },
            &mut seen,
        );
        assert_eq!(
            r.len(),
            2,
            "redirects for unknown clients do not add devices"
        );
        // 全量清单：未出现的 b 被清掉。
        r.apply(CtlMsg::SnapshotBegin, &mut seen);
        r.apply(CtlMsg::Device(device("a", &k, 2)), &mut seen);
        r.apply(CtlMsg::SnapshotEnd, &mut seen);
        assert_eq!(r.ids(), vec!["a".to_string()]);
        // 没有 Begin 的 End 不会误删。
        r.apply(CtlMsg::SnapshotEnd, &mut seen);
        assert_eq!(r.len(), 1);
        r.apply(
            CtlMsg::Remove {
                client_id: "a".into(),
            },
            &mut seen,
        );
        assert!(r.is_empty());
        for m in [
            CtlMsg::Pong,
            CtlMsg::Challenge { nonce: "n".into() },
            CtlMsg::HelloResp {
                ok: true,
                error: String::new(),
                node_id: String::new(),
            },
        ] {
            r.apply(m, &mut seen);
        }
        // 持久化往返；损坏的内容被忽略。
        r.apply(CtlMsg::Device(device("a", &k, 5)), &mut seen);
        let text = r.snapshot_json();
        let (fresh, _o, _e) = Registry::new();
        assert_eq!(fresh.restore(&text), 1);
        assert_eq!(fresh.restore("not json"), 0);
        assert_eq!(fresh.len(), 1);
    }

    /// 取下一条接入请求（跳过上下线等其他消息）。
    async fn next_enroll(out: &mut mpsc::Receiver<NodeMsg>) -> u64 {
        loop {
            if let NodeMsg::Enroll { req_id, .. } = out.recv().await.unwrap() {
                return req_id;
            }
        }
    }

    #[tokio::test]
    async fn enrollment_goes_through_the_controller_and_times_out_cleanly() {
        let (r, mut out, _events) = Registry::new();
        let k = DeviceKey::generate().unwrap();
        let mut l = login("", &k);
        l.enroll = Some(rustfrp_core::wire::Enroll {
            token: "t".into(),
            name: "box".into(),
            pubkey: k.public_hex(),
            os: "linux".into(),
            arch: "x86_64".into(),
            version: "0".into(),
        });
        // 控制面不可达：直接拒绝。
        assert!(r.login(&l).await.err().unwrap().contains("unreachable"));
        r.link_up.store(true, Ordering::Relaxed);
        let rr = r.clone();
        let l2 = l.clone();
        let task = tokio::spawn(async move { rr.login(&l2).await });
        let req_id = next_enroll(&mut out).await;
        let mut seen = None;
        r.apply(
            CtlMsg::EnrollResp {
                req_id,
                ok: true,
                error: String::new(),
                device: Some(device("new", &k, 1)),
            },
            &mut seen,
        );
        let Ok(Admission::Managed(m)) = task.await.unwrap() else {
            panic!()
        };
        assert!(m.just_enrolled && m.client_id == "new");
        assert_eq!(r.len(), 1);
        // 被拒绝（带原因 / 不带原因）。
        for (error, want) in [("token used", "token used"), ("", "enrollment rejected")] {
            let rr = r.clone();
            let l3 = l.clone();
            let task = tokio::spawn(async move { rr.login(&l3).await });
            let req_id = next_enroll(&mut out).await;
            r.apply(
                CtlMsg::EnrollResp {
                    req_id,
                    ok: false,
                    error: error.into(),
                    device: None,
                },
                &mut seen,
            );
            assert_eq!(task.await.unwrap().err().unwrap(), want);
        }
        // Controller 回了个不合法的设备条目：节点拒绝它，设备拿到明确原因。
        let rr = r.clone();
        let l4 = l.clone();
        let task = tokio::spawn(async move { rr.login(&l4).await });
        let req_id = next_enroll(&mut out).await;
        let mut bad_device = device("bad", &k, 1);
        bad_device.pubkey = "zz".into();
        r.apply(
            CtlMsg::EnrollResp {
                req_id,
                ok: true,
                error: String::new(),
                device: Some(bad_device),
            },
            &mut seen,
        );
        assert_eq!(
            task.await.unwrap().err().unwrap(),
            "device rejected by the node"
        );
        // 未知请求编号的应答被忽略；控制面不回应：超时。
        r.apply(
            CtlMsg::EnrollResp {
                req_id: 999,
                ok: true,
                error: String::new(),
                device: None,
            },
            &mut seen,
        );
        r.enroll_timeout_ms.store(100, Ordering::Relaxed);
        let rr = r.clone();
        let task = tokio::spawn(async move { rr.login(&l).await });
        let _ = next_enroll(&mut out).await;
        assert!(task
            .await
            .unwrap()
            .err()
            .unwrap()
            .contains("did not answer"));
        // 登录消息缺少 enroll 时走不到这里（防御分支）。
        let plain = login("x", &k);
        assert!(r.enroll(&plain).await.is_err());
    }

    #[tokio::test]
    async fn enrollment_fails_cleanly_when_the_link_drops_midway_and_start_reports_bind_errors() {
        let (r, mut out, _events) = Registry::new();
        let k = DeviceKey::generate().unwrap();
        let mut l = login("", &k);
        l.enroll = Some(rustfrp_core::wire::Enroll {
            token: "t".into(),
            name: "box".into(),
            pubkey: k.public_hex(),
            os: String::new(),
            arch: String::new(),
            version: String::new(),
        });
        r.link_up.store(true, Ordering::Relaxed);
        let rr = r.clone();
        let task = tokio::spawn(async move { rr.login(&l).await });
        let _ = next_enroll(&mut out).await;
        // 链路断开：等待中的请求被丢弃。
        r.pending.lock().unwrap().clear();
        assert!(task
            .await
            .unwrap()
            .err()
            .unwrap()
            .contains("connection lost"));

        // 把请求通道塞满：新的接入请求被明确拒绝，而不是无限排队。
        let (r2, out2, _e) = Registry::new();
        r2.link_up.store(true, Ordering::Relaxed);
        for _ in 0..1024 {
            r2.out.try_send(NodeMsg::Ping).unwrap();
        }
        let mut l2 = login("", &k);
        l2.enroll = Some(rustfrp_core::wire::Enroll {
            token: "t".into(),
            name: "b".into(),
            pubkey: k.public_hex(),
            os: String::new(),
            arch: String::new(),
            version: String::new(),
        });
        assert!(r2.login(&l2).await.err().unwrap().contains("busy"));
        drop(out2);

        // 监听端口已被占用：start_node 返回带原因的错误，而不是 panic。
        let busy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = busy.local_addr().unwrap().port();
        let (events, _rx) = mpsc::channel(1);
        let run = NodeRun {
            edge: crate::edge::EdgeConfig {
                bind_addr: "127.0.0.1".into(),
                bind_port: port,
                ..Default::default()
            },
            identity: None,
            dashboard_identity: None,
            tcpmux_passthrough: false,
            link: LinkCtx {
                host: "127.0.0.1".into(),
                port: 1,
                key: Arc::new(k),
                node_id: Some("n".into()),
                enroll: None,
                info: NodeInfo::default(),
                connector: TlsConnector::from(rustfrp_core::tls::client_config(None).unwrap()),
                backoff: (Duration::from_millis(10), Duration::from_millis(20)),
                heartbeat: (Duration::from_secs(1), Duration::from_secs(2)),
                metrics: None,
                sample_every: Duration::from_secs(1),
                sweep_after: Duration::from_secs(300),
            },
            store: Arc::new(FileStore {
                path: std::env::temp_dir().join("rfrp-never-written.json"),
            }),
            events,
            reject_exit: false,
        };
        let err = start_node(run, Logger::silent(), CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert!(err.starts_with("start edge:"), "{err}");
    }

    #[test]
    fn file_store_roundtrip_uses_private_permissions() {
        let dir = std::env::temp_dir().join(format!("rfrp-node-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = FileStore {
            path: dir.join("snapshot.json"),
        };
        assert!(store.load().is_none());
        store.save("[1]").unwrap();
        store.save("[2]").unwrap();
        assert_eq!(store.load().as_deref(), Some("[2]"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&store.path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let blocked = FileStore {
            path: dir.join("missing-dir/x.json"),
        };
        assert!(blocked.save("x").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
