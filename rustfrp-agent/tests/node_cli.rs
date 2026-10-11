//! `rustfrp-agent --mode edge --controller …`：用假 Controller 验证接入、身份落盘、签名重连、设备缓存与错误处理。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustfrp_agent::app;
use rustfrp_agent::cli::{parse_args, Command};
use rustfrp_core::frpcfg::{StdEnv, StdFs};
use rustfrp_core::identity::verify_challenge;
use rustfrp_core::node::{CtlMsg, NodeDevice, NodeMsg};
use rustfrp_core::tls;
use rustfrp_core::wire::{read_frame, write_frame, ConfigSet, ManagedProxy};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

/// 假 Controller 观察到的事情。
#[derive(Default)]
struct Seen {
    hellos: Vec<rustfrp_core::node::NodeHello>,
    messages: Vec<NodeMsg>,
    verified_signature: bool,
}

async fn fake_controller(
    seen: Arc<Mutex<Seen>>,
    device: NodeDevice,
) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(
        tls::server_config(tls::self_signed(vec!["rustfrp".into()]).unwrap()).unwrap(),
    );
    let task = tokio::spawn(async move {
        let mut node_pubkey = String::new();
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let Ok(tls) = acceptor.accept(tcp).await else {
                continue;
            };
            let (mut rd, mut wr) = tokio::io::split(tls);
            write_frame(
                &mut wr,
                &CtlMsg::Challenge {
                    nonce: "nonce-1".into(),
                },
            )
            .await
            .unwrap();
            let Ok(NodeMsg::Hello(hello)) = read_frame::<_, NodeMsg>(&mut rd).await else {
                continue;
            };
            seen.lock().unwrap().hellos.push(hello.clone());
            if let Some(en) = &hello.enroll {
                if en.token != "good" {
                    write_frame(
                        &mut wr,
                        &CtlMsg::HelloResp {
                            ok: false,
                            error: "bad token".into(),
                            node_id: String::new(),
                        },
                    )
                    .await
                    .unwrap();
                    continue;
                }
                node_pubkey = en.pubkey.clone();
                write_frame(
                    &mut wr,
                    &CtlMsg::HelloResp {
                        ok: true,
                        error: String::new(),
                        node_id: "node-7".into(),
                    },
                )
                .await
                .unwrap();
                continue;
            }
            let ok = verify_challenge(&node_pubkey, &hello.node_id, "nonce-1", &hello.signature);
            seen.lock().unwrap().verified_signature = ok;
            write_frame(
                &mut wr,
                &CtlMsg::HelloResp {
                    ok,
                    error: String::new(),
                    node_id: hello.node_id.clone(),
                },
            )
            .await
            .unwrap();
            if !ok {
                continue;
            }
            write_frame(&mut wr, &CtlMsg::SnapshotBegin).await.unwrap();
            write_frame(&mut wr, &CtlMsg::Device(device.clone()))
                .await
                .unwrap();
            write_frame(&mut wr, &CtlMsg::SnapshotEnd).await.unwrap();
            // 一直读到连接关闭，把节点发来的消息记下来。
            while let Ok(m) = read_frame::<_, NodeMsg>(&mut rd).await {
                if m == NodeMsg::Ping {
                    write_frame(&mut wr, &CtlMsg::Pong).await.unwrap();
                }
                seen.lock().unwrap().messages.push(m);
            }
        }
    });
    (port, task)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "rfrp-node-cli-{}-{}",
        std::process::id(),
        free_port()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn run_agent(args: Vec<String>, run_for: Duration) -> (i32, String) {
    let cmd = parse_args("rustfrp-agent", &args).unwrap();
    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(run_for).await;
        stopper.cancel();
    });
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = app::execute(cmd, &StdFs, &StdEnv, &mut out, &mut err, cancel).await;
    (code, String::from_utf8_lossy(&err).into_owned())
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(ToString::to_string).collect()
}

#[tokio::test]
async fn enroll_persist_reconnect_and_cache_devices() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let device = NodeDevice {
        id: "dev-1".into(),
        pubkey: "ab".repeat(32),
        disabled: false,
        config: ConfigSet {
            revision: 2,
            proxies: vec![ManagedProxy {
                name: "t".into(),
                kind: "tcp".into(),
                local_port: 22,
                ..Default::default()
            }],
            ..Default::default()
        },
    };
    let (ctl_port, _task) = fake_controller(seen.clone(), device).await;
    let state = dir();
    let listen = free_port();
    let frps = state.join("frps.toml");
    std::fs::write(&frps, format!("bindAddr = \"127.0.0.1\"\nbindPort = {listen}\nproxyBindAddr = \"127.0.0.1\"\nauth.token = \"ignored\"\n")).unwrap();
    let controller = format!("127.0.0.1:{ctl_port}");
    let common = |extra: &[&str]| {
        let mut v = args(&[
            "--mode",
            "edge",
            "--controller",
            &controller,
            "--state-dir",
            state.to_str().unwrap(),
            "-c",
            frps.to_str().unwrap(),
        ]);
        v.extend(args(extra));
        v
    };

    // 首次运行缺少令牌 / 缺少 public-host：明确报错，不留下半截状态。
    let (code, err) = run_agent(common(&[]), T).await;
    assert_eq!(code, 1);
    assert!(err.contains("--enroll-token"), "{err}");
    let (code, err) = run_agent(common(&["--enroll-token", "good"]), T).await;
    assert_eq!(code, 1);
    assert!(err.contains("--public-host"), "{err}");
    // 令牌被拒绝：非零退出并带原因。
    let (code, err) = run_agent(
        common(&[
            "--enroll-token",
            "nope",
            "--public-host",
            "edge.example.com",
        ]),
        T,
    )
    .await;
    assert_eq!(code, 1);
    assert!(err.contains("bad token"), "{err}");
    assert!(
        !state.join("identity.json").exists()
            || !std::fs::read_to_string(state.join("identity.json"))
                .unwrap()
                .contains("node-7")
    );

    // 正常接入：保存节点 ID，随后用挑战签名重连并收到设备清单。
    let (code, _) = run_agent(
        common(&[
            "--enroll-token",
            "good",
            "--public-host",
            "edge.example.com",
        ]),
        Duration::from_millis(1500),
    )
    .await;
    assert_eq!(code, 0);
    let identity = std::fs::read_to_string(state.join("identity.json")).unwrap();
    assert!(identity.contains("node-7"), "{identity}");
    assert!(state.join("edge-cert.pem").exists() && state.join("edge-key.pem").exists());
    {
        let s = seen.lock().unwrap();
        assert!(s
            .hellos
            .iter()
            .any(|h| h.enroll.is_some() && h.info.public_host == "edge.example.com"));
        let signed = s.hellos.iter().find(|h| h.enroll.is_none()).unwrap();
        assert_eq!(signed.node_id, "node-7");
        assert_eq!(signed.info.edge_port, listen);
        assert_eq!(signed.info.tls_fingerprint.len(), 64);
        assert!(
            s.verified_signature,
            "the signature over the live challenge verifies"
        );
    }
    // 设备清单被持久化；之后即使 Controller 不可达，重启也能用缓存。
    let cache = std::fs::read_to_string(state.join("devices.json")).unwrap();
    assert!(cache.contains("dev-1"), "{cache}");
    // 第二次运行不再需要令牌；控制端口不可达也能启动并服务（只是重试连接）。
    let unreachable = format!("127.0.0.1:{}", free_port());
    let (code, _) = run_agent(
        args(&[
            "--mode",
            "edge",
            "--controller",
            &unreachable,
            "--state-dir",
            state.to_str().unwrap(),
            "-c",
            frps.to_str().unwrap(),
        ]),
        Duration::from_millis(800),
    )
    .await;
    assert_eq!(code, 0, "an enrolled node starts without the controller");

    std::fs::remove_dir_all(&state).unwrap();
}

#[tokio::test]
async fn bad_inputs_are_rejected_before_anything_starts() {
    let state = dir();
    let base = |extra: &[&str]| {
        let mut v = args(&[
            "--mode",
            "edge",
            "--state-dir",
            state.to_str().unwrap(),
            "--enroll-token",
            "t",
            "--public-host",
            "h",
        ]);
        v.extend(args(extra));
        v
    };
    for (extra, want) in [
        (vec!["--controller", "host:notaport"], "invalid port"),
        (
            vec!["--controller", "h:1", "--ca-file", "/no/such/ca.pem"],
            "--ca-file",
        ),
        (
            vec!["--controller", "h:1", "-c", "/no/such/frps.toml"],
            "frps.toml",
        ),
        (vec!["--controller", "h:1", "--tls-pin", "short"], "pin"),
    ] {
        let cmd = parse_args("rustfrp-agent", &base(&extra)).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = timeout(
            T,
            app::execute(
                cmd,
                &StdFs,
                &StdEnv,
                &mut out,
                &mut err,
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
        let err = String::from_utf8_lossy(&err).into_owned();
        assert_eq!(code, 1, "{extra:?}: {err}");
        assert!(err.contains(want), "{extra:?}: {err}");
    }
    assert!(matches!(
        parse_args("rustfrp-agent", &base(&["--controller", "h"])),
        Ok(Command::Run(_))
    ));
    let _ = std::fs::remove_dir_all(&state);
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_stops_a_running_agent() {
    let waiter = tokio::spawn(app::wait_for_stop());
    // 等信号处理器注册好再发信号，否则默认动作会终止测试进程。
    tokio::time::sleep(Duration::from_millis(200)).await;
    let status = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    timeout(T, waiter).await.unwrap().unwrap();
}
