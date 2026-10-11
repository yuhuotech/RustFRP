//! 客户端（frpc 角色）：连接边缘节点，注册代理，并把到来的流转发给本地服务。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustfrp_core::auth::{auth_key, random_id, unix_now};
use rustfrp_core::error::TunnelError;
use rustfrp_core::identity::DeviceKey;
use rustfrp_core::mux::{self, MuxConfig, Session, Stream};
use rustfrp_core::pipe;
use rustfrp_core::tls;
use rustfrp_core::udpframe::{read_datagram, write_datagram};
use rustfrp_core::wire::{
    read_frame, write_frame, ClientMsg, Enroll, Login, ProxySpec, ServerMsg, StartWork,
    PROTOCOL_VERSION,
};
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

use crate::log::Logger;

/// 一个待注册的代理。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyCfg {
    /// 名称（已含 user 前缀）。
    pub name: String,
    /// 类型：`tcp` / `udp` / `http` / `https`。
    pub kind: String,
    /// 本地目标主机。
    pub local_ip: String,
    /// 本地目标端口。
    pub local_port: u16,
    /// 请求的远程端口，0 = 由服务端分配。
    pub remote_port: u16,
    /// http / https：自定义域名。
    pub custom_domains: Vec<String>,
    /// http / https：子域名。
    pub subdomain: String,
    /// http：改写 Host 头。
    pub host_header_rewrite: String,
    /// http：路径 / 认证 / 请求头选项。
    pub http: rustfrp_core::wire::HttpOpts,
    /// 连接本地服务时写入 PROXY protocol 头：0 = 不写，1 / 2 = v1 / v2。
    pub proxy_protocol: u8,
    /// 带宽上限（字节 / 秒），0 = 不限。
    pub bandwidth_bps: u64,
    /// 限速由服务端（Edge）执行；否则由本客户端执行。
    pub bandwidth_server: bool,
    /// 负载均衡组与健康检查。
    pub lb: rustfrp_core::wire::LbOpts,
    /// 本地插件（代替连接本地端口）。
    pub plugin: Option<crate::plugin::PluginCfg>,
    /// stcp / sudp 的访问控制。
    pub secret: rustfrp_core::wire::SecretOpts,
}

/// 首次接入所需信息。
#[derive(Debug, Clone)]
pub struct EnrollInfo {
    /// 一次性接入令牌。
    pub token: String,
    /// 设备名。
    pub name: String,
    /// 操作系统。
    pub os: String,
    /// CPU 架构。
    pub arch: String,
    /// Agent 版本。
    pub version: String,
}

/// 受管模式：由 Controller 签发身份并下发配置。
#[derive(Debug, Clone)]
pub struct ManagedCfg {
    /// 设备密钥。
    pub key: Arc<DeviceKey>,
    /// 已签发的设备 ID；为空表示尚未接入，必须提供 `enroll`。
    pub client_id: Option<String>,
    /// 首次接入信息。
    pub enroll: Option<EnrollInfo>,
}

/// 客户端运行参数。
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// 边缘节点地址。
    pub server_addr: String,
    /// 边缘节点端口。
    pub server_port: u16,
    /// 令牌；空表示不鉴权。
    pub token: String,
    /// 代理名前缀（frp 的 `user`）。
    pub user: String,
    /// 客户端标识。
    pub client_id: String,
    /// 受信 CA（PEM 内容）；为空则不校验服务端证书（frp 默认行为）。
    pub trusted_ca_pem: Option<Vec<u8>>,
    /// 首次登录失败是否直接退出（frp `loginFailExit`）。
    pub login_fail_exit: bool,
    /// 代理列表。
    pub proxies: Vec<ProxyCfg>,
    /// 访问者（stcp / sudp）；受管模式下由 Controller 下发。
    pub visitors: Vec<rustfrp_core::wire::VisitorSpec>,
    /// 连接超时。
    pub dial_timeout: Duration,
    /// 握手类步骤超时。
    pub handshake_timeout: Duration,
    /// 心跳间隔。
    pub heartbeat_interval: Duration,
    /// 控制流多久收不到应答就判定断线。
    pub heartbeat_timeout: Duration,
    /// 连接本地服务的超时。
    pub local_dial_timeout: Duration,
    /// 重连退避的起点与上限。
    pub backoff: (Duration, Duration),
    /// 多路复用参数。
    pub mux: MuxConfig,
    /// UDP 接收缓冲（frp 的 `udpPacketSize`）。
    pub udp_packet_size: usize,
    /// UDP 会话空闲回收时间。
    pub udp_idle: Duration,
    /// 受管模式（`None` 为独立模式，等价 frpc）。
    pub managed: Option<ManagedCfg>,
    /// 本机展示的实时计数，不上传控制器重复累计。
    pub traffic: Arc<pipe::Traffic>,
    /// 私有本机界面读取的已应用摘要。
    pub local_tunnels: crate::local_control::TunnelList,
    /// 受管接入的固定叶证书 SHA256。
    pub tls_pin: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server_addr: "127.0.0.1".into(),
            server_port: 7000,
            token: String::new(),
            user: String::new(),
            client_id: String::new(),
            trusted_ca_pem: None,
            login_fail_exit: true,
            proxies: Vec::new(),
            visitors: Vec::new(),
            dial_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(10),
            heartbeat_interval: Duration::from_secs(30),
            heartbeat_timeout: Duration::from_secs(90),
            local_dial_timeout: Duration::from_secs(5),
            backoff: (Duration::from_secs(1), Duration::from_secs(30)),
            mux: MuxConfig::default(),
            udp_packet_size: 1500,
            udp_idle: Duration::from_secs(60),
            managed: None,
            traffic: Arc::default(),
            local_tunnels: Arc::default(),
            tls_pin: None,
        }
    }
}

/// 运行期事件（日志之外，供上层 UI / 控制面订阅）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 登录成功。
    Connected,
    /// 某代理启动成功。
    ProxyStarted {
        /// 代理名。
        name: String,
        /// 实际远程端口。
        remote_port: u16,
    },
    /// 某访问者开始监听。
    VisitorStarted {
        /// 访问者名。
        name: String,
        /// 实际监听端口。
        port: u16,
    },
    /// 某访问者启动失败。
    VisitorFailed {
        /// 访问者名。
        name: String,
        /// 原因。
        error: String,
    },
    /// 某代理启动失败。
    ProxyFailed {
        /// 代理名。
        name: String,
        /// 原因。
        error: String,
    },
    /// 接入成功：Controller 签发了设备 ID，调用方应把它与密钥一起持久化。
    Enrolled {
        /// 设备 ID。
        client_id: String,
    },
    /// 已应用一版受管配置。
    ConfigApplied {
        /// 版本号。
        revision: u64,
        /// 代理数量。
        proxies: usize,
    },
    /// 管理员要求重启这台设备上的 agent。
    RestartRequested,
    /// 连接断开（随后会重连）。
    Disconnected(String),
}

/// 客户端错误。
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// 登录被拒（且配置了 `loginFailExit`）。
    #[error("login to server failed: {0}")]
    LoginFailed(String),
    /// 配置有误。
    #[error("config: {0}")]
    Config(String),
}

enum SessionEnd {
    /// 被主动取消。
    Cancelled,
    /// 连接中断，附原因；`logged_in` 表示这次连接曾登录成功。
    Lost { reason: String, logged_in: bool },
    /// 登录被拒。
    Rejected(String),
}

fn emit(events: &Option<mpsc::Sender<Event>>, e: Event) {
    if let Some(tx) = events {
        let _ = tx.try_send(e);
    }
}

/// 运行客户端：保持与边缘节点的连接，断线按退避重连，直到 `cancel` 被取消。
pub async fn run(
    cfg: ClientConfig,
    log: Logger,
    events: Option<mpsc::Sender<Event>>,
    cancel: CancellationToken,
) -> Result<(), ClientError> {
    let connector = build_connector(&cfg)?;
    let cfg = Arc::new(cfg);
    let mut delay = cfg.backoff.0;
    // 接入成功后由服务端签发的设备 ID：此后重连改用签名登录，不再使用（已作废的）一次性令牌。
    let issued: Issued = Arc::default();
    let mut ever_logged_in = false;
    loop {
        let end = tokio::select! {
            _ = cancel.cancelled() => SessionEnd::Cancelled,
            end = session_once(&cfg, &connector, &log, &events, &cancel, &issued) => end,
        };
        match end {
            SessionEnd::Cancelled => return Ok(()),
            SessionEnd::Rejected(e) => {
                log.error(&format!("login to server failed: {e}"));
                if cfg.login_fail_exit && !ever_logged_in {
                    return Err(ClientError::LoginFailed(e));
                }
            }
            SessionEnd::Lost { reason, logged_in } => {
                log.warn(&format!("connection lost: {reason}"));
                emit(&events, Event::Disconnected(reason));
                if logged_in {
                    ever_logged_in = true;
                    delay = cfg.backoff.0;
                }
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = sleep(delay) => {}
        }
        delay = (delay * 2).min(cfg.backoff.1);
    }
}

fn build_connector(cfg: &ClientConfig) -> Result<TlsConnector, ClientError> {
    let trusted: Option<Vec<CertificateDer<'static>>> = match &cfg.trusted_ca_pem {
        Some(pem) => {
            Some(tls::certs_from_pem(pem).map_err(|e| ClientError::Config(e.to_string()))?)
        }
        None => None,
    };
    let conf = tls::client_config_pinned(trusted, cfg.tls_pin.as_deref())
        .map_err(|e| ClientError::Config(e.to_string()))?;
    Ok(TlsConnector::from(conf))
}

async fn within<T>(
    d: Duration,
    what: &'static str,
    f: impl std::future::Future<Output = Result<T, TunnelError>>,
) -> Result<T, TunnelError> {
    timeout(d, f)
        .await
        .map_err(|_| TunnelError::Timeout(what))?
}

type Issued = Arc<Mutex<Option<String>>>;

async fn session_once(
    cfg: &Arc<ClientConfig>,
    connector: &TlsConnector,
    log: &Logger,
    events: &Option<mpsc::Sender<Event>>,
    cancel: &CancellationToken,
    issued: &Issued,
) -> SessionEnd {
    let lost = |reason: String| SessionEnd::Lost {
        reason,
        logged_in: false,
    };
    let tcp = match within(cfg.dial_timeout, "dial server", async {
        Ok(TcpStream::connect((cfg.server_addr.as_str(), cfg.server_port)).await?)
    })
    .await
    {
        Ok(t) => t,
        Err(e) => return lost(e.to_string()),
    };
    let _ = tcp.set_nodelay(true);
    let Ok(name) = ServerName::try_from(cfg.server_addr.clone()) else {
        return lost("invalid server address for TLS".into());
    };
    let tls = match within(cfg.handshake_timeout, "tls handshake", async {
        Ok(connector.clone().connect(name, tcp).await?)
    })
    .await
    {
        Ok(t) => t,
        Err(e) => return lost(e.to_string()),
    };
    let (session, mut inbound, driver) = mux::spawn(tls, false, cfg.mux);
    let end = talk(cfg, &session, &mut inbound, log, events, cancel, issued).await;
    driver.abort();
    end
}

fn login_msg(cfg: &ClientConfig, issued: &Issued) -> ClientMsg {
    let ts = unix_now();
    let mut login = Login {
        version: PROTOCOL_VERSION,
        user: cfg.user.clone(),
        client_id: String::new(),
        timestamp: ts,
        auth: String::new(),
        metas: Default::default(),
        enroll: None,
    };
    match &cfg.managed {
        None => {
            login.client_id = if cfg.client_id.is_empty() {
                random_id()
            } else {
                cfg.client_id.clone()
            };
            if !cfg.token.is_empty() {
                login.auth = auth_key(&cfg.token, ts);
            }
        }
        Some(m) => match (
            &m.client_id.clone().or_else(|| {
                issued
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            }),
            &m.enroll,
        ) {
            (Some(id), _) => {
                login.client_id = id.clone();
                login.auth = m.key.sign_login(id, ts);
            }
            (None, info) => {
                login.enroll = info.as_ref().map(|i| Enroll {
                    token: i.token.clone(),
                    name: i.name.clone(),
                    pubkey: m.key.public_hex(),
                    os: i.os.clone(),
                    arch: i.arch.clone(),
                    version: i.version.clone(),
                });
            }
        },
    }
    ClientMsg::Login(login)
}

/// 一个本地转发目标。
#[derive(Debug, Clone)]
struct Local {
    kind: String,
    host: String,
    port: u16,
    proxy_protocol: u8,
    /// 客户端侧限速（整个代理共享）。
    limiter: Option<Arc<pipe::Limiter>>,
    /// 本地插件；有插件时不连接本地端口。
    plugin: Option<Arc<crate::plugin::Plugin>>,
}

type Locals = Arc<Mutex<HashMap<String, Local>>>;

/// 一个被监控代理的探测参数。
type Watch = (String, rustfrp_core::wire::HealthSpec, String, u16);

/// 本次会话里的全部健康检查任务及其最近结果。
struct Monitors {
    running: HashMap<String, (Watch, CancellationToken)>,
    states: HashMap<String, bool>,
    tx: mpsc::Sender<(String, bool)>,
    parent: CancellationToken,
}

impl Monitors {
    /// 让运行中的任务与期望一致：多余 / 参数变化的停掉，新增的启动。
    fn sync(&mut self, wanted: Vec<Watch>) {
        self.running.retain(|name, (w, stop)| {
            let keep = wanted.contains(w);
            if !keep {
                stop.cancel();
                self.states.remove(name);
            }
            keep
        });
        for w in wanted {
            if self.running.contains_key(&w.0) {
                continue;
            }
            let stop = self.parent.child_token();
            crate::health::spawn(
                w.0.clone(),
                w.1.clone(),
                w.2.clone(),
                w.3,
                self.tx.clone(),
                stop.clone(),
            );
            self.running.insert(w.0.clone(), (w, stop));
        }
    }
}

async fn talk(
    cfg: &Arc<ClientConfig>,
    session: &Session,
    inbound: &mut mpsc::Receiver<Stream>,
    log: &Logger,
    events: &Option<mpsc::Sender<Event>>,
    cancel: &CancellationToken,
    issued: &Issued,
) -> SessionEnd {
    let lost = |reason: String, logged_in: bool| SessionEnd::Lost { reason, logged_in };
    let ctrl = match session.open().await {
        Ok(c) => c,
        Err(e) => return lost(e.to_string(), false),
    };
    let (mut rd, mut wr) = tokio::io::split(ctrl);
    if let Err(e) = write_frame(&mut wr, &login_msg(cfg, issued)).await {
        return lost(e.to_string(), false);
    }
    let managed = match within(
        cfg.handshake_timeout,
        "login response",
        read_frame::<_, ServerMsg>(&mut rd),
    )
    .await
    {
        Ok(ServerMsg::LoginResp {
            ok: true,
            run_id,
            client_id,
            managed,
            ..
        }) => {
            log.info(&format!("login to server success, run id [{run_id}]"));
            if !client_id.is_empty() {
                *issued.lock().unwrap_or_else(PoisonError::into_inner) = Some(client_id.clone());
                emit(events, Event::Enrolled { client_id });
            }
            managed
        }
        Ok(ServerMsg::LoginResp { error, .. }) => return SessionEnd::Rejected(error),
        Ok(_) => return lost("unexpected message in login".into(), false),
        Err(e) => return lost(e.to_string(), false),
    };
    emit(events, Event::Connected);

    // 独立模式：注册全部代理（顺序进行，逐个读应答）。受管模式由服务端下发配置，这里不注册。
    let locals: Locals = Arc::default();
    let own_proxies: &[ProxyCfg] = if managed { &[] } else { &cfg.proxies };
    let mut watches: Vec<Watch> = Vec::new();
    for p in own_proxies {
        let plugin = match p.plugin.clone().map(crate::plugin::Plugin::new).transpose() {
            Ok(pl) => pl.map(Arc::new),
            Err(error) => {
                log.error(&format!("proxy [{}] start error: {error}", p.name));
                emit(
                    events,
                    Event::ProxyFailed {
                        name: p.name.clone(),
                        error,
                    },
                );
                continue;
            }
        };
        let spec = ProxySpec {
            name: p.name.clone(),
            kind: p.kind.clone(),
            remote_port: p.remote_port,
            custom_domains: p.custom_domains.clone(),
            subdomain: p.subdomain.clone(),
            host_header_rewrite: p.host_header_rewrite.clone(),
            http: p.http.clone(),
            bandwidth_bps: p.bandwidth_bps,
            bandwidth_server: p.bandwidth_server,
            lb: p.lb.clone(),
            secret: p.secret.clone(),
        };
        if let Err(e) = write_frame(&mut wr, &ClientMsg::NewProxy(Box::new(spec))).await {
            return lost(e.to_string(), true);
        }
        match within(
            cfg.handshake_timeout,
            "new proxy response",
            read_frame::<_, ServerMsg>(&mut rd),
        )
        .await
        {
            Ok(ServerMsg::NewProxyResp {
                name,
                remote_port,
                error,
            }) if error.is_empty() => {
                locals
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(
                        name.clone(),
                        Local {
                            kind: local_kind(&p.kind),
                            host: p.local_ip.clone(),
                            port: p.local_port,
                            proxy_protocol: p.proxy_protocol,
                            limiter: client_limiter(p.bandwidth_bps, p.bandwidth_server),
                            plugin: plugin.clone(),
                        },
                    );
                if let Some(h) = &p.lb.health {
                    watches.push((name.clone(), h.clone(), p.local_ip.clone(), p.local_port));
                }
                emit(events, Event::ProxyStarted { name, remote_port });
            }
            Ok(ServerMsg::NewProxyResp { name, error, .. }) => {
                log.error(&format!("proxy [{name}] start error: {error}"));
                emit(events, Event::ProxyFailed { name, error });
            }
            Ok(_) => return lost("unexpected message while registering proxy".into(), true),
            Err(e) => return lost(e.to_string(), true),
        }
    }
    let (health_tx, mut health_rx) = mpsc::channel::<(String, bool)>(16);
    let mon_cancel = cancel.child_token();
    let mut monitors = Monitors {
        running: HashMap::new(),
        states: HashMap::new(),
        tx: health_tx,
        parent: mon_cancel.clone(),
    };
    monitors.sync(watches);
    let mut visitors = crate::visitor::Visitors::new(
        session.clone(),
        crate::visitor::VisitorEnv {
            user: cfg.user.clone(),
            open_timeout: cfg.handshake_timeout,
            udp_packet_size: cfg.udp_packet_size,
            udp_idle: cfg.udp_idle,
            traffic: cfg.traffic.clone(),
            log: log.clone(),
            events: events.clone(),
        },
        mon_cancel.clone(),
    );
    if !managed {
        visitors.sync(&cfg.visitors);
    }
    // 控制流读取放到独立任务（读帧不是取消安全的），主循环只看通道。
    let (pong_tx, mut pong_rx) = mpsc::channel::<Result<ServerMsg, TunnelError>>(4);
    let reader = tokio::spawn(async move {
        loop {
            let m = read_frame::<_, ServerMsg>(&mut rd).await;
            let stop = m.is_err();
            if pong_tx.send(m).await.is_err() || stop {
                break;
            }
        }
    });

    let mut tick = tokio::time::interval(cfg.heartbeat_interval);
    tick.tick().await;
    let mut deadline = tokio::time::Instant::now() + cfg.heartbeat_timeout;
    // 本次会话里第一次见到的重启计数作为基线：之后变大才表示有新的重启请求。
    let mut restart_baseline: Option<u64> = None;
    let end = loop {
        tokio::select! {
            _ = cancel.cancelled() => break SessionEnd::Cancelled,
            _ = tick.tick() => {
                if let Err(e) = write_frame(&mut wr, &ClientMsg::Ping).await {
                    break lost(e.to_string(), true);
                }
            }
            _ = tokio::time::sleep_until(deadline) => break lost("heartbeat timeout".into(), true),
            msg = pong_rx.recv() => match msg {
                Some(Ok(ServerMsg::Pong)) => deadline = tokio::time::Instant::now() + cfg.heartbeat_timeout,
                Some(Ok(ServerMsg::Config(set))) => {
                    match restart_baseline {
                        None => restart_baseline = Some(set.restart_seq),
                        Some(base) if set.restart_seq > base => {
                            restart_baseline = Some(set.restart_seq);
                            emit(events, Event::RestartRequested);
                        }
                        Some(_) => {}
                    }
                    if let Err(e) = crate::local_control::update_tunnels(&cfg.local_tunnels,&set) { break lost(e.into(),true); }
                    let map = set.proxies.iter().map(|p| (p.name.clone(), Local { kind: local_kind(&p.kind), host: p.local_ip.clone(), port: p.local_port, proxy_protocol: p.proxy_protocol, limiter: client_limiter(p.bandwidth_bps, p.bandwidth_server), plugin: None })).collect();
                    *locals.lock().unwrap_or_else(PoisonError::into_inner) = map;
                    visitors.sync(&set.visitors);
                    monitors.sync(
                        set.proxies
                            .iter()
                            .filter_map(|p| {
                                p.lb.health
                                    .clone()
                                    .map(|h| (p.name.clone(), h, p.local_ip.clone(), p.local_port))
                            })
                            .collect(),
                    );
                    emit(events, Event::ConfigApplied { revision: set.revision, proxies: set.proxies.len() });
                    if let Err(e) = write_frame(&mut wr, &ClientMsg::Applied { revision: set.revision }).await {
                        break lost(e.to_string(), true);
                    }
                    // Edge 可能因配置变化重启了代理（健康状态回到初始）：把已知结果重新告知。
                    let mut failed = None;
                    for (name, healthy) in &monitors.states {
                        if let Err(e) = write_frame(&mut wr, &ClientMsg::Health { name: name.clone(), healthy: *healthy }).await {
                            failed = Some(e);
                            break;
                        }
                    }
                    if let Some(e) = failed {
                        break lost(e.to_string(), true);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => break lost(e.to_string(), true),
                None => break lost("control stream closed".into(), true),
            },
            Some((name, healthy)) = health_rx.recv() => {
                monitors.states.insert(name.clone(), healthy);
                if let Err(e) = write_frame(&mut wr, &ClientMsg::Health { name, healthy }).await {
                    break lost(e.to_string(), true);
                }
            }
            stream = inbound.recv() => match stream {
                Some(s) => {
                    let (locals, cfg, log) = (locals.clone(), cfg.clone(), log.clone());
                    tokio::spawn(async move {
                        if let Err(e) = handle_work(s, &locals, &cfg).await {
                            log.debug(&format!("work connection: {e}"));
                        }
                    });
                }
                None => break lost("server closed the connection".into(), true),
            },
        }
    };
    reader.abort();
    mon_cancel.cancel();
    cfg.local_tunnels
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .1
        .clear();
    end
}

async fn handle_work(
    mut stream: Stream,
    locals: &Locals,
    cfg: &ClientConfig,
) -> Result<(), TunnelError> {
    let start: StartWork =
        within(cfg.handshake_timeout, "start work", read_frame(&mut stream)).await?;
    let target = locals
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&start.proxy)
        .cloned();
    let Some(t) = target else {
        return Err(TunnelError::Wire(format!(
            "unknown proxy [{}]",
            start.proxy
        )));
    };
    if t.kind == "udp" {
        return udp_work(stream, &t, cfg).await;
    }
    if let Some(plugin) = &t.plugin {
        return match t.limiter {
            Some(l) => {
                plugin
                    .serve(
                        pipe::Limited::both(stream, l),
                        &cfg.traffic,
                        cfg.local_dial_timeout,
                    )
                    .await
            }
            None => {
                plugin
                    .serve(stream, &cfg.traffic, cfg.local_dial_timeout)
                    .await
            }
        };
    }
    let mut local = within(cfg.local_dial_timeout, "dial local service", async {
        Ok(TcpStream::connect((t.host.as_str(), t.port)).await?)
    })
    .await?;
    let _ = local.set_nodelay(true);
    // PROXY protocol：把访问者的真实地址交给内网服务（头不计入流量）。
    if let Some(h) = rustfrp_core::proxyproto::header(t.proxy_protocol, &start.src, &start.dst) {
        tokio::io::AsyncWriteExt::write_all(&mut local, &h).await?;
    }
    match t.limiter {
        Some(l) => {
            let mut s = pipe::Limited::new(stream, l.clone());
            let mut loc = pipe::Limited::new(local, l);
            pipe::relay_counted(&mut s, &mut loc, &cfg.traffic).await?;
        }
        None => {
            pipe::relay_counted(&mut stream, &mut local, &cfg.traffic).await?;
        }
    }
    Ok(())
}

/// 本地转发的类型：sudp 的数据在流上就是 udp 帧，和 udp 代理一样处理。
fn local_kind(kind: &str) -> String {
    if kind == "sudp" {
        "udp".into()
    } else {
        kind.into()
    }
}

/// 客户端侧限速器：服务端限速的代理不在这里限。
fn client_limiter(bps: u64, server_side: bool) -> Option<Arc<pipe::Limiter>> {
    if server_side {
        None
    } else {
        pipe::Limiter::new(bps)
    }
}

/// UDP：一条流对应一个访问者；数据报在流上用「长度 + 载荷」封装，本地一侧是一个已 connect 的 UDP 套接字。
async fn udp_work(stream: Stream, t: &Local, cfg: &ClientConfig) -> Result<(), TunnelError> {
    let target = tokio::net::lookup_host((t.host.as_str(), t.port))
        .await?
        .next()
        .ok_or_else(|| TunnelError::Wire(format!("cannot resolve {}", t.host)))?;
    let bind = if target.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let sock = Arc::new(UdpSocket::bind(bind).await?);
    sock.connect(target).await?;
    let _connection = cfg.traffic.connection();
    let traffic = cfg.traffic.clone();
    let (mut rd, mut wr) = tokio::io::split(stream);
    let out = sock.clone();
    let mut upstream = tokio::spawn(async move {
        let mut buf = Vec::new();
        while let Ok(Some(_)) = read_datagram(&mut rd, &mut buf).await {
            if let Ok(n) = out.send(&buf).await {
                traffic.record_in(n as u64);
            }
        }
    });
    let mut buf = vec![0u8; cfg.udp_packet_size.max(512)];
    let result = loop {
        tokio::select! {
            _ = &mut upstream => break Ok(()),
            _ = tokio::time::sleep(cfg.udp_idle) => break Ok(()),
            r = sock.recv(&mut buf) => {
                // 本地端口不可达会让 connect 过的 UDP 套接字报 ECONNREFUSED：忽略，继续等待。
                if let Ok(n) = r {
                    if let Err(e) = write_datagram(&mut wr, buf.get(..n).unwrap_or(&[])).await {
                        break Err(e);
                    }
                    cfg.traffic.record_out(n as u64);
                }
            }
        }
    };
    upstream.abort();
    result
}
