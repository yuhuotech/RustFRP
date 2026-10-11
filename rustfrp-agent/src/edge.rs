//! 边缘节点（frps 角色）：监听 Client 的隧道连接，并在公网端口上转发访问者流量。
//!
//! 两种工作方式由 [`Policy`] 决定：
//! * **独立模式**（[`TokenPolicy`]，等价 frps）：客户端自己用 `NewProxy` 注册代理；
//! * **受管模式**：由 Controller 的策略放行客户端，并通过 `watch` 通道下发整份配置，Edge 对账后增删代理。

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustfrp_core::auth::{self, random_id};
use rustfrp_core::error::TunnelError;
use rustfrp_core::mux::{self, MuxConfig, Session, Stream};
use rustfrp_core::pipe;
use rustfrp_core::tls::{self, Identity};
use rustfrp_core::wire::{
    read_frame, write_frame, ClientMsg, ConfigSet, Login, ManagedProxy, ProxySpec, ServerMsg,
    StartWork, PROTOCOL_VERSION,
};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::log::Logger;

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

/// 全局已占用的代理名（跨客户端去重）。
#[derive(Default)]
struct Names(Mutex<HashSet<String>>);

impl Names {
    fn claim(&self, name: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_string())
    }
    fn release(&self, name: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
    }
}

/// 已绑定、待运行的边缘节点。
pub struct Edge {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    cfg: Arc<EdgeConfig>,
    policy: Arc<dyn Policy>,
    log: Logger,
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
        Self::bind_with_policy(cfg, identity, policy, log).await
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
        let acceptor = TlsAcceptor::from(tls::server_config(id)?);
        let listener = TcpListener::bind((cfg.bind_addr.as_str(), cfg.bind_port)).await?;
        Ok(Edge {
            listener,
            acceptor,
            cfg: Arc::new(cfg),
            policy,
            log,
        })
    }

    /// 实际监听地址。
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// 运行直到 `shutdown` 被取消。
    pub async fn serve(self, shutdown: CancellationToken) {
        let names = Arc::new(Names::default());
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                accepted = self.listener.accept() => {
                    let Ok((tcp, peer)) = accepted else { continue };
                    let ctx = ClientCtx {
                        cfg: self.cfg.clone(),
                        acceptor: self.acceptor.clone(),
                        policy: self.policy.clone(),
                        names: names.clone(),
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
    names: Arc<Names>,
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
    let result = async {
        loop {
            let msg = tokio::select! {
                _ = ctx.cancel.cancelled() => return Ok(()),
                m = next_msg(&mut reader, ctx) => m?,
            };
            match msg {
                ClientMsg::Ping => write_frame(wr, &ServerMsg::Pong).await?,
                ClientMsg::NewProxy(spec) => {
                    let resp = match start_proxy(session, &spec, ctx, &mut owned).await {
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
        match start_managed(session, p, ctx).await {
            Ok((port, stop)) => {
                running.insert(
                    p.name.clone(),
                    Running {
                        spec: p.clone(),
                        stop,
                    },
                );
                emit(&p.name, ProxyState::Running(port));
            }
            Err(e) => {
                ctx.log.warn(&format!("proxy [{}] failed: {e}", p.name));
                emit(&p.name, ProxyState::Failed(e));
            }
        }
    }
}

fn same_listener(a: &ManagedProxy, b: &ManagedProxy) -> bool {
    a.kind == b.kind && a.remote_port == b.remote_port
}

async fn start_managed(
    session: &Session,
    p: &ManagedProxy,
    ctx: &ClientCtx,
) -> Result<(u16, CancellationToken), String> {
    if p.kind != "tcp" {
        return Err(format!("proxy type [{}] is not supported yet", p.kind));
    }
    if !ctx.names.claim(&p.name) {
        return Err(format!("proxy [{}] already exists", p.name));
    }
    match bind_proxy_port(p.remote_port, &ctx.cfg).await {
        Ok((listener, port)) => {
            let stop = ctx.cancel.child_token();
            let sub = ClientCtx {
                cancel: stop.clone(),
                ..ctx.clone()
            };
            tokio::spawn(accept_visitors(
                listener,
                p.name.clone(),
                session.clone(),
                sub,
            ));
            ctx.log.info(&format!(
                "proxy [{}] start, listening on port {port}",
                p.name
            ));
            Ok((port, stop))
        }
        Err(e) => {
            ctx.names.release(&p.name);
            Err(e)
        }
    }
}

/// 为一个代理绑定公网端口并启动转发。成功返回实际端口，失败返回给客户端看的原因。
async fn start_proxy(
    session: &Session,
    spec: &ProxySpec,
    ctx: &ClientCtx,
    owned: &mut Vec<String>,
) -> Result<u16, String> {
    if spec.kind != "tcp" {
        return Err(format!("proxy type [{}] is not supported yet", spec.kind));
    }
    if ctx.cfg.max_ports_per_client > 0 && owned.len() >= ctx.cfg.max_ports_per_client {
        return Err("exceed the max_ports_per_client".into());
    }
    if !ctx.names.claim(&spec.name) {
        return Err(format!("proxy [{}] already exists", spec.name));
    }
    match bind_proxy_port(spec.remote_port, &ctx.cfg).await {
        Ok((listener, port)) => {
            owned.push(spec.name.clone());
            tokio::spawn(accept_visitors(
                listener,
                spec.name.clone(),
                session.clone(),
                ctx.clone(),
            ));
            ctx.log.info(&format!(
                "proxy [{}] start, listening on port {port}",
                spec.name
            ));
            Ok(port)
        }
        Err(e) => {
            ctx.names.release(&spec.name);
            Err(e)
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

async fn accept_visitors(listener: TcpListener, name: String, session: Session, ctx: ClientCtx) {
    loop {
        tokio::select! {
            _ = ctx.cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((visitor, peer)) = accepted else { continue };
                let (session, name, ctx) = (session.clone(), name.clone(), ctx.clone());
                tokio::spawn(async move {
                    if let Err(e) = forward(visitor, peer, &name, &session, &ctx).await {
                        ctx.log.debug(&format!("proxy [{name}] visitor {peer}: {e}"));
                    }
                });
            }
        }
    }
}

async fn forward(
    mut visitor: TcpStream,
    peer: SocketAddr,
    name: &str,
    session: &Session,
    ctx: &ClientCtx,
) -> Result<(), TunnelError> {
    let _ = visitor.set_nodelay(true);
    let mut stream = within(
        ctx.cfg.user_conn_timeout,
        "open work stream",
        session.open(),
    )
    .await?;
    write_frame(
        &mut stream,
        &StartWork {
            proxy: name.to_string(),
            src: peer.to_string(),
        },
    )
    .await?;
    pipe::relay(&mut visitor, &mut stream).await?;
    Ok(())
}
