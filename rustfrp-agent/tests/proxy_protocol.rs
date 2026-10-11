//! PROXY protocol：客户端连上本地服务后先写头，内网服务据此得知访问者的真实地址。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, Event, ProxyCfg};
use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(10);

/// 把收到的全部字节记下来的本地服务（不回显：头是写给服务的，不会回流给访问者）。
async fn recording_backend() -> (u16, Arc<Mutex<Vec<u8>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<u8>>> = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let mut b = [0u8; 4096];
                while let Ok(n) = s.read(&mut b).await {
                    if n == 0 {
                        return;
                    }
                    log.lock().unwrap().extend_from_slice(&b[..n]);
                }
            });
        }
    });
    (port, seen)
}

async fn run(
    version: u8,
    kind: &str,
    domains: Vec<String>,
) -> (
    u16,
    Arc<Mutex<Vec<u8>>>,
    CancellationToken,
    CancellationToken,
    u16,
) {
    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            vhost_http_port: Some(0),
            ..Default::default()
        },
        None,
        Logger::silent(),
    )
    .await
    .unwrap();
    let edge_port = edge.local_addr().unwrap().port();
    let vhost = edge.http_addr().map_or(0, |a| a.port());
    let stop = CancellationToken::new();
    tokio::spawn(edge.serve(stop.clone()));
    let (backend, seen) = recording_backend().await;
    let (tx, mut rx) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: edge_port,
        proxies: vec![ProxyCfg {
            name: "pp".into(),
            kind: kind.into(),
            local_ip: "127.0.0.1".into(),
            local_port: backend,
            custom_domains: domains,
            proxy_protocol: version,
            ..Default::default()
        }],
        heartbeat_interval: Duration::from_millis(100),
        ..Default::default()
    };
    tokio::spawn(client::run(cfg, Logger::silent(), Some(tx), cancel.clone()));
    let public = loop {
        match timeout(T, rx.recv()).await.unwrap().unwrap() {
            Event::ProxyStarted { remote_port, .. } => break remote_port,
            Event::ProxyFailed { error, .. } => panic!("{error}"),
            _ => {}
        }
    };
    (
        if kind == "tcp" { public } else { vhost },
        seen,
        cancel,
        stop,
        edge_port,
    )
}

async fn wait_len(seen: &Arc<Mutex<Vec<u8>>>, n: usize) {
    for _ in 0..200 {
        if seen.lock().unwrap().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("backend saw only {} bytes", seen.lock().unwrap().len());
}

#[tokio::test]
async fn v1_header_precedes_the_payload_and_names_the_visitor() {
    let (public, seen, cancel, stop, _) = run(1, "tcp", vec![]).await;
    let mut s = TcpStream::connect(("127.0.0.1", public)).await.unwrap();
    let visitor_port = s.local_addr().unwrap().port();
    s.write_all(b"hello").await.unwrap();
    let expected = format!("PROXY TCP4 127.0.0.1 127.0.0.1 {visitor_port} {public}\r\nhello");
    wait_len(&seen, expected.len()).await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(String::from_utf8_lossy(&seen), expected);
    cancel.cancel();
    stop.cancel();
}

#[tokio::test]
async fn v2_header_is_binary_and_http_tunnels_get_it_too() {
    let (public, seen, cancel, stop, _) = run(2, "tcp", vec![]).await;
    let mut s = TcpStream::connect(("127.0.0.1", public)).await.unwrap();
    s.write_all(b"x").await.unwrap();
    wait_len(&seen, 29).await;
    {
        let bytes = seen.lock().unwrap().clone();
        assert_eq!(&bytes[..12], b"\r\n\r\n\0\r\nQUIT\n");
        assert_eq!(&bytes[12..16], &[0x21, 0x11, 0, 12]);
        assert_eq!(&bytes[16..20], &[127, 0, 0, 1]);
        assert_eq!(
            &bytes[24..26],
            &s.local_addr().unwrap().port().to_be_bytes()
        );
        assert_eq!(&bytes[26..28], &public.to_be_bytes());
        assert_eq!(bytes[28..], *b"x");
    }
    cancel.cancel();
    stop.cancel();

    // http 隧道：头在请求之前，请求本身（含 Host）原样跟在后面。
    let (vhost, seen, cancel, stop, _) = run(1, "http", vec!["a.example.com".into()]).await;
    let mut s = TcpStream::connect(("127.0.0.1", vhost)).await.unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
        .await
        .unwrap();
    wait_len(&seen, 80).await;
    let text = String::from_utf8_lossy(&seen.lock().unwrap().clone()).to_string();
    assert!(
        text.starts_with("PROXY TCP4 127.0.0.1 127.0.0.1 "),
        "{text}"
    );
    assert!(
        text.ends_with("\r\nGET / HTTP/1.1\r\nHost: a.example.com\r\n\r\n"),
        "{text}"
    );
    cancel.cancel();
    stop.cancel();
}

#[tokio::test]
async fn without_the_option_nothing_is_prepended() {
    let (public, seen, cancel, stop, _) = run(0, "tcp", vec![]).await;
    let mut s = TcpStream::connect(("127.0.0.1", public)).await.unwrap();
    s.write_all(b"plain").await.unwrap();
    wait_len(&seen, 5).await;
    assert_eq!(seen.lock().unwrap().as_slice(), b"plain");
    cancel.cancel();
    stop.cancel();
}
