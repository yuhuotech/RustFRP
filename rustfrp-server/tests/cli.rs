//! 命令行：管理员维护命令与 `run`。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::time::Duration;

use rustfrp_server::cli::{execute, parse, Cmd, HELP};
use tokio_util::sync::CancellationToken;

struct Env(HashMap<String, String>);
impl Env {
    fn new(db: &std::path::Path, extra: &[(&str, &str)]) -> Env {
        let mut m = HashMap::new();
        m.insert("RUSTFRP_DB".to_string(), db.display().to_string());
        for (k, v) in extra {
            m.insert(k.to_string(), v.to_string());
        }
        Env(m)
    }
}

async fn run(args: &[&str], env: &Env, stdin: &str) -> (i32, String, String) {
    let cmd = parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut input = stdin.as_bytes();
    let code = execute(
        cmd,
        &|k| env.0.get(k).cloned(),
        &mut input,
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

fn tmp_db() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "rustfrp-cli-{}-{}.db",
        std::process::id(),
        rustfrp_server::store::new_id()
    ))
}

fn cleanup(p: &std::path::Path) {
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
    }
}

#[tokio::test]
async fn admin_lifecycle() {
    let db = tmp_db();
    let env = Env::new(&db, &[]);

    let (c, o, _) = run(
        &["admin", "create", "root"],
        &env,
        "a-long-enough-password\n",
    )
    .await;
    assert_eq!(c, 0);
    assert!(o.contains("created admin \"root\" with role owner"));
    let (c, o, _) = run(
        &["admin", "create", "view", "--role", "viewer"],
        &env,
        "another-long-password\n",
    )
    .await;
    assert!(c == 0 && o.contains("role viewer"));

    let (c, _, e) = run(
        &["admin", "create", "root"],
        &env,
        "a-long-enough-password\n",
    )
    .await;
    assert!(c == 1 && e.contains("already exists"), "{e}");
    let (c, _, e) = run(&["admin", "create", "weak"], &env, "short\n").await;
    assert!(c == 1 && e.contains("at least"), "{e}");

    let (_, o, _) = run(&["admin", "list"], &env, "").await;
    assert!(o.contains("root\towner\tactive") && o.contains("view\tviewer\tactive"));

    let (c, o, _) = run(&["admin", "disable", "view"], &env, "").await;
    assert!(c == 0 && o.contains("disabled"));
    assert!(run(&["admin", "list"], &env, "")
        .await
        .1
        .contains("view\tviewer\tdisabled"));
    assert!(run(&["admin", "enable", "view"], &env, "")
        .await
        .1
        .contains("enabled"));
    assert_eq!(run(&["admin", "disable", "ghost"], &env, "").await.0, 1);

    let (c, o, _) = run(&["admin", "passwd", "root"], &env, "brand-new-password\n").await;
    assert!(c == 0 && o.contains("sessions were revoked"));
    assert_eq!(
        run(&["admin", "passwd", "ghost"], &env, "brand-new-password\n")
            .await
            .0,
        1
    );
    assert_eq!(
        run(&["admin", "passwd", "root"], &env, "short\n").await.0,
        1
    );

    // 密码也可以来自环境变量。
    let env2 = Env::new(&db, &[("RUSTFRP_ADMIN_PASSWORD", "from-the-environment")]);
    assert_eq!(run(&["admin", "create", "envuser"], &env2, "").await.0, 0);
    cleanup(&db);
}

#[tokio::test]
async fn help_version_and_bad_environment() {
    let env = Env::new(&tmp_db(), &[]);
    let (c, o, _) = run(&[], &env, "").await;
    assert!(c == 0 && o == HELP);
    let (c, o, _) = run(&["version"], &env, "").await;
    assert!(c == 0 && o.starts_with("rustfrp-server "));
    let bad = Env::new(&tmp_db(), &[("RUSTFRP_ALLOW_PORTS", "oops")]);
    let (c, _, e) = run(&["run"], &bad, "").await;
    assert!(c == 1 && e.contains("invalid port range"), "{e}");
    assert_eq!(parse(&["run".to_string()]), Ok(Cmd::Run));
    let unwritable = Env::new(std::path::Path::new("/nonexistent-dir/x/y.db"), &[]);
    assert_eq!(run(&["admin", "list"], &unwritable, "").await.0, 1);
}

#[tokio::test]
async fn run_starts_and_stops_on_cancel() {
    let db = tmp_db();
    let env = Env::new(
        &db,
        &[
            ("RUSTFRP_HTTP", "127.0.0.1:0"),
            ("RUSTFRP_EDGE", "127.0.0.1:0"),
        ],
    );
    let cancel = CancellationToken::new();
    let stopper = {
        let c = cancel.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            c.cancel();
        }
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut input: &[u8] = b"";
    let lookup = |k: &str| env.0.get(k).cloned();
    let go = execute(Cmd::Run, &lookup, &mut input, &mut out, &mut err, cancel);
    let (code, _) = tokio::join!(go, stopper);
    assert_eq!(code, 0);

    // 端口被占用时 run 以失败退出。
    let busy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let env = Env::new(
        &db,
        &[
            ("RUSTFRP_HTTP", &busy.local_addr().unwrap().to_string()),
            ("RUSTFRP_EDGE", "127.0.0.1:0"),
        ],
    );
    let lookup2 = |k: &str| env.0.get(k).cloned();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = execute(
        Cmd::Run,
        &lookup2,
        &mut input,
        &mut out,
        &mut err,
        CancellationToken::new(),
    )
    .await;
    assert_eq!(code, 1);
    cleanup(&db);
}
