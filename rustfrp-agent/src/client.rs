//! 客户端（frpc 角色）：连接边缘节点，注册代理，并把到来的流转发给本地服务。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rustfrp_core::auth::{auth_key, random_id, unix_now};
use rustfrp_core::error::TunnelError;
use rustfrp_core::mux::{self, MuxConfig, Session, Stream};
use rustfrp_core::pipe;
use rustfrp_core::tls;
use rustfrp_core::wire::{
    read_frame, write_frame, ClientMsg, Login, ProxySpec, ServerMsg, StartWork, PROTOCOL_VERSION,
};
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

use crate::log::Logger;

/// 一个待注册的代理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyCfg {
    /// 名称（已含 user 前缀）。
    pub name: String,
    /// 类型，当前仅 `tcp`。
    pub kind: String,
    /// 本地目标主机。
    pub local_ip: String,
    /// 本地目标端口。
    pub local_port: u16,
    /// 请求的远程端口，0 = 由服务端分配。
    pub remote_port: u16,
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
            dial_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(10),
            heartbeat_interval: Duration::from_secs(30),
            heartbeat_timeout: Duration::from_secs(90),
            local_dial_timeout: Duration::from_secs(5),
            backoff: (Duration::from_secs(1), Duration::from_secs(30)),
            mux: MuxConfig::default(),
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
    /// 某代理启动失败。
    ProxyFailed {
        /// 代理名。
        name: String,
        /// 原因。
        error: String,
    },
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
    let mut ever_logged_in = false;
    loop {
        let end = tokio::select! {
            _ = cancel.cancelled() => SessionEnd::Cancelled,
            end = session_once(&cfg, &connector, &log, &events, &cancel) => end,
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
    let conf = tls::client_config(trusted).map_err(|e| ClientError::Config(e.to_string()))?;
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

async fn session_once(
    cfg: &Arc<ClientConfig>,
    connector: &TlsConnector,
    log: &Logger,
    events: &Option<mpsc::Sender<Event>>,
    cancel: &CancellationToken,
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
    let end = talk(cfg, &session, &mut inbound, log, events, cancel).await;
    driver.abort();
    end
}

fn login_msg(cfg: &ClientConfig) -> ClientMsg {
    let ts = unix_now();
    let client_id = if cfg.client_id.is_empty() {
        random_id()
    } else {
        cfg.client_id.clone()
    };
    ClientMsg::Login(Login {
        version: PROTOCOL_VERSION,
        user: cfg.user.clone(),
        client_id,
        timestamp: ts,
        auth: if cfg.token.is_empty() {
            String::new()
        } else {
            auth_key(&cfg.token, ts)
        },
        metas: Default::default(),
    })
}

async fn talk(
    cfg: &Arc<ClientConfig>,
    session: &Session,
    inbound: &mut mpsc::Receiver<Stream>,
    log: &Logger,
    events: &Option<mpsc::Sender<Event>>,
    cancel: &CancellationToken,
) -> SessionEnd {
    let lost = |reason: String, logged_in: bool| SessionEnd::Lost { reason, logged_in };
    let ctrl = match session.open().await {
        Ok(c) => c,
        Err(e) => return lost(e.to_string(), false),
    };
    let (mut rd, mut wr) = tokio::io::split(ctrl);
    if let Err(e) = write_frame(&mut wr, &login_msg(cfg)).await {
        return lost(e.to_string(), false);
    }
    match within(
        cfg.handshake_timeout,
        "login response",
        read_frame::<_, ServerMsg>(&mut rd),
    )
    .await
    {
        Ok(ServerMsg::LoginResp {
            ok: true, run_id, ..
        }) => log.info(&format!("login to server success, run id [{run_id}]")),
        Ok(ServerMsg::LoginResp { error, .. }) => return SessionEnd::Rejected(error),
        Ok(_) => return lost("unexpected message in login".into(), false),
        Err(e) => return lost(e.to_string(), false),
    }
    emit(events, Event::Connected);

    // 注册全部代理（顺序进行，逐个读应答）。
    let mut locals: HashMap<String, (String, u16)> = HashMap::new();
    for p in &cfg.proxies {
        let spec = ProxySpec {
            name: p.name.clone(),
            kind: p.kind.clone(),
            remote_port: p.remote_port,
        };
        if let Err(e) = write_frame(&mut wr, &ClientMsg::NewProxy(spec)).await {
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
                locals.insert(name.clone(), (p.local_ip.clone(), p.local_port));
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
    let locals = Arc::new(locals);

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
                Some(Ok(_)) => {}
                Some(Err(e)) => break lost(e.to_string(), true),
                None => break lost("control stream closed".into(), true),
            },
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
    end
}

async fn handle_work(
    mut stream: Stream,
    locals: &HashMap<String, (String, u16)>,
    cfg: &ClientConfig,
) -> Result<(), TunnelError> {
    let start: StartWork =
        within(cfg.handshake_timeout, "start work", read_frame(&mut stream)).await?;
    let Some((host, port)) = locals.get(&start.proxy) else {
        return Err(TunnelError::Wire(format!(
            "unknown proxy [{}]",
            start.proxy
        )));
    };
    let mut local = within(cfg.local_dial_timeout, "dial local service", async {
        Ok(TcpStream::connect((host.as_str(), *port)).await?)
    })
    .await?;
    let _ = local.set_nodelay(true);
    pipe::relay(&mut stream, &mut local).await?;
    Ok(())
}
