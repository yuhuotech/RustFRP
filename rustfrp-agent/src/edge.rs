//! 边缘节点（frps 角色）：监听 Client 的隧道连接，并在公网端口上转发访问者流量。
//!
//! 两种工作方式由 [`Policy`] 决定：
//! * **独立模式**（[`TokenPolicy`]，等价 frps）：客户端自己用 `NewProxy` 注册代理；
//! * **受管模式**：由 Controller 的策略放行客户端，并通过 `watch` 通道下发整份配置，Edge 对账后增删代理。

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;
use std::time::Duration;

use rustfrp_core::auth::{self, random_id};
use rustfrp_core::error::TunnelError;
use rustfrp_core::mux::{self, MuxConfig, Session, Stream};
use rustfrp_core::pipe;
use rustfrp_core::tls::{self, Identity};
use rustfrp_core::vhost::proxy_hosts;
use rustfrp_core::wire::{
    read_frame, write_frame, ClientMsg, ConfigSet, Login, ManagedProxy, ProxySpec, ServerMsg,
    StartWork, PROTOCOL_VERSION,
};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::log::Logger;
use crate::proxy_rt::{serve_vhost, udp_proxy, HttpRoute, Route, Routes, UdpOpts, VhostOpts};

/// 装箱的异步返回值。
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 代理运行状态变化（受管模式下回报给 Controller）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyState {
    /// 已在该端口监听。
    Running(u16),
    /// 启动失败。
    Failed(String),
    /// 已停止（被移除或客户端断开）。
    Stopped,
}

/// 一条代理状态事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEvent {
    /// 设备 ID。
    pub client_id: String,
    /// 代理名。
    pub name: String,
    /// 新状态。
    pub state: ProxyState,
}

/// 受管客户端的准入结果。
pub struct Managed {
    /// 设备 ID。
    pub client_id: String,
    /// 本次是否刚完成接入（需要把 `client_id` 告诉客户端）。
    pub just_enrolled: bool,
    /// 配置通道：每次变更都会收到最新的一整份配置。
    pub config: watch::Receiver<Arc<ConfigSet>>,
    /// 代理状态事件回报。
    pub events: mpsc::Sender<ProxyEvent>,
    /// 存活凭据：会话结束时被丢弃（Controller 据此把设备标为离线）。
    pub guard: Box<dyn Any + Send>,
}

/// 登录准入结果。
pub enum Admission {
    /// 独立模式：客户端自己注册代理。
    Standalone,
    /// 受管模式。
    Managed(Managed),
}

/// 登录策略。
pub trait Policy: Send + Sync + 'static {
    /// 判断是否放行该登录；`Err` 的内容会原样返回给客户端。
    fn login<'a>(&'a self, login: &'a Login) -> BoxFut<'a, Result<Admission, String>>;
}

/// 与 frps 一致的令牌策略；令牌为空表示不校验。
pub struct TokenPolicy {
    /// 令牌。
    pub token: String,
}

impl Policy for TokenPolicy {
    fn login<'a>(&'a self, login: &'a Login) -> BoxFut<'a, Result<Admission, String>> {
        Box::pin(async move {
            if !self.token.is_empty()
                && !auth::verify(&self.token, login.timestamp, &login.auth, auth::unix_now())
            {
                return Err("token in login doesn't match token from configuration".into());
            }
            Ok(Admission::Standalone)
        })
    }
}

/// 边缘节点运行参数。
#[derive(Debug, Clone)]
pub struct EdgeConfig {
    /// Client 连接的监听地址。
    pub bind_addr: String,
    /// Client 连接的监听端口（0 = 系统分配，测试用）。
    pub bind_port: u16,
    /// 代理端口绑定地址。
    pub proxy_bind_addr: String,
    /// 鉴权令牌，空表示不校验（与 frp 一致）。
    pub token: String,
    /// 允许的远程端口区间（含端点）；空 = 不限制。
    pub allow_ports: Vec<(u16, u16)>,
    /// 单客户端最大端口数，0 = 不限制。
    pub max_ports_per_client: usize,
    /// 等待客户端登录等握手步骤的超时。
    pub handshake_timeout: Duration,
    /// 控制流上多久收不到消息就判定客户端失联。
    pub heartbeat_timeout: Duration,
    /// 等待向客户端开流的超时（frp 的 `userConnTimeout`）。
    pub user_conn_timeout: Duration,
    /// 多路复用参数。
    pub mux: MuxConfig,
    /// HTTP 虚拟主机端口（`None` = 不启用，`Some(0)` = 系统分配，测试用）。
    pub vhost_http_port: Option<u16>,
    /// HTTPS（SNI 透传）虚拟主机端口。
    pub vhost_https_port: Option<u16>,
    /// 子域名后缀（frp 的 `subDomainHost`）。
    pub sub_domain_host: String,
    /// 等待请求头的超时。
    pub vhost_head_timeout: Duration,
    /// 每个 UDP 代理同时跟踪的访问者上限。
    pub udp_max_peers: usize,
    /// UDP 访问者空闲回收时间。
    pub udp_idle: Duration,
    /// UDP 接收缓冲（frp 的 `udpPacketSize`）。
    pub udp_packet_size: usize,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0".into(),
            bind_port: 7000,
            proxy_bind_addr: "0.0.0.0".into(),
            token: String::new(),
            allow_ports: Vec::new(),
            max_ports_per_client: 0,
            handshake_timeout: Duration::from_secs(10),
            heartbeat_timeout: Duration::from_secs(90),
            user_conn_timeout: Duration::from_secs(10),
            mux: MuxConfig::default(),
            vhost_http_port: None,
            vhost_https_port: None,
            sub_domain_host: String::new(),
            vhost_head_timeout: Duration::from_secs(10),
            udp_max_peers: 1024,
            udp_idle: Duration::from_secs(60),
            udp_packet_size: 1500,
        }
    }
}

impl EdgeConfig {
    fn port_allowed(&self, port: u16) -> bool {
        self.allow_ports.is_empty()
            || self
                .allow_ports
                .iter()
                .any(|(lo, hi)| (*lo..=*hi).contains(&port))
    }
}

/// 已绑定、待运行的边缘节点。
pub struct Edge {
    listener: TcpListener,
    http_listener: Option<TcpListener>,
    https_listener: Option<TcpListener>,
    acceptor: TlsAcceptor,
    cfg: Arc<EdgeConfig>,
    policy: Arc<dyn Policy>,
    log: Logger,
    traffic: Arc<pipe::Traffic>,
    metrics: Arc<crate::metrics::Metrics>,
    retain_metrics: bool,
    fingerprint: String,
}

impl Edge {
    /// 绑定监听端口（独立模式，令牌取自 `cfg.token`）。`identity` 为空时生成自签名证书。
    pub async fn bind(
        cfg: EdgeConfig,
        identity: Option<Identity>,
        log: Logger,
    ) -> Result<Edge, TunnelError> {
        let policy = Arc::new(TokenPolicy {
            token: cfg.token.clone(),
        });
        let mut edge = Self::bind_with_policy(cfg, identity, policy, log).await?;
        edge.retain_metrics = false;
        Ok(edge)
    }

    /// 绑定监听端口并使用自定义登录策略。
    pub async fn bind_with_policy(
        cfg: EdgeConfig,
        identity: Option<Identity>,
        policy: Arc<dyn Policy>,
        log: Logger,
    ) -> Result<Edge, TunnelError> {
        let id = match identity {
            Some(i) => i,
            None => tls::self_signed(vec!["rustfrp".into()])?,
        };
        let fingerprint = id
            .certs
            .first()
            .map(tls::certificate_fingerprint)
            .unwrap_or_default();
        let acceptor = TlsAcceptor::from(tls::server_config(id)?);
        let listener = TcpListener::bind((cfg.bind_addr.as_str(), cfg.bind_port)).await?;
        let vhost = |port: Option<u16>| {
            let addr = cfg.proxy_bind_addr.clone();
            async move {
                match port {
                    Some(p) => Ok(Some(TcpListener::bind((addr.as_str(), p)).await?)),
                    None => Ok::<_, TunnelError>(None),
                }
            }
        };
        let http_listener = vhost(cfg.vhost_http_port).await?;
        let https_listener = vhost(cfg.vhost_https_port).await?;
        let metrics = Arc::new(crate::metrics::Metrics::default());
        Ok(Edge {
            listener,
            http_listener,
            https_listener,
            acceptor,
            cfg: Arc::new(cfg),
            policy,
            log,
            traffic: metrics.total(),
            metrics,
            retain_metrics: true,
            fingerprint,
        })
    }

    /// 是否保留已停止代理的计数条目直到被确认回收（受管 Controller 采样用）；节点本身不采样时应关闭。
    pub fn retain_metrics(mut self, retain: bool) -> Self {
        self.retain_metrics = retain;
        self
    }

    /// 数据面实时总计数。
    pub fn traffic(&self) -> Arc<pipe::Traffic> {
        self.traffic.clone()
    }

    /// 服务端叶证书指纹，供可信控制面生成固定证书接入命令。
    pub fn certificate_fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// 按代理采样注册表。
    pub fn metrics(&self) -> Arc<crate::metrics::Metrics> {
        self.metrics.clone()
    }

    /// 实际监听地址。
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// HTTP 虚拟主机的实际监听地址。
    pub fn http_addr(&self) -> Option<SocketAddr> {
        self.http_listener
            .as_ref()
            .and_then(|l| l.local_addr().ok())
    }

    /// HTTPS 虚拟主机的实际监听地址。
    pub fn https_addr(&self) -> Option<SocketAddr> {
        self.https_listener
            .as_ref()
            .and_then(|l| l.local_addr().ok())
    }

    /// 运行直到 `shutdown` 被取消。
    pub async fn serve(self, shutdown: CancellationToken) {
        let names = self.metrics.clone();
        let mut sweep = tokio::time::interval(Duration::from_secs(5));
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let routes = Arc::new(Routes::default());
        let groups = Arc::new(TcpGroups::new(shutdown.clone()));
        let vopts = VhostOpts {
            head_timeout: self.cfg.vhost_head_timeout,
            open_timeout: self.cfg.user_conn_timeout,
        };
        for (listener, https) in [(self.http_listener, false), (self.https_listener, true)] {
            if let Some(l) = listener {
                tokio::spawn(serve_vhost(
                    l,
                    https,
                    routes.clone(),
                    vopts.clone(),
                    self.log.clone(),
                    shutdown.clone(),
                ));
            }
        }
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = sweep.tick(), if !self.retain_metrics => { names.sweep_stopped(); },
                accepted = self.listener.accept() => {
                    let Ok((tcp, peer)) = accepted else { continue };
                    let ctx = ClientCtx {
                        cfg: self.cfg.clone(),
                        acceptor: self.acceptor.clone(),
                        policy: self.policy.clone(),
                        names: names.clone(),
                        routes: routes.clone(),
                        groups: groups.clone(),
                        traffic: self.traffic.clone(),
                        limiter: None,
                        log: self.log.clone(),
                        cancel: shutdown.child_token(),
                    };
                    tokio::spawn(async move {
                        if let Err(e) = handle_client(tcp, ctx.clone()).await {
                            ctx.log.debug(&format!("client {peer}: {e}"));
                        }
                    });
                }
            }
        }
    }
}

#[derive(Clone)]
struct ClientCtx {
    cfg: Arc<EdgeConfig>,
    acceptor: TlsAcceptor,
    policy: Arc<dyn Policy>,
    names: Arc<crate::metrics::Metrics>,
    routes: Arc<Routes>,
    groups: Arc<TcpGroups>,
    traffic: Arc<pipe::Traffic>,
    /// 服务端限速（`bandwidthLimitMode = "server"`）；`None` = 不限。
    limiter: Option<Arc<pipe::Limiter>>,
    log: Logger,
    cancel: CancellationToken,
}

async fn within<T>(
    d: Duration,
    what: &'static str,
    f: impl Future<Output = Result<T, TunnelError>>,
) -> Result<T, TunnelError> {
    timeout(d, f)
        .await
        .map_err(|_| TunnelError::Timeout(what))?
}

async fn handle_client(tcp: TcpStream, ctx: ClientCtx) -> Result<(), TunnelError> {
    let _ = tcp.set_nodelay(true);
    let tls = within(ctx.cfg.handshake_timeout, "tls handshake", async {
        Ok(ctx.acceptor.accept(tcp).await?)
    })
    .await?;
    let (session, mut inbound, driver) = mux::spawn(tls, true, ctx.cfg.mux);
    let result = control_loop(&session, &mut inbound, &ctx).await;
    ctx.cancel.cancel();
    // 先让驱动把已排队的帧（例如登录失败的原因）发完再关闭，超时才强制终止。
    drop(session);
    drop(inbound);
    let mut driver = driver;
    if timeout(Duration::from_secs(2), &mut driver).await.is_err() {
        driver.abort();
    }
    result
}

async fn control_loop(
    session: &Session,
    inbound: &mut mpsc::Receiver<Stream>,
    ctx: &ClientCtx,
) -> Result<(), TunnelError> {
    let ctrl = within(ctx.cfg.handshake_timeout, "control stream", async {
        inbound.recv().await.ok_or(TunnelError::Closed)
    })
    .await?;
    let (mut rd, mut wr) = tokio::io::split(ctrl);
    let first: ClientMsg = within(ctx.cfg.handshake_timeout, "login", read_frame(&mut rd)).await?;
    let ClientMsg::Login(login) = first else {
        return Err(TunnelError::Wire("expected login".into()));
    };
    let admission = if login.version != PROTOCOL_VERSION {
        Err(format!("unsupported protocol version {}", login.version))
    } else {
        ctx.policy.login(&login).await
    };
    let reject = |error: String| ServerMsg::LoginResp {
        ok: false,
        error,
        run_id: String::new(),
        client_id: String::new(),
        managed: false,
    };
    let admission = match admission {
        Ok(a) => a,
        Err(error) => {
            write_frame(&mut wr, &reject(error.clone())).await?;
            return Err(TunnelError::Auth(error));
        }
    };
    let run_id = random_id();
    let (client_id, managed) = match &admission {
        Admission::Standalone => (String::new(), false),
        Admission::Managed(m) => (
            if m.just_enrolled {
                m.client_id.clone()
            } else {
                String::new()
            },
            true,
        ),
    };
    write_frame(
        &mut wr,
        &ServerMsg::LoginResp {
            ok: true,
            error: String::new(),
            run_id: run_id.clone(),
            client_id,
            managed,
        },
    )
    .await?;
    ctx.log.info(&format!(
        "client [{}] login success, run id [{run_id}]",
        login.client_id
    ));

    let reader = spawn_reader(rd);
    match admission {
        Admission::Standalone => serve_standalone(session, reader, &mut wr, ctx).await,
        Admission::Managed(m) => serve_managed(session, reader, &mut wr, ctx, m).await,
    }
}

type Reader = mpsc::Receiver<Result<ClientMsg, TunnelError>>;

/// 读帧不是取消安全的，所以放到独立任务里，主循环只看通道。
fn spawn_reader(mut rd: ReadHalf<Stream>) -> Reader {
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        loop {
            let m = read_frame::<_, ClientMsg>(&mut rd).await;
            let stop = m.is_err();
            if tx.send(m).await.is_err() || stop {
                break;
            }
        }
    });
    rx
}

/// 取下一条控制消息，带心跳超时。通道关闭视为连接结束。
async fn next_msg(reader: &mut Reader, ctx: &ClientCtx) -> Result<ClientMsg, TunnelError> {
    match timeout(ctx.cfg.heartbeat_timeout, reader.recv()).await {
        Err(_) => Err(TunnelError::Timeout("heartbeat")),
        Ok(None) => Err(TunnelError::Closed),
        Ok(Some(m)) => m,
    }
}

async fn serve_standalone(
    session: &Session,
    mut reader: Reader,
    wr: &mut WriteHalf<Stream>,
    ctx: &ClientCtx,
) -> Result<(), TunnelError> {
    let mut owned: Vec<String> = Vec::new();
    let mut ups: HashMap<String, Arc<AtomicBool>> = HashMap::new();
    let result = async {
        loop {
            let msg = tokio::select! {
                _ = ctx.cancel.cancelled() => return Ok(()),
                m = next_msg(&mut reader, ctx) => m?,
            };
            match msg {
                ClientMsg::Ping => write_frame(wr, &ServerMsg::Pong).await?,
                ClientMsg::NewProxy(spec) => {
                    let started = if ctx.cfg.max_ports_per_client > 0
                        && owned.len() >= ctx.cfg.max_ports_per_client
                    {
                        Err("exceed the max_ports_per_client".to_string())
                    } else {
                        activate(session, &ProxyDef::from(&spec), ctx)
                            .await
                            .map(|s| {
                                ups.insert(spec.name.clone(), s.up);
                                s.port
                            })
                    };
                    if started.is_ok() {
                        owned.push(spec.name.clone());
                    }
                    let resp = match started {
                        Ok(port) => ServerMsg::NewProxyResp {
                            name: spec.name,
                            remote_port: port,
                            error: String::new(),
                        },
                        Err(error) => {
                            ctx.log
                                .warn(&format!("proxy [{}] rejected: {error}", spec.name));
                            ServerMsg::NewProxyResp {
                                name: spec.name,
                                remote_port: 0,
                                error,
                            }
                        }
                    };
                    write_frame(wr, &resp).await?;
                }
                ClientMsg::Applied { .. } => {}
                ClientMsg::Health { name, healthy } => {
                    if let Some(up) = ups.get(&name) {
                        if up.swap(healthy, Relaxed) != healthy {
                            ctx.log.info(&format!(
                                "proxy [{name}] is {}",
                                if healthy { "healthy" } else { "unhealthy" }
                            ));
                        }
                    }
                }
                ClientMsg::Login(_) => return Err(TunnelError::Wire("duplicate login".into())),
            }
        }
    }
    .await;
    for name in &owned {
        ctx.names.release(name);
    }
    result
}

/// 受管代理的运行记录。
struct Running {
    spec: ManagedProxy,
    stop: CancellationToken,
    port: u16,
    up: Arc<AtomicBool>,
}

async fn serve_managed(
    session: &Session,
    mut reader: Reader,
    wr: &mut WriteHalf<Stream>,
    ctx: &ClientCtx,
    m: Managed,
) -> Result<(), TunnelError> {
    let Managed {
        client_id,
        config,
        events,
        guard,
        ..
    } = m;
    let mut config = config;
    let mut running: HashMap<String, Running> = HashMap::new();
    let emit = |name: &str, state: ProxyState| {
        let _ = events.try_send(ProxyEvent {
            client_id: client_id.clone(),
            name: name.to_string(),
            state,
        });
    };

    let result = async {
        // 先应用并下发当前配置，之后每次变更都重新对账。
        let mut current = config.borrow_and_update().clone();
        loop {
            reconcile(session, &current, &mut running, ctx, &emit).await;
            write_frame(wr, &ServerMsg::Config((*current).clone())).await?;
            loop {
                tokio::select! {
                    _ = ctx.cancel.cancelled() => return Ok(()),
                    changed = config.changed() => {
                        changed.map_err(|_| TunnelError::Closed)?;
                        current = config.borrow_and_update().clone();
                        break;
                    }
                    msg = next_msg(&mut reader, ctx) => match msg? {
                        ClientMsg::Ping => write_frame(wr, &ServerMsg::Pong).await?,
                        ClientMsg::Applied { revision } => ctx.log.debug(&format!("client [{client_id}] applied revision {revision}")),
                        ClientMsg::Health { name, healthy } => {
                            if let Some(r) = running.get(&name) {
                                if r.up.swap(healthy, Relaxed) != healthy {
                                    ctx.log.info(&format!("proxy [{name}] is {}", if healthy { "healthy" } else { "unhealthy" }));
                                    emit(&name, if healthy { ProxyState::Running(r.port) } else { ProxyState::Failed("health check failed".into()) });
                                }
                            }
                        }
                        ClientMsg::NewProxy(_) | ClientMsg::Login(_) => {
                            return Err(TunnelError::Wire("unexpected message for a managed client".into()));
                        }
                    },
                }
            }
        }
    }
    .await;

    for (name, r) in running.drain() {
        r.stop.cancel();
        ctx.names.release(&name);
        emit(&name, ProxyState::Stopped);
    }
    drop(guard);
    result
}

/// 让实际运行的代理与期望配置一致：停掉多余 / 已变更的，启动新增 / 已变更的。
async fn reconcile(
    session: &Session,
    desired: &ConfigSet,
    running: &mut HashMap<String, Running>,
    ctx: &ClientCtx,
    emit: &impl Fn(&str, ProxyState),
) {
    let wanted: HashMap<&str, &ManagedProxy> = desired
        .proxies
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();
    let stale: Vec<String> = running
        .iter()
        .filter(|(name, r)| {
            wanted
                .get(name.as_str())
                .is_none_or(|w| !same_listener(&r.spec, w))
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in stale {
        if let Some(r) = running.remove(&name) {
            r.stop.cancel();
            // 路由的异步清理可能晚于下面的重新登记：先同步摘掉，避免与自己的旧路由冲突。
            ctx.routes.remove_proxy(&name);
            ctx.names.release(&name);
            emit(&name, ProxyState::Stopped);
        }
    }
    for p in &desired.proxies {
        if let Some(r) = running.get_mut(&p.name) {
            // 只有本地目标变化时无需重启监听，由客户端侧更新映射即可。
            r.spec = p.clone();
            continue;
        }
        match activate(session, &ProxyDef::from(p), ctx).await {
            Ok(Started { port, stop, up }) => {
                running.insert(
                    p.name.clone(),
                    Running {
                        spec: p.clone(),
                        stop,
                        port,
                        up,
                    },
                );
                // 启用健康检查的代理要等第一次探测成功才接收访问者：如实回报，而不是先报「运行中」。
                if p.lb.health.is_some() {
                    emit(
                        &p.name,
                        ProxyState::Failed("waiting for the first health check".into()),
                    );
                } else {
                    emit(&p.name, ProxyState::Running(port));
                }
            }
            Err(e) => {
                ctx.log.warn(&format!("proxy [{}] failed: {e}", p.name));
                emit(&p.name, ProxyState::Failed(e));
            }
        }
    }
}

/// 统一的代理定义：独立模式（`ProxySpec`）与受管模式（`ManagedProxy`）都转换成它再启动。
struct ProxyDef {
    name: String,
    kind: String,
    remote_port: u16,
    custom_domains: Vec<String>,
    subdomain: String,
    host_header_rewrite: String,
    http: rustfrp_core::wire::HttpOpts,
    bandwidth_bps: u64,
    bandwidth_server: bool,
    lb: rustfrp_core::wire::LbOpts,
}

impl From<&ProxySpec> for ProxyDef {
    fn from(p: &ProxySpec) -> Self {
        Self {
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
        }
    }
}

impl From<&ManagedProxy> for ProxyDef {
    fn from(p: &ManagedProxy) -> Self {
        Self {
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
        }
    }
}

/// 两份受管定义在「监听 / 路由」层面是否等价（只有本地目标变了则无需重启监听）。
fn same_listener(a: &ManagedProxy, b: &ManagedProxy) -> bool {
    a.kind == b.kind
        && a.remote_port == b.remote_port
        && a.custom_domains == b.custom_domains
        && a.subdomain == b.subdomain
        && a.host_header_rewrite == b.host_header_rewrite
        && a.http == b.http
        && a.bandwidth_server == b.bandwidth_server
        && a.lb.group == b.lb.group
        && a.lb.group_key == b.lb.group_key
        && a.lb.health.is_some() == b.lb.health.is_some()
        && (!a.bandwidth_server || a.bandwidth_bps == b.bandwidth_bps)
}

/// 启动一个代理（tcp / udp / http / https）。成功返回（对外端口, 停止令牌），失败返回给客户端看的原因。
async fn activate(session: &Session, def: &ProxyDef, ctx: &ClientCtx) -> Result<Started, String> {
    if !["tcp", "udp", "http", "https"].contains(&def.kind.as_str()) {
        return Err(format!("proxy type [{}] is not supported yet", def.kind));
    }
    if !ctx.names.claim(&def.name) {
        return Err(format!(
            "proxy [{}] already exists or metrics capacity reached",
            def.name
        ));
    }
    let stop = ctx.cancel.child_token();
    // 启用健康检查的代理在第一次探测成功前不接收访问者。
    let up = Arc::new(AtomicBool::new(def.lb.health.is_none()));
    match launch(session, def, ctx, &stop, &up).await {
        Ok(port) => {
            ctx.log.info(&format!(
                "proxy [{}] ({}) start, port {port}",
                def.name, def.kind
            ));
            Ok(Started { port, stop, up })
        }
        Err(e) => {
            ctx.names.release(&def.name);
            stop.cancel();
            Err(e)
        }
    }
}

/// 一个已启动的代理。
struct Started {
    port: u16,
    stop: CancellationToken,
    /// 健康标志，由客户端的 `Health` 消息更新。
    up: Arc<AtomicBool>,
}

async fn launch(
    session: &Session,
    def: &ProxyDef,
    ctx: &ClientCtx,
    stop: &CancellationToken,
    up: &Arc<AtomicBool>,
) -> Result<u16, String> {
    if !def.lb.group.is_empty() && def.kind == "udp" {
        return Err("udp proxies don't support load balancer groups".into());
    }
    if def.lb.health.is_some() && def.kind == "udp" {
        return Err("udp proxies don't support health checks".into());
    }
    let sub = ClientCtx {
        traffic: ctx
            .names
            .counter(&def.name)
            .ok_or_else(|| "proxy metrics missing".to_string())?,
        cancel: stop.clone(),
        limiter: if def.bandwidth_server {
            pipe::Limiter::new(def.bandwidth_bps)
        } else {
            None
        },
        ..ctx.clone()
    };
    match def.kind.as_str() {
        "tcp" => {
            let member = Member {
                id: 0,
                proxy: def.name.clone(),
                session: session.clone(),
                traffic: sub.traffic.clone(),
                limiter: sub.limiter.clone(),
                up: up.clone(),
            };
            if !def.lb.group.is_empty() {
                return ctx
                    .groups
                    .join(
                        &def.lb.group,
                        &def.lb.group_key,
                        def.remote_port,
                        member,
                        stop,
                        ctx,
                    )
                    .await;
            }
            let (listener, port) = bind_proxy_port(def.remote_port, &ctx.cfg).await?;
            tokio::spawn(accept_visitors(listener, member, sub));
            Ok(port)
        }
        "udp" => {
            let sock = bind_udp_port(def.remote_port, &ctx.cfg).await?;
            let port = sock.local_addr().map_or(def.remote_port, |a| a.port());
            let opts = UdpOpts {
                max_peers: ctx.cfg.udp_max_peers,
                idle: ctx.cfg.udp_idle,
                packet_size: ctx.cfg.udp_packet_size,
                open_timeout: ctx.cfg.user_conn_timeout,
                traffic: sub.traffic.clone(),
            };
            tokio::spawn(udp_proxy(
                sock,
                def.name.clone(),
                session.clone(),
                opts,
                ctx.log.clone(),
                stop.clone(),
            ));
            Ok(port)
        }
        kind => {
            let https = kind == "https";
            let (enabled, label) = if https {
                (ctx.cfg.vhost_https_port.is_some(), "https")
            } else {
                (ctx.cfg.vhost_http_port.is_some(), "http")
            };
            if !enabled {
                return Err(format!(
                    "type [{label}] not supported when vhost {label} port is not set"
                ));
            }
            if !def.subdomain.is_empty() && ctx.cfg.sub_domain_host.is_empty() {
                return Err(
                    "subdomain is not supported because this feature is not enabled in server"
                        .into(),
                );
            }
            let hosts = proxy_hosts(
                &def.custom_domains,
                &def.subdomain,
                &ctx.cfg.sub_domain_host,
            );
            if hosts.is_empty() {
                return Err("subdomain and custom domains should not be both empty".into());
            }
            let route = Route {
                id: 0,
                proxy: def.name.clone(),
                session: session.clone(),
                rewrite: def.host_header_rewrite.clone(),
                traffic: sub.traffic.clone(),
                limiter: sub.limiter.clone(),
                http: Arc::new(if https {
                    HttpRoute::default()
                } else {
                    HttpRoute::from_opts(&def.http)?
                }),
                group: def.lb.group.clone(),
                group_key: def.lb.group_key.clone(),
                up: up.clone(),
            };
            let route_id = ctx.routes.add(https, &hosts, &route)?;
            let (routes, stop) = (ctx.routes.clone(), stop.clone());
            tokio::spawn(async move {
                stop.cancelled().await;
                routes.remove(route_id);
            });
            Ok(if https {
                ctx.cfg.vhost_https_port
            } else {
                ctx.cfg.vhost_http_port
            }
            .unwrap_or(0))
        }
    }
}

/// 绑定代理端口，返回监听器与实际端口。
async fn bind_proxy_port(requested: u16, cfg: &EdgeConfig) -> Result<(TcpListener, u16), String> {
    let addr = cfg.proxy_bind_addr.as_str();
    let listener = if requested != 0 {
        if !cfg.port_allowed(requested) {
            return Err(format!("port not allowed: {requested}"));
        }
        TcpListener::bind((addr, requested))
            .await
            .map_err(|e| format!("listen on port {requested} failed: {e}"))?
    } else if cfg.allow_ports.is_empty() {
        TcpListener::bind((addr, 0))
            .await
            .map_err(|e| format!("listen failed: {e}"))?
    } else {
        first_free_port(addr, &cfg.allow_ports)
            .await
            .ok_or("no available port in allowPorts")?
    };
    let port = listener.local_addr().map_or(requested, |a| a.port());
    Ok((listener, port))
}

async fn bind_udp_port(requested: u16, cfg: &EdgeConfig) -> Result<UdpSocket, String> {
    let addr = cfg.proxy_bind_addr.as_str();
    if requested != 0 && !cfg.port_allowed(requested) {
        return Err(format!("port not allowed: {requested}"));
    }
    if requested != 0 || cfg.allow_ports.is_empty() {
        return UdpSocket::bind((addr, requested))
            .await
            .map_err(|e| format!("listen on udp port {requested} failed: {e}"));
    }
    for (lo, hi) in &cfg.allow_ports {
        for port in *lo..=*hi {
            if let Ok(s) = UdpSocket::bind((addr, port)).await {
                return Ok(s);
            }
        }
    }
    Err("no available port in allowPorts".into())
}

async fn first_free_port(addr: &str, ranges: &[(u16, u16)]) -> Option<TcpListener> {
    for (lo, hi) in ranges {
        for port in *lo..=*hi {
            if let Ok(l) = TcpListener::bind((addr, port)).await {
                return Some(l);
            }
        }
    }
    None
}

/// 一个 tcp 代理的转发目标（组成员或独立代理）。
#[derive(Clone)]
struct Member {
    id: u64,
    proxy: String,
    session: Session,
    traffic: Arc<pipe::Traffic>,
    limiter: Option<Arc<pipe::Limiter>>,
    /// 健康（可接收访问者）。
    up: Arc<AtomicBool>,
}

/// tcp 负载均衡组：同组成员共用一个公网端口，连接在健康成员间轮转。
struct TcpGroup {
    key: String,
    port: u16,
    members: Vec<Member>,
    stop: CancellationToken,
}

/// Edge 上的全部 tcp 组。
struct TcpGroups {
    groups: tokio::sync::Mutex<HashMap<String, TcpGroup>>,
    next_id: AtomicU64,
    rr: AtomicUsize,
    shutdown: CancellationToken,
}

impl TcpGroups {
    fn new(shutdown: CancellationToken) -> Self {
        Self {
            groups: Default::default(),
            next_id: AtomicU64::new(1),
            rr: AtomicUsize::new(0),
            shutdown,
        }
    }

    /// 加入（必要时创建）一个组，返回组的公网端口。成员在 `stop` 取消时自动离开；最后一个成员离开则关闭监听。
    async fn join(
        self: &Arc<Self>,
        group: &str,
        key: &str,
        requested: u16,
        mut member: Member,
        stop: &CancellationToken,
        ctx: &ClientCtx,
    ) -> Result<u16, String> {
        let mut groups = self.groups.lock().await;
        member.id = self.next_id.fetch_add(1, Relaxed);
        let id = member.id;
        let port = if let Some(g) = groups.get_mut(group) {
            if g.key != key {
                return Err(format!("group [{group}] key doesn't match"));
            }
            if requested != 0 && requested != g.port {
                return Err(format!("group [{group}] port not match"));
            }
            g.members.push(member);
            g.port
        } else {
            let (listener, port) = bind_proxy_port(requested, &ctx.cfg).await?;
            let token = self.shutdown.child_token();
            groups.insert(
                group.to_string(),
                TcpGroup {
                    key: key.to_string(),
                    port,
                    members: vec![member],
                    stop: token.clone(),
                },
            );
            tokio::spawn(accept_group(
                listener,
                group.to_string(),
                self.clone(),
                token,
                ctx.clone(),
            ));
            port
        };
        drop(groups);
        let (this, group, stop) = (self.clone(), group.to_string(), stop.clone());
        tokio::spawn(async move {
            stop.cancelled().await;
            let mut groups = this.groups.lock().await;
            if let Some(g) = groups.get_mut(&group) {
                g.members.retain(|m| m.id != id);
                if g.members.is_empty() {
                    g.stop.cancel();
                    groups.remove(&group);
                }
            }
        });
        Ok(port)
    }

    /// 在健康成员里轮转挑一个。
    async fn pick(&self, group: &str) -> Option<Member> {
        let groups = self.groups.lock().await;
        let up: Vec<&Member> = groups
            .get(group)?
            .members
            .iter()
            .filter(|m| m.up.load(Relaxed))
            .collect();
        let i = self.rr.fetch_add(1, Relaxed) % up.len().max(1);
        up.get(i).map(|m| (*m).clone())
    }
}

async fn accept_group(
    listener: TcpListener,
    group: String,
    groups: Arc<TcpGroups>,
    stop: CancellationToken,
    ctx: ClientCtx,
) {
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((visitor, peer)) = accepted else { continue };
                let (groups, group, ctx) = (groups.clone(), group.clone(), ctx.clone());
                tokio::spawn(async move {
                    // 没有健康成员：直接断开，等同 frp 里代理被摘除。
                    let Some(m) = groups.pick(&group).await else { return };
                    if let Err(e) = forward(visitor, peer, &m, &ctx.cfg).await {
                        ctx.log.debug(&format!("proxy [{}] visitor {peer}: {e}", m.proxy));
                    }
                });
            }
        }
    }
}

async fn accept_visitors(listener: TcpListener, member: Member, ctx: ClientCtx) {
    loop {
        tokio::select! {
            _ = ctx.cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((visitor, peer)) = accepted else { continue };
                if !member.up.load(Relaxed) {
                    continue;
                }
                let (member, ctx) = (member.clone(), ctx.clone());
                tokio::spawn(async move {
                    if let Err(e) = forward(visitor, peer, &member, &ctx.cfg).await {
                        ctx.log.debug(&format!("proxy [{}] visitor {peer}: {e}", member.proxy));
                    }
                });
            }
        }
    }
}

async fn forward(
    mut visitor: TcpStream,
    peer: SocketAddr,
    m: &Member,
    cfg: &EdgeConfig,
) -> Result<(), TunnelError> {
    let _ = visitor.set_nodelay(true);
    let mut stream = within(cfg.user_conn_timeout, "open work stream", m.session.open()).await?;
    write_frame(
        &mut stream,
        &StartWork {
            proxy: m.proxy.clone(),
            src: peer.to_string(),
            dst: visitor
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_default(),
        },
    )
    .await?;
    match &m.limiter {
        Some(l) => {
            let mut v = pipe::Limited::new(visitor, l.clone());
            let mut w = pipe::Limited::new(stream, l.clone());
            pipe::relay_counted(&mut v, &mut w, &m.traffic).await?;
        }
        None => {
            pipe::relay_counted(&mut visitor, &mut stream, &m.traffic).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    fn base() -> ManagedProxy {
        ManagedProxy {
            name: "p".into(),
            kind: "http".into(),
            remote_port: 0,
            local_ip: "127.0.0.1".into(),
            local_port: 80,
            custom_domains: vec!["a.com".into()],
            ..Default::default()
        }
    }

    #[test]
    fn only_listener_level_changes_require_a_restart() {
        let a = base();
        // 只有本地目标变化：无需重启。
        let mut same = a.clone();
        same.local_ip = "10.0.0.1".into();
        same.local_port = 8080;
        same.proxy_protocol = 2;
        assert!(same_listener(&a, &same));
        type Change = Box<dyn Fn(&mut ManagedProxy)>;
        let changes: Vec<(&str, Change)> = vec![
            ("kind", Box::new(|p| p.kind = "https".into())),
            ("remote_port", Box::new(|p| p.remote_port = 7)),
            (
                "domains",
                Box::new(|p| p.custom_domains.push("b.com".into())),
            ),
            ("subdomain", Box::new(|p| p.subdomain = "s".into())),
            ("rewrite", Box::new(|p| p.host_header_rewrite = "x".into())),
            ("http", Box::new(|p| p.http.http_user = "u".into())),
            ("bandwidth mode", Box::new(|p| p.bandwidth_server = true)),
            ("group", Box::new(|p| p.lb.group = "g".into())),
            ("group key", Box::new(|p| p.lb.group_key = "k".into())),
            (
                "health on",
                Box::new(|p| p.lb.health = Some(Default::default())),
            ),
        ];
        for (what, change) in changes {
            let mut b = a.clone();
            change(&mut b);
            assert!(!same_listener(&a, &b), "{what}");
        }
        // 服务端限速时，速率变化也要重建限速器。
        let (mut s1, mut s2) = (a.clone(), a);
        s1.bandwidth_server = true;
        s2.bandwidth_server = true;
        s1.bandwidth_bps = 1;
        s2.bandwidth_bps = 2;
        assert!(!same_listener(&s1, &s2));
        s2.bandwidth_bps = 1;
        assert!(same_listener(&s1, &s2));
    }
}
