//! 命令执行：校验、打印兼容性报告、运行客户端或边缘节点。

use std::io::Write;

use rustfrp_core::frpcfg::validate::{validate_client_all, validate_server};
use rustfrp_core::frpcfg::{
    load_client, load_server, CompatReport, ConfigFs, Env, LoadOptions, Severity,
};
use tokio_util::sync::CancellationToken;

use crate::cli::{Command, Mode, Options, HELP};
use crate::edge::Edge;
use crate::log::{Level, Logger};
use crate::{client, managed, plan};

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
        Command::Verify(o) => verify(&o, fs, env, out, err),
        Command::Run(o) => run(&o, fs, env, out, err, cancel).await,
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
) -> i32 {
    if let Some(m) = &o.managed {
        return run_managed(m, fs, env, err, cancel).await;
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
            for n in &plan.notes {
                log.warn(n);
            }
            match client::run(plan.cfg, log, None, cancel).await {
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
            for n in &plan.notes {
                log.warn(n);
            }
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

/// 受管客户端：身份来自状态目录，配置由 Controller 下发。
async fn run_managed(
    m: &managed::ManagedOpts,
    fs: &dyn ConfigFs,
    env: &dyn Env,
    err: &mut dyn Write,
    cancel: CancellationToken,
) -> i32 {
    let prepared = managed::state_dir(m, env).and_then(|dir| {
        let id = managed::load_or_create(fs, &dir)?;
        let cfg = managed::client_config(m, &id, fs, env)?;
        Ok((dir, id, cfg))
    });
    let (dir, id, cfg) = match prepared {
        Ok(v) => v,
        Err(e) => return fail(err, "managed", e),
    };
    let log = Logger::new(Level::Info, Box::new(std::io::stderr()));
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let key = id.key.clone();
    let persist = async {
        while let Some(ev) = rx.recv().await {
            match ev {
                client::Event::Enrolled { client_id } => {
                    match managed::save(fs, &dir, &key, Some(&client_id)) {
                        Ok(()) => log.info(&format!("enrolled as device [{client_id}]")),
                        Err(e) => log.error(&e),
                    }
                }
                client::Event::ConfigApplied { revision, proxies } => log.info(&format!(
                    "applied config revision {revision} ({proxies} tunnels)"
                )),
                client::Event::Disconnected(r) => log.warn(&format!("disconnected: {r}")),
                _ => {}
            }
        }
    };
    let run = client::run(cfg, log.clone(), Some(tx), cancel);
    let (result, ()) = tokio::join!(run, persist);
    match result {
        Ok(()) => 0,
        Err(e) => fail(err, "managed", e),
    }
}

fn fail(err: &mut dyn Write, config: &str, e: impl std::fmt::Display) -> i32 {
    let _ = writeln!(err, "{config}: {e}");
    1
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use rustfrp_core::frpcfg::{MapEnv, MemFs};
    use std::time::Duration;

    fn opts(mode: Mode, config: &str) -> Options {
        Options {
            mode,
            config: config.into(),
            managed: None,
            strict: true,
            allow_unsafe: vec![],
        }
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
            .with("frps.toml", &format!("bindAddr='127.0.0.1'\nbindPort={port}\nvhostHTTPPort=80\n"))
            .with(
                "frpc.toml",
                &format!("serverAddr='127.0.0.1'\nserverPort={port}\nlog.level='error'\n[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\nremotePort=0\n"),
            );
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
}
