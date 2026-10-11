//! 命令行：`run`（启动服务）与 `admin`（管理员账号维护，唯一的创建途径）。

use std::io::{BufRead, Write};

use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::db::{Db, StoreError};
use crate::models::Role;
use crate::{app, auth, store};
use rustfrp_agent::log::{Level, Logger};

/// 帮助文本。
pub const HELP: &str = "\
rustfrp-server — RustFRP controller (management API + embedded edge)

USAGE:
  rustfrp-server run
  rustfrp-server backup <new-file>
  rustfrp-server admin create <name> [--role owner|admin|operator|viewer]
  rustfrp-server admin list
  rustfrp-server admin disable <name>
  rustfrp-server admin enable <name>
  rustfrp-server admin passwd <name>
  rustfrp-server version

The password for `admin create` / `admin passwd` is read from RUSTFRP_ADMIN_PASSWORD,
or from the first line of standard input. There is no default administrator.

ENVIRONMENT:
  RUSTFRP_DB (rustfrp.db)   RUSTFRP_HTTP (127.0.0.1:8080)   RUSTFRP_EDGE (0.0.0.0:7000)
  RUSTFRP_PROXY_BIND (0.0.0.0)   RUSTFRP_ALLOW_PORTS (e.g. 6000-6100,7000)
  RUSTFRP_VHOST_HTTP / RUSTFRP_VHOST_HTTPS (ports for http / https tunnels)   RUSTFRP_SUBDOMAIN_HOST
  RUSTFRP_AUDIT_RETENTION_DAYS (90; 0 keeps all)   RUSTFRP_CLEANUP_BATCH (500; 1..1000)
  RUSTFRP_EDGE_TLS_CERT / RUSTFRP_EDGE_TLS_KEY (PEM files; configured together)
  RUSTFRP_PUBLIC_HOST   RUSTFRP_PUBLIC_URL   RUSTFRP_PANEL_DIR   RUSTFRP_SECURE_COOKIE   RUSTFRP_SESSION_HOURS
";

/// 子命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// 启动服务。
    Run,
    /// 在线一致性快照；目标必须不存在。
    Backup(String),
    /// 创建管理员。
    AdminCreate(String, Role),
    /// 列出管理员。
    AdminList,
    /// 禁用 / 启用管理员。
    AdminDisable(String, bool),
    /// 重置密码。
    AdminPasswd(String),
    /// 版本。
    Version,
    /// 帮助。
    Help,
}

/// 解析命令行（不含 argv[0]）。
pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        [] | ["help" | "-h" | "--help"] => Ok(Cmd::Help),
        ["version" | "-v" | "--version"] => Ok(Cmd::Version),
        ["run"] => Ok(Cmd::Run),
        ["backup", path] if !path.is_empty() => Ok(Cmd::Backup((*path).into())),
        ["admin", "list"] => Ok(Cmd::AdminList),
        ["admin", "create", name] => Ok(Cmd::AdminCreate((*name).into(), Role::Owner)),
        ["admin", "create", name, "--role", role] => match *role {
            "owner" | "admin" | "operator" | "viewer" => {
                Ok(Cmd::AdminCreate((*name).into(), Role::parse(role)))
            }
            other => Err(format!("unknown role \"{other}\"")),
        },
        ["admin", "disable", name] => Ok(Cmd::AdminDisable((*name).into(), true)),
        ["admin", "enable", name] => Ok(Cmd::AdminDisable((*name).into(), false)),
        ["admin", "passwd", name] => Ok(Cmd::AdminPasswd((*name).into())),
        other => Err(format!("unknown command: {}", other.join(" "))),
    }
}

fn read_password(
    env: &dyn Fn(&str) -> Option<String>,
    stdin: &mut dyn BufRead,
) -> Result<String, String> {
    if let Some(p) = env("RUSTFRP_ADMIN_PASSWORD") {
        return Ok(p);
    }
    let mut line = String::new();
    stdin.read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// 执行命令，返回退出码。`cancel` 用于停止 `run`。
pub async fn execute(
    cmd: Cmd,
    env: &dyn Fn(&str) -> Option<String>,
    stdin: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
    cancel: CancellationToken,
) -> i32 {
    let fail = |err: &mut dyn Write, m: String| {
        let _ = writeln!(err, "error: {m}");
        1
    };
    let cfg = match Config::from_lookup(env) {
        Ok(c) => c,
        Err(e) => return fail(err, e),
    };
    match cmd {
        Cmd::Help => {
            let _ = write!(out, "{HELP}");
            0
        }
        Cmd::Version => {
            let _ = writeln!(out, "rustfrp-server {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Cmd::Backup(destination) => {
            if let Err(e) = tokio::fs::metadata(&cfg.db_path).await {
                return fail(err, format!("source database: {e}"));
            }
            let result = backup_database(
                cfg.db_path.clone(),
                destination.clone().into(),
                Db::open_read_only,
            )
            .await;
            match result {
                Ok(()) => {
                    let _ = writeln!(out, "snapshot saved to {destination}");
                    0
                }
                Err(e) => fail(err, e.to_string()),
            }
        }
        Cmd::Run => {
            let log =
                Logger::from_target("console", Level::Info).unwrap_or_else(|_| Logger::silent());
            match app::start(cfg, log).await {
                Ok(server) => {
                    cancel.cancelled().await;
                    server.stop().await;
                    0
                }
                Err(e) => fail(err, e),
            }
        }
        admin => match admin_command(admin, &cfg, env, stdin, out).await {
            Ok(()) => 0,
            Err(e) => fail(err, e),
        },
    }
}

async fn backup_database(
    source: std::path::PathBuf,
    destination: std::path::PathBuf,
    open: fn(&std::path::Path) -> Result<Db, StoreError>,
) -> Result<(), StoreError> {
    match tokio::task::spawn_blocking(move || open(&source)).await {
        Ok(Ok(db)) => db.backup(destination).await,
        Ok(Err(e)) => Err(e),
        Err(e) => Err(StoreError::Sqlite(format!("backup worker failed: {e}"))),
    }
}

async fn admin_command(
    cmd: Cmd,
    cfg: &Config,
    env: &dyn Fn(&str) -> Option<String>,
    stdin: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<(), String> {
    let db = Db::open(&cfg.db_path).map_err(|e| format!("open database: {e}"))?;
    let store_err = |e: StoreError| e.to_string();
    match cmd {
        Cmd::AdminCreate(name, role) => {
            let pw = read_password(env, stdin)?;
            auth::check_password(&pw)?;
            let hash = auth::hash_password(&pw)?;
            let n = name.clone();
            let a = db
                .call(move |c| store::create_admin(c, &n, &hash, role, store::now_ms()))
                .await
                .map_err(store_err)?;
            let _ = writeln!(
                out,
                "created admin \"{}\" with role {}",
                a.name,
                a.role.as_str()
            );
        }
        Cmd::AdminList => {
            for a in db
                .call(|c| store::list_admins(c))
                .await
                .map_err(store_err)?
            {
                let state = if a.disabled_at.is_some() {
                    "disabled"
                } else {
                    "active"
                };
                let _ = writeln!(out, "{}\t{}\t{state}", a.name, a.role.as_str());
            }
        }
        Cmd::AdminDisable(name, disabled) => {
            let n = name.clone();
            db.call(move |c| store::set_admin_disabled(c, &n, disabled, store::now_ms()))
                .await
                .map_err(store_err)?;
            let _ = writeln!(
                out,
                "admin \"{name}\" {}",
                if disabled { "disabled" } else { "enabled" }
            );
        }
        Cmd::AdminPasswd(name) => {
            let pw = read_password(env, stdin)?;
            auth::check_password(&pw)?;
            let hash = auth::hash_password(&pw)?;
            let n = name.clone();
            db.call(move |c| store::set_admin_password(c, &n, &hash))
                .await
                .map_err(store_err)?;
            let _ = writeln!(
                out,
                "password updated for \"{name}\"; all of its sessions were revoked"
            );
        }
        Cmd::Run | Cmd::Backup(_) | Cmd::Version | Cmd::Help => {}
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Cmd, String> {
        parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[tokio::test]
    async fn backup_worker_failure_is_reported() {
        fn broken(_: &std::path::Path) -> Result<Db, StoreError> {
            panic!("injected reader failure");
        }
        let result = backup_database("unused-source".into(), "unused-target".into(), broken).await;
        assert!(matches!(result, Err(StoreError::Sqlite(e)) if e.contains("backup worker failed")));
        assert!(!std::path::Path::new("unused-target").exists());
    }

    #[test]
    fn parsing() {
        assert_eq!(p(&[]), Ok(Cmd::Help));
        assert_eq!(p(&["--help"]), Ok(Cmd::Help));
        assert_eq!(p(&["-v"]), Ok(Cmd::Version));
        assert_eq!(p(&["run"]), Ok(Cmd::Run));
        assert_eq!(p(&["admin", "list"]), Ok(Cmd::AdminList));
        assert_eq!(
            p(&["admin", "create", "bob"]),
            Ok(Cmd::AdminCreate("bob".into(), Role::Owner))
        );
        assert_eq!(
            p(&["admin", "create", "bob", "--role", "viewer"]),
            Ok(Cmd::AdminCreate("bob".into(), Role::Viewer))
        );
        assert!(p(&["admin", "create", "bob", "--role", "root"])
            .unwrap_err()
            .contains("unknown role"));
        assert_eq!(
            p(&["admin", "disable", "bob"]),
            Ok(Cmd::AdminDisable("bob".into(), true))
        );
        assert_eq!(
            p(&["admin", "enable", "bob"]),
            Ok(Cmd::AdminDisable("bob".into(), false))
        );
        assert_eq!(
            p(&["admin", "passwd", "bob"]),
            Ok(Cmd::AdminPasswd("bob".into()))
        );
        assert!(p(&["bogus"]).unwrap_err().contains("unknown command"));
    }

    #[test]
    fn password_sources() {
        let mut empty: &[u8] = b"";
        assert_eq!(
            read_password(&|_| Some("from-env".into()), &mut empty).unwrap(),
            "from-env"
        );
        let mut stdin: &[u8] = b"from-stdin\r\nsecond";
        assert_eq!(read_password(&|_| None, &mut stdin).unwrap(), "from-stdin");
        struct Bad;
        impl std::io::Read for Bad {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("boom"))
            }
        }
        assert!(read_password(&|_| None, &mut std::io::BufReader::new(Bad)).is_err());
    }
}
