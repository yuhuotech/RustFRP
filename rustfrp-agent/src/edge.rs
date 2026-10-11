//! 边缘节点（frps 角色）：监听 Client 的隧道连接，并在公网端口上转发访问者流量。

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustfrp_core::auth::{self, random_id};
use rustfrp_core::error::TunnelError;
use rustfrp_core::mux::{self, MuxConfig, Session, Stream};
use rustfrp_core::pipe;
use rustfrp_core::tls::{self, Identity};
use rustfrp_core::wire::{
    read_frame, write_frame, ClientMsg, Login, ProxySpec, ServerMsg, StartWork, PROTOCOL_VERSION,
};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::log::Logger;

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
    log: Logger,
}

impl Edge {
    /// 绑定监听端口。`identity` 为空时生成自签名证书。
    pub async fn bind(
        cfg: EdgeConfig,
        identity: Option<Identity>,
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
    names: Arc<Names>,
    log: Logger,
    cancel: CancellationToken,
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

fn login_error(login: &Login, cfg: &EdgeConfig) -> Option<String> {
    if login.version != PROTOCOL_VERSION {
        return Some(format!("unsupported protocol version {}", login.version));
    }
    if !cfg.token.is_empty()
        && !auth::verify(&cfg.token, login.timestamp, &login.auth, auth::unix_now())
    {
        return Some("token in login doesn't match token from configuration".into());
    }
    None
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
    if let Some(error) = login_error(&login, &ctx.cfg) {
        write_frame(
            &mut wr,
            &ServerMsg::LoginResp {
                ok: false,
                error: error.clone(),
                run_id: String::new(),
            },
        )
        .await?;
        return Err(TunnelError::Auth(error));
    }
    let run_id = random_id();
    write_frame(
        &mut wr,
        &ServerMsg::LoginResp {
            ok: true,
            error: String::new(),
            run_id: run_id.clone(),
        },
    )
    .await?;
    ctx.log.info(&format!(
        "client [{}] login success, run id [{run_id}]",
        login.client_id
    ));

    let mut owned: Vec<String> = Vec::new();
    let result = serve_control(session, &mut rd, &mut wr, ctx, &mut owned).await;
    for name in &owned {
        ctx.names.release(name);
    }
    result
}

async fn serve_control(
    session: &Session,
    rd: &mut ReadHalf<Stream>,
    wr: &mut WriteHalf<Stream>,
    ctx: &ClientCtx,
    owned: &mut Vec<String>,
) -> Result<(), TunnelError> {
    loop {
        let msg: ClientMsg = tokio::select! {
            _ = ctx.cancel.cancelled() => return Ok(()),
            m = within(ctx.cfg.heartbeat_timeout, "heartbeat", read_frame(rd)) => m?,
        };
        match msg {
            ClientMsg::Ping => write_frame(wr, &ServerMsg::Pong).await?,
            ClientMsg::NewProxy(spec) => {
                let resp = match start_proxy(session, &spec, ctx, owned).await {
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
            ClientMsg::Login(_) => return Err(TunnelError::Wire("duplicate login".into())),
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
