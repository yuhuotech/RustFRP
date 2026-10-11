//! 命令执行：校验、打印兼容性报告、运行客户端或边缘节点。

use std::io::Write;
use std::sync::Arc;

use rustfrp_core::frpcfg::validate::{validate_client_all, validate_server};
use rustfrp_core::frpcfg::{
    load_client, load_server, CompatReport, ConfigFs, Env, LoadOptions, Severity,
};
use tokio_util::sync::CancellationToken;

use crate::cli::{AdminAction, Command, Mode, Options, HELP};
use crate::edge::Edge;
use crate::log::{Level, Logger};
use crate::{client, local_control, managed, plan};

/// 程序版本。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_report(report: &CompatReport, out: &mut dyn Write) {
    for (sev, tag) in [
        (Severity::Unsupported, "unsupported"),
        (Severity::Ignored, "ignored"),
        (Severity::UnknownField, "unknown-field"),
    ] {
        for i in report.of(sev) {
            let _ = writeln!(out, "[{tag}] {}: {}", i.path, i.message);
        }
    }
}

fn logger(to: &str, level: &str, err: &mut dyn Write) -> Logger {
    match Logger::from_target(to, Level::parse(level)) {
        Ok(l) => l,
        Err(e) => {
            let _ = writeln!(
                err,
                "cannot open log target {to}: {e}; falling back to console"
            );
            Logger::new(Level::parse(level), Box::new(std::io::stderr()))
        }
    }
}

/// 等待停止信号：Ctrl-C，Unix 上还有 systemd / launchd 发的 SIGTERM。
pub async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// 执行命令，返回进程退出码。
pub async fn execute(
    cmd: Command,
    fs: &dyn ConfigFs,
    env: &dyn Env,
    out: &mut dyn Write,
    err: &mut dyn Write,
    cancel: CancellationToken,
) -> i32 {
    match cmd {
        Command::Help => {
            let _ = write!(out, "{HELP}");
            0
        }
        Command::Version => {
            let _ = writeln!(out, "rustfrp-agent {VERSION}");
            0
        }
        Command::Service(c) => crate::service::run(&c, &crate::service::StdHost, out, err),
        Command::Admin(action, o) => admin_command(action, &o, fs, env, out, err).await,
        Command::Verify(o) => verify(&o, fs, env, out, err),
        Command::Convert(o) => match crate::convert::convert(&o, fs, env) {
            Ok((text, report)) => {
                print_report(&report, err);
                match out.write_all(text.as_bytes()) {
                    Ok(()) => 0,
                    Err(e) => fail(err, "convert output", e),
                }
            }
            Err(e) => fail(err, "convert", e),
        },
        Command::Run(o) => {
            let control = match &o.local_control {
                Some(path) => {
                    let identity = match (&o.managed, o.mode, o.config.is_empty()) {
                        (Some(m), Mode::Client, _) => m.controller.clone(),
                        (None, Mode::Client, false) => format!("standalone:{}", o.config),
                        _ => return fail(err, "local control", "requires a client mode"),
                    };
                    match local_control::LocalControl::bind(path, &identity).await {
                        Ok(c) => Some(c),
                        Err(e) => return fail(err, "local control", e),
                    }
                }
                None => None,
            };
            let status = control.as_ref().map(|c| {
                (
                    c.status.clone(),
                    c.traffic.clone(),
                    c.logs.clone(),
                    c.tunnels.clone(),
                )
            });
            let local_cancel = cancel.child_token();
            let local_run = async {
                if let Some(c) = control {
                    c.serve(local_cancel.clone()).await;
                }
            };
            let agent_run = async {
                let code = run(&o, fs, env, out, err, local_cancel.clone(), status).await;
                local_cancel.cancel();
                code
            };
            tokio::join!(agent_run, local_run).0
        }
    }
}

/// `reload` / `status` / `stop`：读配置里的 `webServer`，向运行中的客户端发一次请求。
async fn admin_command(
    action: AdminAction,
    o: &Options,
    fs: &dyn ConfigFs,
    env: &dyn Env,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let opts = LoadOptions {
        strict: false,
        fs,
        env,
    };
    let l = match load_client(&o.config, &opts) {
        Ok(l) => l,
        Err(e) => return fail(err, &o.config, e),
    };
    let web = &l.common.web_server;
    if web.port <= 0 {
        return fail(
            err,
            &o.config,
            "webServer.port is not configured; the client has no admin interface",
        );
    }
    let host = if web.addr.is_empty() || web.addr == "0.0.0.0" {
        "127.0.0.1"
    } else {
        web.addr.as_str()
    };
    let addr = format!("{host}:{}", web.port);
    let path = match action {
        AdminAction::Reload => "/api/reload",
        AdminAction::Status => "/api/status",
        AdminAction::Stop => "/api/stop",
    };
    match crate::admin::request(&addr, path, &web.user, &web.password).await {
        Ok((200, body)) => {
            let _ = match action {
                AdminAction::Reload => writeln!(out, "reload success"),
                AdminAction::Stop => writeln!(out, "stop success"),
                AdminAction::Status => writeln!(out, "{body}"),
            };
            0
        }
        Ok((code, body)) => fail(err, &addr, format!("{path}: HTTP {code} {}", body.trim())),
        Err(e) => fail(err, &addr, e),
    }
}

fn verify(
    o: &Options,
    fs: &dyn ConfigFs,
    env: &dyn Env,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let opts = LoadOptions {
        strict: o.strict,
        fs,
        env,
    };
    let result = match o.mode {
        Mode::Client => load_client(&o.config, &opts).and_then(|l| {
            let warnings =
                validate_client_all(&l.common, &l.proxies, &l.visitors, &o.allow_unsafe)?;
            for w in warnings {
                let _ = writeln!(out, "[warning] {w}");
            }
            Ok(l.report)
        }),
        Mode::Edge => load_server(&o.config, &opts).and_then(|l| {
            validate_server(&l.config, &o.allow_unsafe)?;
            Ok(l.report)
        }),
    };
    match result {
        Ok(report) => {
            print_report(&report, out);
            let _ = writeln!(out, "{}: configuration file is ok", o.config);
            0
        }
        Err(e) => {
            let _ = writeln!(err, "{}: {e}", o.config);
            1
        }
    }
}

async fn run(
    o: &Options,
    fs: &dyn ConfigFs,
    env: &dyn Env,
    out: &mut dyn Write,
    err: &mut dyn Write,
    cancel: CancellationToken,
    status: Option<(
        local_control::SharedStatus,
        Arc<rustfrp_core::pipe::Traffic>,
        Arc<crate::log_ring::LogRing>,
        crate::local_control::TunnelList,
    )>,
) -> i32 {
    if let Some(m) = &o.managed {
        if o.mode == Mode::Edge {
            return run_node(o, m, fs, env, err, cancel).await;
        }
        return run_managed(m, fs, env, err, cancel, status).await;
    }
    let opts = LoadOptions {
        strict: o.strict,
        fs,
        env,
    };
    match o.mode {
        Mode::Client => {
            let l = match load_client(&o.config, &opts) {
                Ok(l) => l,
                Err(e) => return fail(err, &o.config, e),
            };
            if let Err(e) = validate_client_all(&l.common, &l.proxies, &l.visitors, &o.allow_unsafe)
            {
                return fail(err, &o.config, e);
            }
            print_report(&l.report, out);
            let plan = match plan::client_plan(&l, fs) {
                Ok(p) => p,
                Err(e) => return fail(err, &o.config, e),
            };
            let log = logger(&plan.log_to, &plan.log_level, err);
            log_notes(&log, &plan.notes);
            let web = l.common.web_server.clone();
            match run_standalone_client(o, plan.cfg, &web, (fs, env), log, cancel, status).await {
                Ok(()) => 0,
                Err(e) => fail(err, &o.config, e),
            }
        }
        Mode::Edge => {
            let l = match load_server(&o.config, &opts) {
                Ok(l) => l,
                Err(e) => return fail(err, &o.config, e),
            };
            if let Err(e) = validate_server(&l.config, &o.allow_unsafe) {
                return fail(err, &o.config, e);
            }
            print_report(&l.report, out);
            let plan = match plan::edge_plan(&l, fs) {
                Ok(p) => p,
                Err(e) => return fail(err, &o.config, e),
            };
            let log = logger(&plan.log_to, &plan.log_level, err);
            log_notes(&log, &plan.notes);
            let edge = match Edge::bind(plan.cfg, plan.identity, log.clone()).await {
                Ok(e) => e,
                Err(e) => return fail(err, &o.config, e),
            };
            let addr = edge
                .local_addr()
                .map_or_else(|_| "?".to_string(), |a| a.to_string());
            log.info(&format!("rustfrp edge listening on {addr}"));
            edge.serve(cancel).await;
            0
        }
    }
}

/// 独立模式的客户端；配置了 `webServer.port` 时同时提供管理接口，并支持 `reload`。
async fn run_standalone_client(
    o: &Options,
    mut cfg: client::ClientConfig,
    web: &rustfrp_core::frpcfg::WebServerConfig,
    sources: (&dyn ConfigFs, &dyn Env),
    mut log: Logger,
    cancel: CancellationToken,
    local: Option<(
        local_control::SharedStatus,
        Arc<rustfrp_core::pipe::Traffic>,
        Arc<crate::log_ring::LogRing>,
        local_control::TunnelList,
    )>,
) -> Result<(), String> {
    use crate::admin::{self, Admin, ProxyStatus, ReloadReq};
    let (fs, env) = sources;
    let local_status = local.as_ref().map(|x| x.0.clone());
    let local_tunnels = local.as_ref().map(|x| x.3.clone());
    if let Some((status, traffic, logs, tunnels)) = &local {
        cfg.traffic = traffic.clone();
        cfg.local_tunnels = tunnels.clone();
        local_control::update_standalone_tunnels(status, tunnels, &cfg.proxies)
            .map_err(str::to_string)?;
        log = log.with_ring(logs.clone());
    }
    if web.port <= 0 {
        if let Some(status) = local_status {
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let tracker = tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    local_control::apply(&status, &event);
                }
            });
            let result = client::run(cfg, log, Some(tx), cancel).await;
            let _ = tracker.await;
            return result.map_err(|e| e.to_string());
        }
        return client::run(cfg, log, None, cancel)
            .await
            .map_err(|e| e.to_string());
    }
    let status: admin::StatusTable = Arc::default();
    let fill = |cfg: &client::ClientConfig| {
        *status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = cfg
            .proxies
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    ProxyStatus {
                        kind: p.kind.clone(),
                        state: "wait start".into(),
                        error: String::new(),
                        local: format!("{}:{}", p.local_ip, p.local_port),
                        remote: String::new(),
                    },
                )
            })
            .collect();
    };
    fill(&cfg);
    // 配置加载时已补默认值（127.0.0.1）。
    let addr = web.addr.as_str();
    let port = u16::try_from(web.port).map_err(|_| "webServer.port out of range".to_string())?;
    let listener = tokio::net::TcpListener::bind((addr, port))
        .await
        .map_err(|e| format!("webServer listen on {addr}:{port}: {e}"))?;
    log.info(&format!("admin server listening on {addr}:{port}"));
    let config_text = Arc::new(std::sync::Mutex::new(
        fs.read_to_string(&o.config).unwrap_or_default(),
    ));
    let (reload_tx, mut reload_rx) = tokio::sync::mpsc::channel::<ReloadReq>(1);
    let (cfg_tx, cfg_rx) = tokio::sync::mpsc::channel(1);
    let (etx, mut erx) = tokio::sync::mpsc::channel(64);
    let admin_events = status.clone();
    let local_events = local_status.clone();
    tokio::spawn(async move {
        while let Some(event) = erx.recv().await {
            if let Some(local) = &local_events {
                local_control::apply(local, &event);
            }
            admin::apply_event(&admin_events, event);
        }
    });
    let admin = Arc::new(Admin {
        user: web.user.clone(),
        password: web.password.clone(),
        status: status.clone(),
        config_text: config_text.clone(),
        reload: reload_tx,
        stop: cancel.clone(),
    });
    let server = tokio::spawn(admin::serve(listener, admin, cancel.clone()));
    let traffic = cfg.traffic.clone();
    let task = client::run_with_reload(cfg, log.clone(), Some(etx), cancel.clone(), Some(cfg_rx));
    tokio::pin!(task);
    let result = loop {
        tokio::select! {
            r = &mut task => break r.map_err(|e| e.to_string()),
            Some(req) = reload_rx.recv() => {
                let reloaded = reload_config(o, fs, env, &log);
                let reply = match reloaded {
                    Ok(mut new_cfg) => {
                        new_cfg.traffic = traffic.clone();
                        if let (Some(local), Some(tunnels)) = (&local_status, &local_tunnels) {
                            new_cfg.local_tunnels = tunnels.clone();
                            if let Err(error) = local_control::update_standalone_tunnels(
                                local,
                                tunnels,
                                &new_cfg.proxies,
                            ) {
                                let _ = req.reply.send(Err(error.into()));
                                continue;
                            }
                        }
                        fill(&new_cfg);
                        if let Ok(text) = fs.read_to_string(&o.config) {
                            *config_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = text;
                        }
                        let _ = cfg_tx.send(new_cfg).await;
                        Ok(())
                    }
                    Err(e) => {
                        log.error(&format!("reload failed: {e}"));
                        Err(e)
                    }
                };
                let _ = req.reply.send(reply);
            }
        }
    };
    cancel.cancel();
    let _ = server.await;
    result
}

/// 重新读取并校验配置文件，得到新的运行配置（失败时旧配置继续运行）。
fn reload_config(
    o: &Options,
    fs: &dyn ConfigFs,
    env: &dyn Env,
    log: &Logger,
) -> Result<client::ClientConfig, String> {
    let opts = LoadOptions {
        strict: o.strict,
        fs,
        env,
    };
    let l = load_client(&o.config, &opts).map_err(|e| e.to_string())?;
    validate_client_all(&l.common, &l.proxies, &l.visitors, &o.allow_unsafe)
        .map_err(|e| e.to_string())?;
    let plan = plan::client_plan(&l, fs)?;
    log_notes(log, &plan.notes);
    Ok(plan.cfg)
}

/// 把「已识别但尚未实现」的选项逐条写进启动日志（不得静默忽略）。
fn log_notes(log: &Logger, notes: &[String]) {
    for n in notes {
        log.warn(n);
    }
}

/// 受管客户端：身份来自状态目录，配置由 Controller 下发。
async fn run_managed(
    m: &managed::ManagedOpts,
    fs: &dyn ConfigFs,
    env: &dyn Env,
    err: &mut dyn Write,
    cancel: CancellationToken,
    status: Option<(
        local_control::SharedStatus,
        Arc<rustfrp_core::pipe::Traffic>,
        Arc<crate::log_ring::LogRing>,
        crate::local_control::TunnelList,
    )>,
) -> i32 {
    let prepared = managed::state_dir(m, env).and_then(|dir| {
        let id = managed::load_or_create(fs, &dir)?;
        let cfg = managed::client_config(m, &id, fs, env)?;
        Ok((dir, id, cfg))
    });
    let (dir, id, mut cfg) = match prepared {
        Ok(v) => v,
        Err(e) => return fail(err, "managed", e),
    };
    if let Some((_, traffic, _, tunnels)) = &status {
        cfg.traffic = traffic.clone();
        cfg.local_tunnels = tunnels.clone();
    }
    let log = Logger::new(Level::Info, Box::new(std::io::stderr()));
    let log = match &status {
        Some((_, _, logs, _)) => log.with_ring(logs.clone()),
        None => log,
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let key = id.key.clone();
    let persist = async {
        let mut persistence_error = None;
        let mut restart = false;
        while let Some(ev) = rx.recv().await {
            if let Some((s, _, _, _)) = &status {
                local_control::apply(s, &ev);
            }
            match ev {
                client::Event::Enrolled { client_id } => {
                    match managed::save(fs, &dir, &key, Some(&client_id)) {
                        Ok(()) => log.info(&format!("enrolled as device [{client_id}]")),
                        Err(e) => {
                            cancel.cancel();
                            persistence_error = Some(e);
                            break;
                        }
                    }
                }
                client::Event::ConfigApplied { revision, proxies } => log.info(&format!(
                    "applied config revision {revision} ({proxies} tunnels)"
                )),
                client::Event::RestartRequested => {
                    if env.get("RUSTFRP_SUPERVISED").as_deref() == Some("1") {
                        log.warn("restart requested by the controller; exiting so the service manager restarts the agent");
                        restart = true;
                        cancel.cancel();
                        break;
                    }
                    log.warn("restart requested by the controller, but the agent is not running as a service; ignoring");
                }
                client::Event::Disconnected(r) => log.warn(&format!("disconnected: {r}")),
                _ => {}
            }
        }
        (persistence_error, restart)
    };
    let run = client::run(cfg, log.clone(), Some(tx), cancel.clone());
    let (result, (persistence_error, restart)) = tokio::join!(run, persist);
    if let Some(e) = persistence_error {
        return fail(err, "managed identity", e);
    }
    match result {
        // 非零退出码让 systemd / launchd 把进程重新拉起（`Restart=always` / `KeepAlive`）。
        Ok(()) if restart => RESTART_EXIT_CODE,
        Ok(()) => 0,
        Err(e) => fail(err, "managed", e),
    }
}

/// 管理员远程重启时的退出码（EX_TEMPFAIL）：服务管理器据此重新拉起 agent。
pub const RESTART_EXIT_CODE: i32 = 75;

/// 受管 Edge：身份与设备缓存在状态目录，监听参数来自可选的 frps.toml，设备与隧道配置来自 Controller。
async fn run_node(
    o: &Options,
    m: &managed::ManagedOpts,
    fs: &dyn ConfigFs,
    env: &dyn Env,
    err: &mut dyn Write,
    cancel: CancellationToken,
) -> i32 {
    let prepared = prepare_node(o, m, fs, env, err);
    let (dir, identity, run, log, node_id_known) = match prepared {
        Ok(v) => v,
        Err(e) => return fail(err, "managed edge", e),
    };
    let key = identity.key.clone();
    let (events_tx, mut events) = tokio::sync::mpsc::channel(16);
    let mut run = run;
    run.events = events_tx;
    run.reject_exit = !node_id_known;
    let inner = cancel.child_token();
    let handle = match crate::node::start_node(run, log.clone(), inner.clone()).await {
        Ok(h) => h,
        Err(e) => return fail(err, "managed edge", e),
    };
    let mut code = 0;
    loop {
        tokio::select! {
            _ = inner.cancelled() => break,
            event = events.recv() => {
                match handle_node_event(event, NodeEventContext {
                    node_id_known,
                    fs,
                    dir: &dir,
                    key: &key,
                    log: &log,
                    cancel: &inner,
                    err,
                }) {
                    NodeEventResult::Continue => {}
                    NodeEventResult::Stop => break,
                    NodeEventResult::Exit(exit_code) => {
                        code = exit_code;
                        break;
                    }
                }
            },
        }
    }
    inner.cancel();
    handle.join().await;
    code
}

#[derive(Debug, PartialEq, Eq)]
enum NodeEventResult {
    Continue,
    Stop,
    Exit(i32),
}

struct NodeEventContext<'a> {
    node_id_known: bool,
    fs: &'a dyn ConfigFs,
    dir: &'a str,
    key: &'a rustfrp_core::identity::DeviceKey,
    log: &'a Logger,
    cancel: &'a CancellationToken,
    err: &'a mut dyn Write,
}

fn handle_node_event(
    event: Option<crate::node::LinkEvent>,
    context: NodeEventContext<'_>,
) -> NodeEventResult {
    match event {
        Some(crate::node::LinkEvent::Enrolled(id)) => {
            match managed::save(context.fs, context.dir, context.key, Some(&id)) {
                Ok(()) => {
                    context.log.info(&format!("enrolled as node [{id}]"));
                    NodeEventResult::Continue
                }
                Err(e) => {
                    context.cancel.cancel();
                    NodeEventResult::Exit(fail(context.err, "managed edge identity", e))
                }
            }
        }
        Some(crate::node::LinkEvent::Rejected(why)) if !context.node_id_known => {
            context.cancel.cancel();
            NodeEventResult::Exit(fail(
                context.err,
                "managed edge",
                format!("controller rejected the enrollment: {why}"),
            ))
        }
        Some(_) => NodeEventResult::Continue,
        None => NodeEventResult::Stop,
    }
}

type NodePrepared = (
    String,
    managed::LoadedIdentity,
    crate::node::NodeRun,
    Logger,
    bool,
);

fn prepare_node(
    o: &Options,
    m: &managed::ManagedOpts,
    fs: &dyn ConfigFs,
    env: &dyn Env,
    err: &mut dyn Write,
) -> Result<NodePrepared, String> {
    let dir = managed::state_dir(m, env)?;
    let id = managed::load_or_create(fs, &dir)?;
    let (host, port) = managed::split_controller_default(&m.controller, 7001)?;
    // 监听参数：frps.toml（可选）；证书缺省时在状态目录里持久化一份自签名证书。
    let (edge_cfg, cfg_identity, log) = if o.config.is_empty() {
        (
            crate::edge::EdgeConfig::default(),
            None,
            Logger::new(Level::Info, Box::new(std::io::stderr())),
        )
    } else {
        let opts = LoadOptions {
            strict: o.strict,
            fs,
            env,
        };
        let l = load_server(&o.config, &opts).map_err(|e| format!("{}: {e}", o.config))?;
        validate_server(&l.config, &o.allow_unsafe).map_err(|e| format!("{}: {e}", o.config))?;
        print_report(&l.report, err);
        let plan = plan::edge_plan(&l, fs).map_err(|e| format!("{}: {e}", o.config))?;
        let log = logger(&plan.log_to, &plan.log_level, err);
        // 设备由 Controller 的身份体系认证，frps 的共享令牌在受管节点上不起作用。
        if !plan.cfg.token.is_empty() {
            log.warn("auth.token in the frps config is ignored in managed mode; devices authenticate with their own keys");
        }
        (plan.cfg, plan.identity, log)
    };
    let identity = match cfg_identity {
        Some(i) => i,
        None => persistent_edge_identity(fs, &dir, m.public_host.as_deref())?,
    };
    let node_id = id.client_id.clone();
    let enroll = match (&node_id, &m.enroll_token) {
        (Some(_), _) => None,
        (None, Some(token)) => {
            let public = m.public_host.clone().ok_or(
                "pass --public-host <name clients use to reach this node> for the first run",
            )?;
            let _ = public;
            Some(rustfrp_core::node::NodeEnroll {
                token: token.clone(),
                name: m
                    .name
                    .clone()
                    .or_else(|| env.get("HOSTNAME"))
                    .or_else(|| env.get("COMPUTERNAME"))
                    .unwrap_or_else(|| std::env::consts::OS.to_string()),
                pubkey: id.key.public_hex(),
            })
        }
        (None, None) => {
            return Err(
                "this node is not enrolled yet; pass --enroll-token <token> from the panel".into(),
            )
        }
    };
    let trusted = match &m.ca_file {
        Some(f) => {
            let pem = fs
                .read_to_string(f)
                .map_err(|e| format!("read --ca-file {f}: {e}"))?;
            Some(rustfrp_core::tls::certs_from_pem(pem.as_bytes()).map_err(|e| e.to_string())?)
        }
        None => None,
    };
    let connector = tokio_rustls::TlsConnector::from(
        rustfrp_core::tls::client_config_pinned(trusted, m.tls_pin.as_deref())
            .map_err(|e| e.to_string())?,
    );
    let known = node_id.is_some();
    let run = crate::node::NodeRun {
        edge: edge_cfg,
        identity: Some(identity),
        link: crate::node::LinkCtx {
            host,
            port,
            key: id.key.clone(),
            node_id,
            enroll,
            info: rustfrp_core::node::NodeInfo {
                public_host: m.public_host.clone().unwrap_or_default(),
                version: VERSION.into(),
                os: std::env::consts::OS.into(),
                ..Default::default()
            },
            connector,
            backoff: (
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(30),
            ),
            heartbeat: (
                std::time::Duration::from_secs(20),
                std::time::Duration::from_secs(60),
            ),
            metrics: None,
            sample_every: std::time::Duration::from_secs(5),
            sweep_after: std::time::Duration::from_secs(300),
        },
        store: Arc::new(crate::node::FileStore {
            path: std::path::PathBuf::from(format!("{dir}/devices.json")),
        }),
        events: tokio::sync::mpsc::channel(1).0,
        reject_exit: !known,
    };
    Ok((dir, id, run, log, known))
}

/// 客户端连接用的 TLS 证书：优先复用状态目录里的，否则生成自签名并保存（私钥仅当前用户可读）。
fn persistent_edge_identity(
    fs: &dyn ConfigFs,
    dir: &str,
    public_host: Option<&str>,
) -> Result<rustfrp_core::tls::Identity, String> {
    let (cert_path, key_path) = (
        format!("{dir}/edge-cert.pem"),
        format!("{dir}/edge-key.pem"),
    );
    if let (Ok(cert), Ok(key)) = (fs.read_to_string(&cert_path), fs.read_to_string(&key_path)) {
        return rustfrp_core::tls::identity_from_pem(cert.as_bytes(), key.as_bytes())
            .map_err(|e| format!("{cert_path}: {e}"));
    }
    let mut names = vec!["rustfrp".to_string()];
    names.extend(public_host.map(str::to_string));
    let (cert, key) = rustfrp_core::tls::self_signed_pem(names).map_err(|e| e.to_string())?;
    fs.create_dir_all(dir)
        .map_err(|e| format!("create {dir}: {e}"))?;
    fs.write_private(&key_path, &key)
        .map_err(|e| format!("write {key_path}: {e}"))?;
    fs.write_private(&cert_path, &cert)
        .map_err(|e| format!("write {cert_path}: {e}"))?;
    rustfrp_core::tls::identity_from_pem(cert.as_bytes(), key.as_bytes()).map_err(|e| e.to_string())
}

fn fail(err: &mut dyn Write, config: &str, e: impl std::fmt::Display) -> i32 {
    let _ = writeln!(err, "{config}: {e}");
    1
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use crate::edge::{Edge, EdgeConfig};
    use rustfrp_core::frpcfg::{MapEnv, MemFs};
    use std::time::Duration;

    fn opts(mode: Mode, config: &str) -> Options {
        Options {
            mode,
            config: config.into(),
            managed: None,
            local_control: None,
            strict: true,
            allow_unsafe: vec![],
        }
    }

    #[tokio::test]
    async fn standalone_local_control_tracks_connection_events_without_web_server() {
        let fs = MemFs::new().with(
            "local.toml",
            "serverAddr='127.0.0.1'\nserverPort=7000\n[[proxies]]\nname='local'\ntype='tcp'\nlocalPort=1\nremotePort=0\n",
        );
        let env = MapEnv::default();
        let loaded = load_client(
            "local.toml",
            &LoadOptions {
                strict: true,
                fs: &fs,
                env: &env,
            },
        )
        .unwrap();
        let plan = plan::client_plan(&loaded, &fs).unwrap();
        let edge = Edge::bind(
            EdgeConfig {
                bind_addr: "127.0.0.1".into(),
                bind_port: 0,
                proxy_bind_addr: "127.0.0.1".into(),
                ..Default::default()
            },
            None,
            Logger::silent(),
        )
        .await
        .unwrap();
        let port = edge.local_addr().unwrap().port();
        let stop_edge = CancellationToken::new();
        tokio::spawn(edge.serve(stop_edge.clone()));
        let mut cfg = plan.cfg;
        cfg.server_port = port;
        let status = local_control::SharedStatus::default();
        let tunnels = local_control::TunnelList::default();
        let cancel = CancellationToken::new();
        let options = opts(Mode::Client, "local.toml");
        let run = run_standalone_client(
            &options,
            cfg,
            &loaded.common.web_server,
            (&fs, &env),
            Logger::silent(),
            cancel.clone(),
            Some((
                status.clone(),
                Arc::default(),
                Arc::default(),
                tunnels.clone(),
            )),
        );
        let stop = cancel.clone();
        let local_tunnels = tunnels.clone();
        let wait_connected = async move {
            for _ in 0..100 {
                if status
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .connected
                {
                    let has_summary = local_tunnels
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .1
                        .len()
                        == 1;
                    stop.cancel();
                    return has_summary;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            stop.cancel();
            false
        };
        let (result, connected) = tokio::join!(run, wait_connected);
        assert!(result.is_ok());
        assert!(connected);
        stop_edge.cancel();
    }

    #[tokio::test]
    async fn standalone_local_control_rejects_an_oversized_initial_summary() {
        let fs = MemFs::new();
        let env = MapEnv::default();
        let cfg = client::ClientConfig {
            proxies: vec![client::ProxyCfg::default(); 1025],
            ..Default::default()
        };
        let error = run_standalone_client(
            &opts(Mode::Client, "local.toml"),
            cfg,
            &Default::default(),
            (&fs, &env),
            Logger::silent(),
            CancellationToken::new(),
            Some((
                local_control::SharedStatus::default(),
                Arc::default(),
                Arc::default(),
                local_control::TunnelList::default(),
            )),
        )
        .await
        .unwrap_err();
        assert!(error.contains("standalone tunnel summary exceeds capacity"));
    }

    async fn exec(cmd: Command, fs: &MemFs) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = execute(
            cmd,
            fs,
            &MapEnv::default(),
            &mut out,
            &mut err,
            CancellationToken::new(),
        )
        .await;
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[tokio::test]
    async fn help_and_version() {
        let fs = MemFs::new();
        let (c, o, _) = exec(Command::Help, &fs).await;
        assert!((c, o.contains("USAGE")) == (0, true));
        let (c, o, _) = exec(Command::Version, &fs).await;
        assert!(c == 0 && o.starts_with("rustfrp-agent "));
    }

    #[tokio::test]
    async fn verify_reports() {
        let fs = MemFs::new()
            .with("ok.toml", "serverAddr='x'\ntransport.protocol='quic'\n[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\n")
            .with("warn.toml", "transport.tls.enable=false\ntransport.tls.certFile='c'\n")
            .with("bad.toml", "serverPort = \"x\"\n")
            .with("invalid.toml", "log.level='loud'\n")
            .with("srv.toml", "bindPort=7000\nkcpBindPort=7000\n")
            .with("srv_bad.toml", "bindPort=70000\n");
        let (c, o, _) = exec(Command::Verify(opts(Mode::Client, "ok.toml")), &fs).await;
        assert_eq!(c, 0);
        assert!(
            o.contains("[unsupported] transport.protocol")
                && o.contains("configuration file is ok")
        );
        let (c, o, _) = exec(Command::Verify(opts(Mode::Client, "warn.toml")), &fs).await;
        assert!(c == 0 && o.contains("[warning]"));
        for (mode, file) in [
            (Mode::Client, "bad.toml"),
            (Mode::Client, "invalid.toml"),
            (Mode::Client, "missing.toml"),
            (Mode::Edge, "srv_bad.toml"),
        ] {
            let (c, _, e) = exec(Command::Verify(opts(mode, file)), &fs).await;
            assert!(c == 1 && e.starts_with(file), "{file}: {e}");
        }
        let (c, o, _) = exec(Command::Verify(opts(Mode::Edge, "srv.toml")), &fs).await;
        assert!(c == 0 && o.contains("[unsupported] kcpBindPort"));
    }

    #[tokio::test]
    async fn run_rejects_bad_configs() {
        let fs = MemFs::new()
            .with("bad.toml", "serverPort = \"x\"\n")
            .with("invalid.toml", "log.level='loud'\n")
            .with("oidc.toml", "auth.method='oidc'\nauth.oidc.clientID='a'\nauth.oidc.tokenEndpointURL='http://x'\n")
            .with("sbad.toml", "bindPort=70000\n")
            .with("soidc.toml", "auth.method='oidc'\n");
        for (mode, file) in [
            (Mode::Client, "bad.toml"),
            (Mode::Client, "invalid.toml"),
            (Mode::Client, "oidc.toml"),
            (Mode::Edge, "bad.toml"),
            (Mode::Edge, "sbad.toml"),
            (Mode::Edge, "soidc.toml"),
        ] {
            let (c, _, e) = exec(Command::Run(opts(mode, file)), &fs).await;
            assert!(c == 1 && !e.is_empty(), "{file}: code {c} {e}");
        }
    }

    async fn run_cmd(cmd: Command, fs: &MemFs, cancel: CancellationToken) -> i32 {
        let (mut o, mut e) = (Vec::new(), Vec::new());
        execute(cmd, fs, &MapEnv::default(), &mut o, &mut e, cancel).await
    }

    #[tokio::test]
    async fn edge_and_client_run_until_cancelled() {
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let fs = MemFs::new()
            .with("frps.toml", &format!("bindAddr='127.0.0.1'\nbindPort={port}\n"))
            .with(
                "frpc.toml",
                &format!("serverAddr='127.0.0.1'\nserverPort={port}\nlog.level='error'\n[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\nremotePort=0\ntransport.useEncryption=true\n"),
            );
        let loaded = load_client(
            "frpc.toml",
            &LoadOptions {
                strict: true,
                fs: &fs,
                env: &MapEnv::default(),
            },
        )
        .unwrap();
        assert!(plan::client_plan(&loaded, &fs)
            .unwrap()
            .notes
            .iter()
            .any(|n| n.contains("transport.useEncryption")));
        let cancel = CancellationToken::new();
        let stopper = {
            let c = cancel.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(900)).await;
                c.cancel();
            }
        };
        let client = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            run_cmd(
                Command::Run(opts(Mode::Client, "frpc.toml")),
                &fs,
                cancel.clone(),
            )
            .await
        };
        let edge = run_cmd(
            Command::Run(opts(Mode::Edge, "frps.toml")),
            &fs,
            cancel.clone(),
        );
        let (e, c, _) = tokio::join!(edge, client, stopper);
        assert_eq!((e, c), (0, 0));
    }

    #[tokio::test]
    async fn client_with_wrong_token_exits_with_failure_and_logs_skipped_proxies() {
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let fs = MemFs::new()
            .with("frps.toml", &format!("bindAddr='127.0.0.1'\nbindPort={port}\nauth.token='right'\n"))
            .with(
                "frpc.toml",
                &format!("serverAddr='127.0.0.1'\nserverPort={port}\nauth.token='wrong'\nlog.level='error'\n[[proxies]]\nname='w'\ntype='http'\nlocalPort=80\ncustomDomains=['a.com']\n"),
            );
        let cancel = CancellationToken::new();
        let client = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let code = run_cmd(
                Command::Run(opts(Mode::Client, "frpc.toml")),
                &fs,
                CancellationToken::new(),
            )
            .await;
            cancel.cancel();
            code
        };
        let edge = run_cmd(
            Command::Run(opts(Mode::Edge, "frps.toml")),
            &fs,
            cancel.clone(),
        );
        let (e, c) = tokio::join!(edge, client);
        assert_eq!((e, c), (0, 1));
    }

    #[tokio::test]
    async fn bind_failure_and_client_retry_exit_codes() {
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = occupied.local_addr().unwrap().port();
        let fs = MemFs::new()
            .with("frps.toml", &format!("bindAddr='127.0.0.1'\nbindPort={port}\n"))
            .with("frpc.toml", &format!("serverAddr='127.0.0.1'\nserverPort={port}\nlog.level='error'\ntransport.dialServerTimeout=1\n"));
        let (c, _, e) = exec(Command::Run(opts(Mode::Edge, "frps.toml")), &fs).await;
        assert!(c == 1 && !e.is_empty());
        // 对端占着端口却不会说话：client 握手超时后重试，直到被取消，退出码为 0。
        let cancel = CancellationToken::new();
        let stopper = {
            let c = cancel.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                c.cancel();
            }
        };
        let (code, _) = tokio::join!(
            run_cmd(Command::Run(opts(Mode::Client, "frpc.toml")), &fs, cancel),
            stopper
        );
        assert_eq!(code, 0);
        drop(occupied);
    }

    #[test]
    fn bad_log_target_falls_back_to_console() {
        let mut err = Vec::new();
        let _ = logger("/nonexistent-dir/x/y.log", "info", &mut err);
        assert!(String::from_utf8(err)
            .unwrap()
            .contains("falling back to console"));
    }
    fn node_opts(config: &str, token: Option<&str>, public: Option<&str>) -> Options {
        Options {
            mode: Mode::Edge,
            config: config.into(),
            managed: Some(managed::ManagedOpts {
                controller: "127.0.0.1:1".into(),
                enroll_token: token.map(str::to_string),
                name: None,
                state_dir: Some("/state".into()),
                ca_file: None,
                tls_pin: None,
                public_host: public.map(str::to_string),
            }),
            local_control: None,
            strict: true,
            allow_unsafe: vec![],
        }
    }

    #[tokio::test]
    async fn managed_edge_validates_its_inputs() {
        let fs = MemFs::new().with("bad.toml", "bindPort = 70000\n");
        for (o, want) in [
            (node_opts("", None, None), "--enroll-token"),
            (node_opts("", Some("t"), None), "--public-host"),
            (
                node_opts("missing.toml", Some("t"), Some("h")),
                "missing.toml",
            ),
            (node_opts("bad.toml", Some("t"), Some("h")), "bad.toml"),
        ] {
            let (c, _, e) = exec(Command::Run(o), &fs).await;
            assert_eq!(c, 1);
            assert!(e.contains(want), "{want}: {e}");
        }
        let mut o = node_opts("", Some("t"), Some("h"));
        o.managed.as_mut().unwrap().ca_file = Some("/no-ca.pem".into());
        assert!(exec(Command::Run(o.clone()), &fs)
            .await
            .2
            .contains("--ca-file"));
        o.managed.as_mut().unwrap().ca_file = None;
        o.managed.as_mut().unwrap().controller = "host:port".into();
        assert!(exec(Command::Run(o), &fs).await.2.contains("invalid port"));
    }

    #[tokio::test]
    async fn managed_edge_runs_from_a_cached_identity_without_the_controller() {
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        // 已接入的身份（含节点 ID）+ 带证书的 frps.toml + CA 文件 + 一条会产生提示的选项。
        let key = rustfrp_core::identity::DeviceKey::generate().unwrap();
        let (cert, private) = rustfrp_core::tls::self_signed_pem(vec!["rustfrp".into()]).unwrap();
        let fs = MemFs::new()
            .with(
                "frps.toml",
                &format!(
                    "bindAddr='127.0.0.1'\nbindPort={port}\nproxyBindAddr='127.0.0.1'\nauth.token='ignored'\nlog.level='error'\ntransport.tls.certFile='cert.pem'\ntransport.tls.keyFile='key.pem'\n"
                ),
            )
            .with("cert.pem", &cert)
            .with("key.pem", &private)
            .with("ca.pem", &cert);
        managed::save(&fs, "/state", &key, Some("node-1")).unwrap();
        let mut o = node_opts("frps.toml", None, None);
        o.managed.as_mut().unwrap().ca_file = Some("ca.pem".into());
        let cancel = CancellationToken::new();
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            stopper.cancel();
        });
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = execute(
            Command::Run(o),
            &fs,
            &MapEnv::default(),
            &mut out,
            &mut err,
            cancel,
        )
        .await;
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));

        // 没有 frps.toml、没有证书：在状态目录里生成并保存自签名证书，下次复用。
        let fs = MemFs::new();
        managed::save(&fs, "/state", &key, Some("node-1")).unwrap();
        for _ in 0..2 {
            let cancel = CancellationToken::new();
            let stopper = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                stopper.cancel();
            });
            let mut o = node_opts("", None, Some("edge.example.com"));
            // 默认监听 0.0.0.0:7000 可能被占用：用错误码验证「要么跑起来，要么明确报错」。
            let code = run_cmd(
                Command::Run(std::mem::replace(&mut o, node_opts("", None, None))),
                &fs,
                cancel,
            )
            .await;
            assert!(code == 0 || code == 1);
        }
        assert!(fs.read_to_string("/state/edge-cert.pem").is_ok());
        assert!(fs.read_to_string("/state/edge-key.pem").is_ok());
    }

    /// 读写都转给 `MemFs`，但可以让创建目录 / 写文件失败。
    struct FlakyFs {
        inner: MemFs,
        fail_dirs: bool,
        fail_writes: bool,
    }
    impl ConfigFs for FlakyFs {
        fn read_to_string(&self, path: &str) -> std::io::Result<String> {
            self.inner.read_to_string(path)
        }
        fn list_files(&self, dir: &str) -> std::io::Result<Vec<String>> {
            self.inner.list_files(dir)
        }
        fn dir_exists(&self, dir: &str) -> bool {
            self.inner.dir_exists(dir)
        }
        fn write_private(&self, path: &str, content: &str) -> std::io::Result<()> {
            if self.fail_writes {
                return Err(std::io::Error::other("read-only"));
            }
            self.inner.write_private(path, content)
        }
        fn create_dir_all(&self, dir: &str) -> std::io::Result<()> {
            if self.fail_dirs {
                return Err(std::io::Error::other("no space"));
            }
            self.inner.create_dir_all(dir)
        }
    }

    #[test]
    fn edge_identity_persistence_reports_each_failure() {
        let flaky = |fail_dirs, fail_writes| FlakyFs {
            inner: MemFs::new(),
            fail_dirs,
            fail_writes,
        };
        let err = |fs: &FlakyFs| {
            persistent_edge_identity(fs, "/state", Some("h"))
                .err()
                .unwrap()
        };
        assert!(err(&flaky(true, false)).contains("create /state"));
        assert!(err(&flaky(false, true)).contains("write /state/edge-key.pem"));
        // 证书文件损坏：明确指出是哪个文件。
        let broken = FlakyFs {
            inner: MemFs::new()
                .with("/state/edge-cert.pem", "not a cert")
                .with("/state/edge-key.pem", "not a key"),
            fail_dirs: false,
            fail_writes: false,
        };
        assert!(err(&broken).contains("/state/edge-cert.pem"));
        // 正常：生成、保存、再次读取得到同一证书。
        let ok = flaky(false, false);
        let first = persistent_edge_identity(&ok, "/state", None).unwrap();
        let second = persistent_edge_identity(&ok, "/state", None).unwrap();
        assert_eq!(
            first
                .certs
                .first()
                .map(rustfrp_core::tls::certificate_fingerprint),
            second
                .certs
                .first()
                .map(rustfrp_core::tls::certificate_fingerprint)
        );
    }

    #[tokio::test]
    async fn managed_edge_reports_unusable_files_and_unsaveable_identity() {
        // frps.toml 里的证书文件缺失 / 不是 PEM：明确报错。
        let fs = MemFs::new()
            .with(
                "missing-cert.toml",
                "transport.tls.certFile='nope.pem'\ntransport.tls.keyFile='nope.key'\n",
            )
            .with(
                "junk.toml",
                "transport.tls.certFile='junk.pem'\ntransport.tls.keyFile='junk.key'\n",
            )
            .with("junk.pem", "x")
            .with("junk.key", "x")
            .with("ca.toml", "bindPort=7000\n")
            .with("bad-ca.pem", "not pem");
        for cfg in ["missing-cert.toml", "junk.toml"] {
            let (c, _, e) = exec(Command::Run(node_opts(cfg, Some("t"), Some("h"))), &fs).await;
            assert_eq!(c, 1, "{cfg}");
            assert!(e.contains(cfg), "{e}");
        }
        let mut o = node_opts("ca.toml", Some("t"), Some("h"));
        o.managed.as_mut().unwrap().ca_file = Some("bad-ca.pem".into());
        let (c, _, e) = exec(Command::Run(o), &fs).await;
        assert_eq!(c, 1);
        assert!(!e.is_empty());
    }

    #[test]
    fn managed_edge_event_handling_persists_and_rejects_enrollment_failures() {
        let key = rustfrp_core::identity::DeviceKey::generate().unwrap();
        let log = Logger::silent();
        let cancel = CancellationToken::new();
        let failing = FlakyFs {
            inner: MemFs::new(),
            fail_dirs: false,
            fail_writes: true,
        };
        let mut err = Vec::new();
        assert_eq!(
            handle_node_event(
                Some(crate::node::LinkEvent::Enrolled("node-7".into())),
                NodeEventContext {
                    node_id_known: false,
                    fs: &failing,
                    dir: "/state",
                    key: &key,
                    log: &log,
                    cancel: &cancel,
                    err: &mut err,
                },
            ),
            NodeEventResult::Exit(1)
        );
        assert!(cancel.is_cancelled());
        assert!(String::from_utf8(err)
            .unwrap()
            .contains("managed edge identity"));

        let fs = MemFs::new();
        let keep_running = CancellationToken::new();
        assert_eq!(
            handle_node_event(
                Some(crate::node::LinkEvent::Enrolled("node-7".into())),
                NodeEventContext {
                    node_id_known: false,
                    fs: &fs,
                    dir: "/state",
                    key: &key,
                    log: &log,
                    cancel: &keep_running,
                    err: &mut Vec::new(),
                },
            ),
            NodeEventResult::Continue
        );
        assert_eq!(
            managed::load_or_create(&fs, "/state")
                .unwrap()
                .client_id
                .as_deref(),
            Some("node-7")
        );
        assert_eq!(
            handle_node_event(
                Some(crate::node::LinkEvent::Rejected("disabled".into())),
                NodeEventContext {
                    node_id_known: true,
                    fs: &fs,
                    dir: "/state",
                    key: &key,
                    log: &log,
                    cancel: &keep_running,
                    err: &mut Vec::new(),
                },
            ),
            NodeEventResult::Continue
        );
        assert_eq!(
            handle_node_event(
                None,
                NodeEventContext {
                    node_id_known: false,
                    fs: &fs,
                    dir: "/state",
                    key: &key,
                    log: &log,
                    cancel: &keep_running,
                    err: &mut Vec::new(),
                },
            ),
            NodeEventResult::Stop
        );
    }

    #[tokio::test]
    async fn the_service_command_is_reachable_from_execute() {
        // `--print` 只打印服务定义，不改动系统。
        let args: Vec<String> = [
            "service",
            "install",
            "--print",
            "--",
            "--mode",
            "edge",
            "-c",
            "frps.toml",
        ]
        .map(String::from)
        .to_vec();
        let cmd = crate::cli::parse_args("rustfrp-agent", &args).unwrap();
        let (c, o, _) = exec(cmd, &MemFs::new()).await;
        assert_eq!(c, 0);
        assert!(o.contains("frps.toml"), "{o}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sigterm_is_a_stop_signal() {
        let waiter = tokio::spawn(wait_for_stop());
        // 等信号处理器注册好再发信号，避免默认动作杀掉测试进程。
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let status = std::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn convert_emits_toml_and_reports_errors_without_mutating_source() {
        let fs=MemFs::new().with("legacy.ini","[common]\nserver_addr=localhost\n[web]\ntype=http\nlocal_port=80\ncustom_domains=example.com\nuse_encryption=true\nbandwidth_limit=1MB\n");
        let (code, text, report) =
            exec(Command::Convert(opts(Mode::Client, "legacy.ini")), &fs).await;
        assert_eq!(code, 0);
        assert!(text.contains("bandwidthLimit = \"1MB\""));
        assert!(report.contains("unsupported"));
        assert!(fs
            .read_to_string("legacy.ini")
            .unwrap()
            .contains("[common]"));
        let (code, _, error) = exec(Command::Convert(opts(Mode::Client, "missing.ini")), &fs).await;
        assert_eq!(code, 1);
        assert!(error.contains("convert"));
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("closed"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut error = Vec::new();
        let code = execute(
            Command::Convert(opts(Mode::Client, "legacy.ini")),
            &fs,
            &MapEnv::default(),
            &mut FailedWriter,
            &mut error,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(code, 1);
        assert!(String::from_utf8(error).unwrap().contains("convert output"));
    }
}
