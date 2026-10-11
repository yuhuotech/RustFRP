//! 独立客户端的管理接口（webServer）：status / reload / stop 与对应命令行。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::Duration;

use rustfrp_agent::admin;
use rustfrp_agent::app;
use rustfrp_agent::cli::{parse_args, AdminAction, Command, Mode, Options};
use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use rustfrp_core::frpcfg::{ConfigFs, MapEnv, MemFs};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn echo() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 256];
                while let Ok(n) = s.read(&mut b).await {
                    if n == 0 || s.write_all(&b[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

fn config(edge: u16, web: u16, echo: u16, extra_proxy: &str, web_extra: &str) -> String {
    format!(
        "serverAddr = \"127.0.0.1\"\nserverPort = {edge}\nwebServer.addr = \"127.0.0.1\"\nwebServer.port = {web}\nwebServer.user = \"admin\"\nwebServer.password = \"s3cret\"\n{web_extra}\n\
[[proxies]]\nname = \"first\"\ntype = \"tcp\"\nlocalPort = {echo}\n{extra_proxy}"
    )
}

fn options(config: &str) -> Options {
    Options {
        mode: Mode::Client,
        config: config.into(),
        managed: None,
        local_control: None,
        strict: true,
        allow_unsafe: vec![],
    }
}

async fn http(web: u16, path: &str, auth: bool) -> (u16, String) {
    let (user, pass) = if auth { ("admin", "s3cret") } else { ("", "") };
    admin::request(&format!("127.0.0.1:{web}"), path, user, pass)
        .await
        .unwrap()
}

async fn eventually<F: std::future::Future<Output = bool>>(mut f: impl FnMut() -> F) {
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("condition not reached");
}

#[tokio::test]
async fn status_reload_and_stop_through_the_admin_interface_and_cli() {
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
    let edge_port = edge.local_addr().unwrap().port();
    let stop_edge = CancellationToken::new();
    tokio::spawn(edge.serve(stop_edge.clone()));
    let (web, echo_port) = (free_port(), echo().await);
    let fs = Arc::new(MemFs::new().with("frpc.toml", &config(edge_port, web, echo_port, "", "")));
    let cancel = CancellationToken::new();
    let run = {
        let (fs, cancel) = (fs.clone(), cancel.clone());
        async move {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = app::execute(
                Command::Run(options("frpc.toml")),
                &*fs,
                &MapEnv::default(),
                &mut out,
                &mut err,
                cancel,
            )
            .await;
            (code, String::from_utf8_lossy(&err).into_owned())
        }
    };
    let driver = async {
        eventually(|| async {
            admin::request(&format!("127.0.0.1:{web}"), "/healthz", "", "")
                .await
                .is_ok_and(|(c, _)| c == 200)
        })
        .await;
        // 健康检查不需要认证；其余接口需要。
        assert_eq!(http(web, "/api/status", false).await.0, 401);
        assert_eq!(
            admin::request(&format!("127.0.0.1:{web}"), "/api/status", "admin", "wrong")
                .await
                .unwrap()
                .0,
            401
        );
        assert_eq!(http(web, "/nope", true).await.0, 404);
        eventually(|| async {
            http(web, "/api/status", true)
                .await
                .1
                .contains("\"running\"")
        })
        .await;
        let (_, status) = http(web, "/api/status", true).await;
        assert!(
            status.contains("\"tcp\":[{") && status.contains("\"name\":\"first\""),
            "{status}"
        );
        assert!(http(web, "/api/config", true)
            .await
            .1
            .contains("name = \"first\""));

        // 重载：新增一个代理；无效配置被拒绝，旧配置继续运行。
        fs.write_private(
            "frpc.toml",
            &config(
                edge_port,
                web,
                echo_port,
                "[[proxies]]\nname = \"second\"\ntype = \"tcp\"\nlocalPort = 1\n",
                "",
            ),
        )
        .unwrap();
        assert_eq!(http(web, "/api/reload", true).await.0, 200);
        eventually(|| async {
            let s = http(web, "/api/status", true).await.1;
            s.contains("\"second\"") && s.matches("\"running\"").count() == 2
        })
        .await;
        assert!(http(web, "/api/config", true).await.1.contains("second"));
        fs.write_private("frpc.toml", "serverPort = \"oops\"")
            .unwrap();
        let (code, body) = http(web, "/api/reload", true).await;
        assert_eq!(code, 400, "{body}");
        assert!(http(web, "/api/status", true)
            .await
            .1
            .contains("\"second\""));
        fs.write_private(
            "frpc.toml",
            &config(
                edge_port,
                web,
                echo_port,
                "",
                "webServer.tls.certFile = \"x\"\nwebServer.tls.keyFile = \"y\"\n",
            ),
        )
        .unwrap();

        // 命令行：status / reload / stop 都经管理接口。
        let admin_cmd = |action: AdminAction| {
            let fs = fs.clone();
            async move {
                let (mut out, mut err) = (Vec::new(), Vec::new());
                let code = app::execute(
                    Command::Admin(action, options("frpc.toml")),
                    &*fs,
                    &MapEnv::default(),
                    &mut out,
                    &mut err,
                    CancellationToken::new(),
                )
                .await;
                (
                    code,
                    String::from_utf8_lossy(&out).into_owned(),
                    String::from_utf8_lossy(&err).into_owned(),
                )
            }
        };
        let (code, out, _) = admin_cmd(AdminAction::Status).await;
        assert_eq!(code, 0);
        assert!(out.contains("\"first\""), "{out}");
        let (code, out, _) = admin_cmd(AdminAction::Reload).await;
        assert_eq!((code, out.trim()), (0, "reload success"));
        // 配置文件里没有 webServer：命令行直接报错。
        fs.write_private("other.toml", "serverAddr = \"127.0.0.1\"\n")
            .unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = app::execute(
            Command::Admin(AdminAction::Status, options("other.toml")),
            &*fs,
            &MapEnv::default(),
            &mut out,
            &mut err,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(code, 1);
        assert!(String::from_utf8_lossy(&err).contains("webServer.port is not configured"));
        // 客户端不在运行 / 密码错误：命令失败并给出原因。
        fs.write_private(
            "wrong.toml",
            "webServer.port = 1\nwebServer.addr = \"0.0.0.0\"\n",
        )
        .unwrap();
        let code = app::execute(
            Command::Admin(AdminAction::Stop, options("wrong.toml")),
            &*fs,
            &MapEnv::default(),
            &mut out,
            &mut err,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(code, 1);
        // 密码错误：服务端 401，命令行失败并带上原因。
        fs.write_private(
        "badpass.toml",
        &format!("webServer.port = {web}\nwebServer.user = \"admin\"\nwebServer.password = \"nope\"\n"),
    )
    .unwrap();
        let mut err2 = Vec::new();
        let code = app::execute(
            Command::Admin(AdminAction::Status, options("badpass.toml")),
            &*fs,
            &MapEnv::default(),
            &mut out,
            &mut err2,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(code, 1);
        assert!(String::from_utf8_lossy(&err2).contains("HTTP 401"));
        fs.write_private("missing-file", "").unwrap();
        let code = app::execute(
            Command::Admin(AdminAction::Status, options("does-not-exist.toml")),
            &*fs,
            &MapEnv::default(),
            &mut out,
            &mut err,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(code, 1);

        let (code, out, _) = admin_cmd(AdminAction::Stop).await;
        assert_eq!((code, out.trim()), (0, "stop success"));
    };
    let (finished, ()) =
        tokio::time::timeout(Duration::from_secs(40), async { tokio::join!(run, driver) })
            .await
            .unwrap();
    assert_eq!(
        finished.0, 0,
        "stop ends the client cleanly: {}",
        finished.1
    );
    stop_edge.cancel();
    let _ = cancel;
}

#[test]
fn command_line_parsing_and_basic_credentials() {
    let a = |args: &[&str]| {
        parse_args(
            "frpc",
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    };
    for (word, action) in [
        ("reload", AdminAction::Reload),
        ("status", AdminAction::Status),
        ("stop", AdminAction::Stop),
    ] {
        assert_eq!(
            a(&[word, "-c", "x.toml"]),
            Ok(Command::Admin(action, options("x.toml")))
        );
    }
    assert!(a(&["reload", "verify", "-c", "x"]).is_err());
    assert!(parse_args(
        "frps",
        &["reload".to_string(), "-c".to_string(), "x".to_string()]
    )
    .is_err());
    assert!(a(&["reload", "--controller", "h"]).is_err());
}

#[tokio::test]
async fn reload_is_refused_when_the_client_loop_is_gone() {
    // 请求端遇到连不上的地址：返回连接错误而不是挂起。
    let dead = free_port();
    let err = admin::request(&format!("127.0.0.1:{dead}"), "/api/status", "", "")
        .await
        .unwrap_err();
    assert!(err.contains("connect"), "{err}");
    // 服务端没有任何重载通道接收者：返回 400 而不是挂起。
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);
    let cancel = CancellationToken::new();
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let a = Arc::new(admin::Admin {
        user: String::new(),
        password: String::new(),
        status: Arc::default(),
        config_text: Arc::default(),
        reload: tx,
        stop: cancel.clone(),
    });
    tokio::spawn(admin::serve(l, a, cancel.clone()));
    let (code, body) = admin::request(&format!("127.0.0.1:{port}"), "/api/reload", "", "")
        .await
        .unwrap();
    assert_eq!(code, 400, "{body}");
    // 接收端取走请求却不应答（丢弃 reply）：同样得到 400，而不是一直等。
    let (tx2, mut rx2) = tokio::sync::mpsc::channel::<admin::ReloadReq>(1);
    tokio::spawn(async move { while let Some(_req) = rx2.recv().await {} });
    let l2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port2 = l2.local_addr().unwrap().port();
    tokio::spawn(admin::serve(
        l2,
        Arc::new(admin::Admin {
            user: String::new(),
            password: String::new(),
            status: Arc::default(),
            config_text: Arc::default(),
            reload: tx2,
            stop: cancel.clone(),
        }),
        cancel.clone(),
    ));
    let (code, body) = admin::request(&format!("127.0.0.1:{port2}"), "/api/reload", "", "")
        .await
        .unwrap();
    assert_eq!((code, body.as_str()), (400, "reload was abandoned"));
    // 垃圾请求：400；超大请求头：连接被关闭。
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(b"GARBAGE\r\n\r\n").await.unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await;
    assert!(out.starts_with("HTTP/1.1 400"), "{out}");
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(&vec![b'a'; 20_000]).await.unwrap();
    let mut out = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), s.read_to_string(&mut out)).await;
    assert!(out.is_empty());
    cancel.cancel();
}

#[tokio::test]
async fn run_with_reload_applies_new_configs_and_survives_a_closed_channel() {
    use rustfrp_agent::client::{self, ClientConfig, Event};
    use tokio::sync::mpsc;
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
    let stop = CancellationToken::new();
    tokio::spawn(edge.serve(stop.clone()));
    let mk = |name: &str| ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: port,
        proxies: vec![client::ProxyCfg {
            name: name.into(),
            kind: "tcp".into(),
            local_ip: "127.0.0.1".into(),
            local_port: 1,
            ..Default::default()
        }],
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    };
    let (etx, mut erx) = mpsc::channel(32);
    let (rtx, rrx) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let task = tokio::spawn(client::run_with_reload(
        mk("one"),
        Logger::silent(),
        Some(etx),
        cancel.clone(),
        Some(rrx),
    ));
    async fn started(name: &str, rx: &mut mpsc::Receiver<Event>) {
        loop {
            let e = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(&e, Event::ProxyStarted { name: n, .. } if n == name) {
                return;
            }
        }
    }
    started("one", &mut erx).await;
    // 新配置替换旧配置；CA 文件无法解析的配置被拒绝，当前配置继续运行。
    rtx.send(mk("two")).await.unwrap();
    started("two", &mut erx).await;
    let mut bad = mk("three");
    bad.trusted_ca_pem = Some(b"not a pem".to_vec());
    rtx.send(bad).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // 重载通道关闭后继续按当前配置运行，直到被取消。
    drop(rtx);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!task.is_finished());
    cancel.cancel();
    assert!(tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .is_ok());
    stop.cancel();
}

#[tokio::test]
async fn admin_defaults_to_loopback_without_authentication() {
    let web = free_port();
    let fs = MemFs::new().with(
        "frpc.toml",
        &format!("serverAddr = \"127.0.0.1\"\nserverPort = 1\nwebServer.port = {web}\n"),
    );
    let cancel = CancellationToken::new();
    let run = async {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        app::execute(
            Command::Run(options("frpc.toml")),
            &fs,
            &MapEnv::default(),
            &mut out,
            &mut err,
            cancel.clone(),
        )
        .await
    };
    let driver = async {
        // 没有配置 webServer.addr：只监听回环；没有认证。
        eventually(|| async {
            admin::request(&format!("127.0.0.1:{web}"), "/healthz", "", "")
                .await
                .is_ok_and(|(c, _)| c == 200)
        })
        .await;
        assert_eq!(http(web, "/api/status", false).await.0, 200);
        // 重载成功后客户端循环仍在；然后停止。
        assert_eq!(http(web, "/api/reload", false).await.0, 200);
        assert_eq!(http(web, "/api/stop", false).await.0, 200);
    };
    let (code, ()) =
        tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(run, driver) })
            .await
            .unwrap();
    assert_eq!(code, 0);
}
