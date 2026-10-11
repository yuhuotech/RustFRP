//! 带宽限制：客户端侧 / 服务端侧限速都应让大块下行数据明显变慢，且不丢字节。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use rustfrp_agent::client::{self, ClientConfig, Event, ProxyCfg};
use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const T: Duration = Duration::from_secs(20);
fn client_hello(sni: &str) -> Vec<u8> {
    let n = sni.as_bytes();
    let mut ext = vec![0, 0];
    let list = 3 + n.len();
    ext.extend(((list + 2) as u16).to_be_bytes());
    ext.extend((list as u16).to_be_bytes());
    ext.push(0);
    ext.extend((n.len() as u16).to_be_bytes());
    ext.extend(n);
    let mut hs = vec![3, 3];
    hs.extend([1u8; 32]);
    hs.push(0);
    hs.extend([0, 2, 0x13, 1, 1, 0]);
    hs.extend((ext.len() as u16).to_be_bytes());
    hs.extend(ext);
    let mut body = vec![1, 0, (hs.len() >> 8) as u8, hs.len() as u8];
    body.extend(hs);
    let mut rec = vec![0x16, 3, 1];
    rec.extend((body.len() as u16).to_be_bytes());
    rec.extend(body);
    rec
}

const RATE: u64 = 64 * 1024;
const TOTAL: usize = 192 * 1024;

/// 连上（tcp）或收到第一段数据后（http / https）吐 `TOTAL` 字节的本地服务；
/// http 的字节带一个合法的响应头。
async fn firehose(kind: &'static str) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                if kind != "tcp" {
                    let mut b = [0u8; 4096];
                    let _ = s.read(&mut b).await;
                }
                if kind == "http" {
                    let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {TOTAL}\r\n\r\n");
                    let _ = s.write_all(head.as_bytes()).await;
                }
                let _ = s.write_all(&vec![7u8; TOTAL]).await;
                let _ = s.shutdown().await;
            });
        }
    });
    port
}

async fn download(kind: &'static str, bps: u64, server_side: bool) -> Duration {
    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            vhost_http_port: Some(0),
            vhost_https_port: Some(0),
            ..Default::default()
        },
        None,
        Logger::silent(),
    )
    .await
    .unwrap();
    let edge_port = edge.local_addr().unwrap().port();
    let http = edge.http_addr().map_or(0, |a| a.port());
    let https = edge.https_addr().map_or(0, |a| a.port());
    let stop = CancellationToken::new();
    tokio::spawn(edge.serve(stop.clone()));
    let (tx, mut rx) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    let cfg = ClientConfig {
        server_addr: "127.0.0.1".into(),
        server_port: edge_port,
        proxies: vec![ProxyCfg {
            name: "bw".into(),
            kind: kind.into(),
            custom_domains: if kind == "tcp" {
                vec![]
            } else {
                vec!["bw.example.com".into()]
            },
            local_ip: "127.0.0.1".into(),
            local_port: firehose(kind).await,
            bandwidth_bps: bps,
            bandwidth_server: server_side,
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
    let start = Instant::now();
    let port = match kind {
        "http" => http,
        "https" => https,
        _ => public,
    };
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut got = 0usize;
    match kind {
        "http" => {
            s.write_all(b"GET / HTTP/1.1\r\nHost: bw.example.com\r\n\r\n")
                .await
                .unwrap();
        }
        "https" => s.write_all(&client_hello("bw.example.com")).await.unwrap(),
        _ => {}
    }
    let mut buf = vec![0u8; 16 * 1024];
    while got < TOTAL {
        let n = timeout(T, s.read(&mut buf)).await.unwrap().unwrap();
        assert!(n > 0, "closed after {got} bytes");
        got += n;
    }
    let took = start.elapsed();
    cancel.cancel();
    stop.cancel();
    took
}

#[tokio::test]
async fn client_side_limit_slows_the_download() {
    let slow = download("tcp", RATE, false).await;
    assert!(slow >= Duration::from_millis(1500), "{slow:?}");
}

#[tokio::test]
async fn server_side_limit_slows_the_download() {
    let slow = download("tcp", RATE, true).await;
    assert!(slow >= Duration::from_millis(1500), "{slow:?}");
}

#[tokio::test]
async fn no_limit_is_fast() {
    let fast = download("tcp", 0, false).await;
    assert!(fast < Duration::from_millis(1500), "{fast:?}");
}

#[tokio::test]
async fn http_and_https_tunnels_are_limited_too() {
    for kind in ["http", "https"] {
        for server_side in [false, true] {
            let slow = download(kind, RATE, server_side).await;
            assert!(
                slow >= Duration::from_millis(1500),
                "{kind} server={server_side}: {slow:?}"
            );
        }
    }
    assert!(download("http", 0, false).await < Duration::from_millis(1500));
}
