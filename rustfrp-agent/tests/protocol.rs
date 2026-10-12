//! 对端「不守规矩」时的行为：用原始协议帧扮演客户端或边缘节点，覆盖错误路径。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::future::Future;
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, Event, ProxyCfg};
use rustfrp_agent::edge::{Admission, BoxFut, Edge, EdgeConfig, Policy};
use rustfrp_agent::log::Logger;
use rustfrp_core::auth::{auth_key, unix_now};
use rustfrp_core::mux::{self, MuxConfig, Session, Stream};
use rustfrp_core::tls;
use rustfrp_core::wire::{
    read_frame, write_frame, ClientMsg, EdgeRedirect, HealthSpec, LbOpts, Login, ProxySpec,
    ServerMsg, StartWork, PROTOCOL_VERSION,
};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

// ---------- 扮演客户端，连真实的 Edge ----------

async fn start_edge(cfg: EdgeConfig) -> (u16, CancellationToken) {
    let cfg = EdgeConfig {
        bind_addr: "127.0.0.1".into(),
        bind_port: 0,
        proxy_bind_addr: "127.0.0.1".into(),
        ..cfg
    };
    let edge = Edge::bind(cfg, None, Logger::silent()).await.unwrap();
    let port = edge.local_addr().unwrap().port();
    let stop = CancellationToken::new();
    tokio::spawn(edge.serve(stop.clone()));
    (port, stop)
}

struct Raw {
    _session: Session,
    _inbound: mpsc::Receiver<Stream>,
    ctrl: Stream,
}

async fn raw_client(port: u16) -> Raw {
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let conn = TlsConnector::from(tls::client_config(None).unwrap());
    let tls = conn
        .connect(ServerName::try_from("127.0.0.1").unwrap(), tcp)
        .await
        .unwrap();
    let (session, inbound, _driver) = mux::spawn(tls, false, MuxConfig::default());
    let ctrl = session.open().await.unwrap();
    Raw {
        _session: session,
        _inbound: inbound,
        ctrl,
    }
}

fn login(version: u32) -> ClientMsg {
    let ts = unix_now();
    ClientMsg::Login(Login {
        version,
        user: String::new(),
        client_id: "raw".into(),
        timestamp: ts,
        auth: auth_key("", ts),
        metas: Default::default(),
        enroll: None,
    })
}

async fn expect_login_ok(r: &mut Raw) {
    write_frame(&mut r.ctrl, &login(PROTOCOL_VERSION))
        .await
        .unwrap();
    match timeout(T, read_frame::<_, ServerMsg>(&mut r.ctrl))
        .await
        .unwrap()
        .unwrap()
    {
        ServerMsg::LoginResp { ok: true, .. } => {}
        other => panic!("{other:?}"),
    }
}

async fn assert_closed(r: &mut Raw) {
    let n = timeout(T, async {
        let mut b = [0u8; 1];
        r.ctrl.read(&mut b).await.unwrap_or(0)
    })
    .await
    .unwrap();
    assert_eq!(n, 0, "edge should close the control stream");
}

struct RedirectPolicy(EdgeRedirect);

impl Policy for RedirectPolicy {
    fn login<'a>(&'a self, _login: &'a Login) -> BoxFut<'a, Result<Admission, String>> {
        let target = self.0.clone();
        Box::pin(async move { Ok(Admission::Redirect(target)) })
    }
}

#[tokio::test]
async fn login_redirect_reconnects_with_the_assigned_certificate_pin() {
    let target = Edge::bind(
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
    let target_port = target.local_addr().unwrap().port();
    let expected = EdgeRedirect {
        address: "127.0.0.1".into(),
        port: target_port,
        tls_fingerprint: target.certificate_fingerprint().into(),
    };
    let target_stop = CancellationToken::new();
    tokio::spawn(target.serve(target_stop.clone()));

    let source = Edge::bind_with_policy(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            ..Default::default()
        },
        None,
        std::sync::Arc::new(RedirectPolicy(expected.clone())),
        Logger::silent(),
    )
    .await
    .unwrap();
    let source_port = source.local_addr().unwrap().port();
    let source_stop = CancellationToken::new();
    tokio::spawn(source.serve(source_stop.clone()));

    let (tx, mut rx) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    let run = tokio::spawn(client::run(
        ClientConfig {
            server_addr: "127.0.0.1".into(),
            server_port: source_port,
            heartbeat_interval: Duration::from_millis(100),
            heartbeat_timeout: Duration::from_secs(2),
            ..Default::default()
        },
        Logger::silent(),
        Some(tx),
        cancel.clone(),
    ));

    let mut redirected = false;
    let mut connected = false;
    while !redirected || !connected {
        match timeout(T, rx.recv()).await.unwrap().unwrap() {
            Event::Redirected(target) => {
                assert_eq!(target, expected);
                redirected = true;
            }
            Event::Connected => connected = true,
            _ => {}
        }
    }
    cancel.cancel();
    source_stop.cancel();
    let _ = timeout(T, run).await;

    let invalid_redirect = EdgeRedirect {
        address: "127.0.0.1".into(),
        port: target_port,
        tls_fingerprint: "not-a-fingerprint".into(),
    };
    let invalid_source = Edge::bind_with_policy(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            ..Default::default()
        },
        None,
        std::sync::Arc::new(RedirectPolicy(invalid_redirect.clone())),
        Logger::silent(),
    )
    .await
    .unwrap();
    let invalid_source_port = invalid_source.local_addr().unwrap().port();
    let invalid_source_stop = CancellationToken::new();
    tokio::spawn(invalid_source.serve(invalid_source_stop.clone()));
    let (tx, mut rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let run = tokio::spawn(client::run(
        ClientConfig {
            server_addr: "127.0.0.1".into(),
            server_port: invalid_source_port,
            ..Default::default()
        },
        Logger::silent(),
        Some(tx),
        cancel.clone(),
    ));
    assert_eq!(
        timeout(T, rx.recv()).await.unwrap().unwrap(),
        Event::Redirected(invalid_redirect)
    );
    let error = timeout(T, run).await.unwrap().unwrap().unwrap_err();
    assert!(error.to_string().contains("TLS pin"));
    cancel.cancel();
    invalid_source_stop.cancel();
    target_stop.cancel();
}

#[tokio::test]
async fn edge_rejects_wrong_protocol_version() {
    let (port, stop) = start_edge(EdgeConfig::default()).await;
    let mut r = raw_client(port).await;
    write_frame(&mut r.ctrl, &login(PROTOCOL_VERSION + 1))
        .await
        .unwrap();
    match timeout(T, read_frame::<_, ServerMsg>(&mut r.ctrl))
        .await
        .unwrap()
        .unwrap()
    {
        ServerMsg::LoginResp {
            ok: false, error, ..
        } => assert!(error.contains("unsupported protocol version")),
        other => panic!("{other:?}"),
    }
    stop.cancel();
}

#[tokio::test]
async fn edge_refuses_tcp_mux_without_listener_or_with_groups_and_health_checks() {
    let (port, stop) = start_edge(EdgeConfig::default()).await;
    let mut raw = raw_client(port).await;
    expect_login_ok(&mut raw).await;
    write_frame(
        &mut raw.ctrl,
        &ClientMsg::NewProxy(Box::new(ProxySpec {
            name: "mux-disabled".into(),
            kind: "tcpmux".into(),
            multiplexer: "httpconnect".into(),
            ..Default::default()
        })),
    )
    .await
    .unwrap();
    let response = timeout(T, read_frame::<_, ServerMsg>(&mut raw.ctrl))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(response, ServerMsg::NewProxyResp { error, .. } if error.contains("tcpmux requires"))
    );
    drop(raw);
    stop.cancel();

    let (port, stop) = start_edge(EdgeConfig {
        tcpmux_http_connect_port: Some(0),
        ..Default::default()
    })
    .await;
    let mut raw = raw_client(port).await;
    expect_login_ok(&mut raw).await;
    for (name, lb) in [
        (
            "mux-group",
            LbOpts {
                group: "unsupported".into(),
                ..Default::default()
            },
        ),
        (
            "mux-health",
            LbOpts {
                health: Some(HealthSpec {
                    kind: "tcp".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
    ] {
        write_frame(
            &mut raw.ctrl,
            &ClientMsg::NewProxy(Box::new(ProxySpec {
                name: name.into(),
                kind: "tcpmux".into(),
                multiplexer: "httpconnect".into(),
                lb,
                ..Default::default()
            })),
        )
        .await
        .unwrap();
        let response = timeout(T, read_frame::<_, ServerMsg>(&mut raw.ctrl))
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(response, ServerMsg::NewProxyResp { error, .. } if error.contains("tcpmux does not support"))
        );
    }
    stop.cancel();
}

#[tokio::test]
async fn edge_requires_login_first_and_rejects_duplicates() {
    let (port, stop) = start_edge(EdgeConfig::default()).await;
    let mut r = raw_client(port).await;
    write_frame(&mut r.ctrl, &ClientMsg::Ping).await.unwrap();
    assert_closed(&mut r).await;

    let mut r = raw_client(port).await;
    expect_login_ok(&mut r).await;
    write_frame(&mut r.ctrl, &login(PROTOCOL_VERSION))
        .await
        .unwrap();
    assert_closed(&mut r).await;
    stop.cancel();
}

#[tokio::test]
async fn visitor_streams_with_bad_hellos_are_refused_without_hurting_the_session() {
    let (port, stop) = start_edge(EdgeConfig::default()).await;
    let mut r = raw_client(port).await;
    expect_login_ok(&mut r).await;
    // 未知代理：得到明确的拒绝原因。
    let mut v = r._session.open().await.unwrap();
    write_frame(
        &mut v,
        &rustfrp_core::wire::VisitorHello {
            server_name: "nobody".into(),
            timestamp: unix_now(),
            sign: "x".into(),
            use_encryption: false,
            use_compression: false,
        },
    )
    .await
    .unwrap();
    let resp: rustfrp_core::wire::VisitorResp =
        timeout(T, read_frame(&mut v)).await.unwrap().unwrap();
    assert!(!resp.ok && resp.error.contains("not found"), "{resp:?}");
    // 乱码首帧：这条流被丢弃；控制流照常工作。
    let mut junk = r._session.open().await.unwrap();
    junk.write_all(&3u32.to_be_bytes()).await.unwrap();
    junk.write_all(b"???").await.unwrap();
    junk.flush().await.unwrap();
    let mut buf = [0u8; 1];
    let n = timeout(T, junk.read(&mut buf)).await.unwrap().unwrap_or(0);
    assert_eq!(n, 0);
    write_frame(&mut r.ctrl, &ClientMsg::Ping).await.unwrap();
    assert_eq!(
        timeout(T, read_frame::<_, ServerMsg>(&mut r.ctrl))
            .await
            .unwrap()
            .unwrap(),
        ServerMsg::Pong
    );
    stop.cancel();
}

#[tokio::test]
async fn edge_answers_pings_and_survives_garbage() {
    let (port, stop) = start_edge(EdgeConfig::default()).await;
    let mut r = raw_client(port).await;
    expect_login_ok(&mut r).await;
    write_frame(&mut r.ctrl, &ClientMsg::Ping).await.unwrap();
    assert_eq!(
        timeout(T, read_frame::<_, ServerMsg>(&mut r.ctrl))
            .await
            .unwrap()
            .unwrap(),
        ServerMsg::Pong
    );
    // 垃圾帧：连接被关闭，边缘节点本身不受影响。
    r.ctrl.write_all(&3u32.to_be_bytes()).await.unwrap();
    r.ctrl.write_all(b"???").await.unwrap();
    r.ctrl.flush().await.unwrap();
    assert_closed(&mut r).await;
    let mut again = raw_client(port).await;
    expect_login_ok(&mut again).await;
    stop.cancel();
}

#[tokio::test]
async fn edge_survives_a_client_that_vanishes_with_visitors_waiting() {
    let (port, stop) = start_edge(EdgeConfig {
        user_conn_timeout: Duration::from_millis(300),
        ..Default::default()
    })
    .await;
    let mut r = raw_client(port).await;
    expect_login_ok(&mut r).await;
    write_frame(
        &mut r.ctrl,
        &ClientMsg::NewProxy(Box::new(ProxySpec {
            name: "gone".into(),
            kind: "tcp".into(),
            remote_port: 0,
            ..Default::default()
        })),
    )
    .await
    .unwrap();
    let remote = match timeout(T, read_frame::<_, ServerMsg>(&mut r.ctrl))
        .await
        .unwrap()
        .unwrap()
    {
        ServerMsg::NewProxyResp {
            remote_port, error, ..
        } if error.is_empty() => remote_port,
        other => panic!("{other:?}"),
    };
    // 客户端会话被「拔线」，但访问者已经连进来：连接应被干净地关闭。
    drop(r);
    tokio::time::sleep(Duration::from_millis(100)).await;
    if let Ok(mut v) = TcpStream::connect(("127.0.0.1", remote)).await {
        let mut b = [0u8; 1];
        let n = timeout(T, v.read(&mut b)).await.unwrap().unwrap_or(0);
        assert_eq!(n, 0);
    }
    stop.cancel();
}

#[tokio::test]
async fn proxy_names_are_released_when_a_client_disconnects() {
    let (port, stop) = start_edge(EdgeConfig::default()).await;
    for _ in 0..2 {
        let mut r = raw_client(port).await;
        expect_login_ok(&mut r).await;
        write_frame(
            &mut r.ctrl,
            &ClientMsg::NewProxy(Box::new(ProxySpec {
                name: "same".into(),
                kind: "tcp".into(),
                remote_port: 0,
                ..Default::default()
            })),
        )
        .await
        .unwrap();
        match timeout(T, read_frame::<_, ServerMsg>(&mut r.ctrl))
            .await
            .unwrap()
            .unwrap()
        {
            ServerMsg::NewProxyResp { error, .. } => assert!(error.is_empty(), "{error}"),
            other => panic!("{other:?}"),
        }
        drop(r);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    stop.cancel();
}

#[tokio::test]
async fn heartbeats_keep_a_quiet_session_alive() {
    let (port, stop) = start_edge(EdgeConfig {
        heartbeat_timeout: Duration::from_millis(400),
        ..Default::default()
    })
    .await;
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: port,
        client_id: "hb".into(),
        heartbeat_interval: Duration::from_millis(60),
        ..Default::default()
    };
    let (tx, mut rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
    assert_eq!(
        timeout(T, rx.recv()).await.unwrap().unwrap(),
        Event::Connected
    );
    // 1.2 秒内（3 倍于边缘的失联超时）不应出现断线事件。
    assert!(timeout(Duration::from_millis(1200), rx.recv())
        .await
        .is_err());
    cancel.cancel();
    stop.cancel();
}

// ---------- 扮演边缘节点，连真实的 Client ----------

async fn fake_edge<F, Fut>(script: F) -> u16
where
    F: FnOnce(Session, mpsc::Receiver<Stream>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (tcp, _) = l.accept().await.unwrap();
        let acceptor = TlsAcceptor::from(
            tls::server_config(tls::self_signed(vec!["x".into()]).unwrap()).unwrap(),
        );
        let tls = acceptor.accept(tcp).await.unwrap();
        let (session, inbound, _driver) = mux::spawn(tls, true, MuxConfig::default());
        script(session, inbound).await;
    });
    port
}

fn quick_client(port: u16, proxies: Vec<ProxyCfg>) -> ClientConfig {
    ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: port,
        proxies,
        client_id: "fixed-id".into(),
        token: "tok".into(),
        login_fail_exit: false,
        backoff: (Duration::from_secs(30), Duration::from_secs(30)),
        handshake_timeout: Duration::from_millis(500),
        ..Default::default()
    }
}

async fn first_end(cfg: ClientConfig) -> String {
    let (tx, mut rx) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
    let reason = loop {
        if let Event::Disconnected(r) = timeout(T, rx.recv()).await.unwrap().unwrap() {
            break r;
        }
    };
    cancel.cancel();
    reason
}

fn p(name: &str, local_port: u16) -> ProxyCfg {
    ProxyCfg {
        name: name.into(),
        kind: "tcp".into(),
        local_ip: "127.0.0.1".into(),
        local_port,
        remote_port: 0,
        ..Default::default()
    }
}

#[tokio::test]
async fn client_handles_an_unexpected_reply_to_login() {
    let port = fake_edge(|s, mut inbound| async move {
        let _hold = s;
        let mut ctrl = inbound.recv().await.unwrap();
        let _: ClientMsg = read_frame(&mut ctrl).await.unwrap();
        write_frame(&mut ctrl, &ServerMsg::Pong).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    })
    .await;
    assert!(first_end(quick_client(port, vec![]))
        .await
        .contains("unexpected message in login"));
}

#[tokio::test]
async fn client_handles_an_unexpected_reply_while_registering() {
    let port = fake_edge(|s, mut inbound| async move {
        let _hold = s;
        let mut ctrl = inbound.recv().await.unwrap();
        let _: ClientMsg = read_frame(&mut ctrl).await.unwrap();
        write_frame(
            &mut ctrl,
            &ServerMsg::LoginResp {
                ok: true,
                error: String::new(),
                run_id: "r".into(),
                client_id: String::new(),
                managed: false,
                redirect: None,
            },
        )
        .await
        .unwrap();
        let _: ClientMsg = read_frame(&mut ctrl).await.unwrap();
        write_frame(&mut ctrl, &ServerMsg::Pong).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    })
    .await;
    assert!(first_end(quick_client(port, vec![p("a", 1)]))
        .await
        .contains("unexpected message while registering"));
}

#[tokio::test]
async fn client_notices_when_the_control_stream_ends() {
    let port = fake_edge(|s, mut inbound| async move {
        let _hold = s;
        let mut ctrl = inbound.recv().await.unwrap();
        let _: ClientMsg = read_frame(&mut ctrl).await.unwrap();
        write_frame(
            &mut ctrl,
            &ServerMsg::LoginResp {
                ok: true,
                error: String::new(),
                run_id: "r".into(),
                client_id: String::new(),
                managed: false,
                redirect: None,
            },
        )
        .await
        .unwrap();
        drop(ctrl);
        tokio::time::sleep(Duration::from_secs(2)).await;
    })
    .await;
    let reason = first_end(quick_client(port, vec![])).await;
    assert!(
        reason.contains("io:") || reason.contains("control stream closed"),
        "{reason}"
    );
}

#[tokio::test]
async fn client_notices_when_the_server_closes_the_connection() {
    let port = fake_edge(|session, mut inbound| async move {
        let mut ctrl = inbound.recv().await.unwrap();
        let _: ClientMsg = read_frame(&mut ctrl).await.unwrap();
        write_frame(
            &mut ctrl,
            &ServerMsg::LoginResp {
                ok: true,
                error: String::new(),
                run_id: "r".into(),
                client_id: String::new(),
                managed: false,
                redirect: None,
            },
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop((session, inbound, ctrl));
    })
    .await;
    let reason = first_end(quick_client(port, vec![])).await;
    assert!(!reason.is_empty());
}

#[tokio::test]
async fn client_times_out_when_pongs_never_arrive() {
    let port = fake_edge(|s, mut inbound| async move {
        let _hold = s;
        let mut ctrl = inbound.recv().await.unwrap();
        let _: ClientMsg = read_frame(&mut ctrl).await.unwrap();
        write_frame(
            &mut ctrl,
            &ServerMsg::LoginResp {
                ok: true,
                error: String::new(),
                run_id: "r".into(),
                client_id: String::new(),
                managed: false,
                redirect: None,
            },
        )
        .await
        .unwrap();
        // 之后只读不答。
        while read_frame::<_, ClientMsg>(&mut ctrl).await.is_ok() {}
    })
    .await;
    let cfg = ClientConfig {
        heartbeat_interval: Duration::from_millis(50),
        heartbeat_timeout: Duration::from_millis(250),
        ..quick_client(port, vec![])
    };
    assert_eq!(first_end(cfg).await, "heartbeat timeout");
}

#[tokio::test]
async fn client_ignores_stray_messages_and_work_for_unknown_proxies() {
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let port = fake_edge(|session, mut inbound| async move {
        let mut ctrl = inbound.recv().await.unwrap();
        let _: ClientMsg = read_frame(&mut ctrl).await.unwrap();
        write_frame(
            &mut ctrl,
            &ServerMsg::LoginResp {
                ok: true,
                error: String::new(),
                run_id: "r".into(),
                client_id: String::new(),
                managed: false,
                redirect: None,
            },
        )
        .await
        .unwrap();
        // 多余的 LoginResp 被忽略。
        write_frame(
            &mut ctrl,
            &ServerMsg::LoginResp {
                ok: true,
                error: String::new(),
                run_id: "x".into(),
                client_id: String::new(),
                managed: false,
                redirect: None,
            },
        )
        .await
        .unwrap();
        // 针对未知代理的工作流：客户端应直接丢弃，连接保持可用。
        let mut work = session.open().await.unwrap();
        write_frame(
            &mut work,
            &StartWork {
                proxy: "nope".into(),
                src: "1.1.1.1:1".into(),
                dst: String::new(),
                use_encryption: false,
                use_compression: false,
                encryption_key: String::new(),
            },
        )
        .await
        .unwrap();
        let mut b = [0u8; 1];
        let n = work.read(&mut b).await.unwrap_or(0);
        let _ = done_tx.send(n);
        tokio::time::sleep(Duration::from_millis(300)).await;
    })
    .await;
    let (tx, mut rx) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    tokio::spawn(client::run(
        quick_client(port, vec![]),
        Logger::silent(),
        Some(tx),
        cancel.clone(),
    ));
    assert_eq!(
        timeout(T, rx.recv()).await.unwrap().unwrap(),
        Event::Connected
    );
    assert_eq!(timeout(T, done_rx).await.unwrap().unwrap(), 0);
    cancel.cancel();
}

#[tokio::test]
async fn client_rejects_an_invalid_server_name_and_a_failed_dial() {
    let cfg = ClientConfig {
        server_addr: "not a valid host!".into(),
        login_fail_exit: false,
        ..Default::default()
    };
    assert!(first_end(cfg).await.contains("invalid server address"));
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    assert!(first_end(quick_client(port, vec![])).await.contains("io:"));
}

#[tokio::test]
async fn login_rejection_after_a_successful_login_does_not_exit() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        for round in 0..2 {
            let (tcp, _) = l.accept().await.unwrap();
            let acceptor = TlsAcceptor::from(
                tls::server_config(tls::self_signed(vec!["x".into()]).unwrap()).unwrap(),
            );
            let tls = acceptor.accept(tcp).await.unwrap();
            let (session, mut inbound, _d) = mux::spawn(tls, true, MuxConfig::default());
            let mut ctrl = inbound.recv().await.unwrap();
            let _: ClientMsg = read_frame(&mut ctrl).await.unwrap();
            let resp = if round == 0 {
                ServerMsg::LoginResp {
                    ok: true,
                    error: String::new(),
                    run_id: "r".into(),
                    client_id: String::new(),
                    managed: false,
                    redirect: None,
                }
            } else {
                ServerMsg::LoginResp {
                    ok: false,
                    error: "nope".into(),
                    run_id: String::new(),
                    client_id: String::new(),
                    managed: false,
                    redirect: None,
                }
            };
            write_frame(&mut ctrl, &resp).await.unwrap();
            if round == 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            } else {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            drop((session, inbound, ctrl));
        }
    });
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: port,
        login_fail_exit: true,
        backoff: (Duration::from_millis(30), Duration::from_millis(60)),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let task = tokio::spawn(client::run(cfg, Logger::silent(), None, cancel.clone()));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !task.is_finished(),
        "the first login succeeded, so a later rejection must not terminate the client"
    );
    cancel.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn standalone_edge_tolerates_applied_acks_and_a_closed_control_stream() {
    let (port, stop) = start_edge(EdgeConfig::default()).await;
    let mut r = raw_client(port).await;
    expect_login_ok(&mut r).await;
    write_frame(&mut r.ctrl, &ClientMsg::Applied { revision: 3 })
        .await
        .unwrap();
    write_frame(&mut r.ctrl, &ClientMsg::Ping).await.unwrap();
    assert_eq!(
        timeout(T, read_frame::<_, ServerMsg>(&mut r.ctrl))
            .await
            .unwrap()
            .unwrap(),
        ServerMsg::Pong
    );
    // 客户端正常关闭控制流（而不是掉线）：边缘节点干净地结束该会话。
    r.ctrl.shutdown().await.unwrap();
    drop(r);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut again = raw_client(port).await;
    expect_login_ok(&mut again).await;
    stop.cancel();
}
