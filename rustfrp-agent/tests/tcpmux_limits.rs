#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use rustfrp_agent::client::{self, ClientConfig, Event, ProxyCfg};
use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

async fn echo_listener() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                while let Ok(count) = socket.read(&mut buffer).await {
                    if count == 0
                        || socket
                            .write_all(buffer.get(..count).unwrap_or(&[]))
                            .await
                            .is_err()
                    {
                        break;
                    }
                }
            });
        }
    });
    (port, task)
}

async fn udp_echo_listener() -> (u16, tokio::task::JoinHandle<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let mut buffer = [0u8; 2048];
        while let Ok((count, peer)) = socket.recv_from(&mut buffer).await {
            let _ = socket.send_to(&buffer[..count], peer).await;
        }
    });
    (port, task)
}

async fn connect_tunnel(port: u16, payload: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let request = "CONNECT mux.example.test:443 HTTP/1.1\r\nHost: mux.example.test\r\n\r\n";
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(payload).await.unwrap();
    let response = b"HTTP/1.1 200 Connection Established\r\n\r\n";
    let mut got = vec![0; response.len()];
    timeout(Duration::from_secs(3), stream.read_exact(&mut got))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, response);
    let mut echoed = Vec::with_capacity(payload.len());
    let mut chunk = [0u8; 4096];
    while echoed.len() < payload.len() {
        let count = timeout(Duration::from_secs(3), stream.read(&mut chunk))
            .await
            .unwrap()
            .unwrap();
        if count == 0 {
            break;
        }
        echoed.extend_from_slice(chunk.get(..count).unwrap_or(&[]));
    }
    assert_eq!(echoed, payload);
    stream
}

#[tokio::test]
async fn tcpmux_connect_routes_compressed_encrypted_work_and_enforces_limit() {
    let (local_port, echo) = echo_listener().await;
    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            tcpmux_http_connect_port: Some(0),
            ..Default::default()
        },
        None,
        Logger::silent(),
    )
    .await
    .unwrap();
    let control_port = edge.local_addr().unwrap().port();
    let mux_port = edge.tcpmux_addr().unwrap().port();
    let edge_stop = CancellationToken::new();
    tokio::spawn(edge.serve(edge_stop.clone()));

    let client_stop = CancellationToken::new();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(32);
    let run = tokio::spawn(client::run(
        ClientConfig {
            server_addr: "127.0.0.1".into(),
            server_port: control_port,
            heartbeat_interval: Duration::from_millis(100),
            heartbeat_timeout: Duration::from_secs(2),
            proxies: vec![ProxyCfg {
                name: "mux".into(),
                kind: "tcpmux".into(),
                multiplexer: "httpconnect".into(),
                max_connections: 1,
                use_encryption: true,
                use_compression: true,
                local_ip: "127.0.0.1".into(),
                local_port,
                custom_domains: vec!["mux.example.test".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        Logger::silent(),
        Some(events_tx),
        client_stop.clone(),
    ));
    timeout(Duration::from_secs(3), async {
        loop {
            if matches!(events_rx.recv().await, Some(Event::ProxyStarted { name, .. }) if name == "mux") {
                break;
            }
        }
    })
    .await
    .unwrap();

    let payload = vec![b'x'; 12 * 1024];
    let first = connect_tunnel(mux_port, &payload).await;
    let mut second = TcpStream::connect(("127.0.0.1", mux_port)).await.unwrap();
    second
        .write_all(b"CONNECT mux.example.test:443 HTTP/1.1\r\nHost: mux.example.test\r\n\r\n")
        .await
        .unwrap();
    let mut rejected = [0u8; 128];
    let count = timeout(Duration::from_secs(3), second.read(&mut rejected))
        .await
        .unwrap()
        .unwrap();
    assert!(
        String::from_utf8_lossy(rejected.get(..count).unwrap_or(&[])).starts_with("HTTP/1.1 503")
    );

    drop(first);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let third = connect_tunnel(mux_port, b"again").await;
    drop(third);
    client_stop.cancel();
    edge_stop.cancel();
    let _ = timeout(Duration::from_secs(2), run).await;
    echo.abort();
}

#[tokio::test]
async fn client_connection_limit_is_shared_between_tunnels() {
    let (local_port, echo) = echo_listener().await;
    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            max_connections_per_client: 1,
            ..Default::default()
        },
        None,
        Logger::silent(),
    )
    .await
    .unwrap();
    let port = edge.local_addr().unwrap().port();
    let edge_stop = CancellationToken::new();
    tokio::spawn(edge.serve(edge_stop.clone()));
    let client_stop = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let run = tokio::spawn(client::run(
        ClientConfig {
            server_addr: "127.0.0.1".into(),
            server_port: port,
            heartbeat_interval: Duration::from_millis(100),
            heartbeat_timeout: Duration::from_secs(2),
            proxies: vec![
                ProxyCfg {
                    name: "a".into(),
                    kind: "tcp".into(),
                    local_ip: "127.0.0.1".into(),
                    local_port,
                    ..Default::default()
                },
                ProxyCfg {
                    name: "b".into(),
                    kind: "tcp".into(),
                    local_ip: "127.0.0.1".into(),
                    local_port,
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
        Logger::silent(),
        Some(tx),
        client_stop.clone(),
    ));
    let remote = tokio::time::timeout(Duration::from_secs(3), async {
        let mut ports = std::collections::HashMap::new();
        while ports.len() < 2 {
            if let Some(Event::ProxyStarted { name, remote_port }) = rx.recv().await {
                ports.insert(name, remote_port);
            }
        }
        ports
    })
    .await
    .unwrap();
    let first_port = *remote.get("a").unwrap();
    let second_port = *remote.get("b").unwrap();
    let _first = TcpStream::connect(("127.0.0.1", first_port)).await.unwrap();
    let mut second = TcpStream::connect(("127.0.0.1", second_port))
        .await
        .unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(
        timeout(Duration::from_secs(3), second.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    drop(_first);
    client_stop.cancel();
    edge_stop.cancel();
    let _ = timeout(Duration::from_secs(2), run).await;
    echo.abort();
}

#[tokio::test]
async fn tcpmux_rejects_incomplete_bad_and_unknown_connect_requests() {
    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            tcpmux_http_connect_port: Some(0),
            ..Default::default()
        },
        None,
        Logger::silent(),
    )
    .await
    .unwrap();
    let mux_port = edge.tcpmux_addr().unwrap().port();
    let stop = CancellationToken::new();
    tokio::spawn(edge.serve(stop.clone()));

    async fn response(port: u16, request: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(request).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut bytes = Vec::new();
        timeout(Duration::from_secs(3), async {
            let mut chunk = [0u8; 1024];
            loop {
                match stream.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(count) => bytes.extend_from_slice(&chunk[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
                    Err(error) => panic!("read CONNECT response: {error}"),
                }
            }
        })
        .await
        .unwrap();
        bytes
    }

    let bad = response(mux_port, b"GET / HTTP/1.1\r\nHost: mux.test\r\n\r\n").await;
    assert!(String::from_utf8_lossy(&bad).starts_with("HTTP/1.1 400"));

    let empty = response(mux_port, b"").await;
    assert!(String::from_utf8_lossy(&empty).starts_with("HTTP/1.1 400"));

    let missing = response(
        mux_port,
        b"CONNECT missing.example.test:443 HTTP/1.1\r\nHost: missing.example.test\r\n\r\n",
    )
    .await;
    assert!(String::from_utf8_lossy(&missing).starts_with("HTTP/1.1 404"));

    let mut oversized = b"CONNECT mux.example.test:443 HTTP/1.1\r\nX-Large: ".to_vec();
    oversized.extend(std::iter::repeat_n(b'a', 17 * 1024));
    let too_large = response(mux_port, &oversized).await;
    assert!(String::from_utf8_lossy(&too_large).starts_with("HTTP/1.1 400"));

    stop.cancel();
}

#[tokio::test]
async fn tcpmux_passthrough_forwards_the_complete_connect_request_to_the_client() {
    let (local_port, echo) = echo_listener().await;
    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            tcpmux_http_connect_port: Some(0),
            ..Default::default()
        },
        None,
        Logger::silent(),
    )
    .await
    .unwrap()
    .with_tcpmux_passthrough(true);
    let control_port = edge.local_addr().unwrap().port();
    let mux_port = edge.tcpmux_addr().unwrap().port();
    let edge_stop = CancellationToken::new();
    tokio::spawn(edge.serve(edge_stop.clone()));

    let client_stop = CancellationToken::new();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(16);
    let client_run = tokio::spawn(client::run(
        ClientConfig {
            server_addr: "127.0.0.1".into(),
            server_port: control_port,
            heartbeat_interval: Duration::from_millis(100),
            heartbeat_timeout: Duration::from_secs(2),
            proxies: vec![ProxyCfg {
                name: "passthrough".into(),
                kind: "tcpmux".into(),
                multiplexer: "httpconnect".into(),
                local_ip: "127.0.0.1".into(),
                local_port,
                custom_domains: vec!["mux.example.test".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        Logger::silent(),
        Some(events_tx),
        client_stop.clone(),
    ));
    timeout(Duration::from_secs(3), async {
        loop {
            if matches!(events_rx.recv().await, Some(Event::ProxyStarted { name, .. }) if name == "passthrough") {
                break;
            }
        }
    })
    .await
    .unwrap();

    let request =
        b"CONNECT mux.example.test:443 HTTP/1.1\r\nHost: mux.example.test\r\nX-Trace: keep\r\n\r\n";
    let payload = b"opaque payload";
    let mut expected = request.to_vec();
    expected.extend_from_slice(payload);
    let mut stream = TcpStream::connect(("127.0.0.1", mux_port)).await.unwrap();
    stream.write_all(&expected).await.unwrap();
    let mut echoed = vec![0; expected.len()];
    timeout(Duration::from_secs(3), stream.read_exact(&mut echoed))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(echoed, expected);

    client_stop.cancel();
    edge_stop.cancel();
    let _ = timeout(Duration::from_secs(2), client_run).await;
    echo.abort();
}

#[tokio::test]
async fn udp_peer_sessions_share_the_client_connection_limit_and_release_it_on_idle() {
    let (local_port, echo) = udp_echo_listener().await;
    let edge = Edge::bind(
        EdgeConfig {
            bind_addr: "127.0.0.1".into(),
            bind_port: 0,
            proxy_bind_addr: "127.0.0.1".into(),
            max_connections_per_client: 1,
            udp_idle: Duration::from_millis(250),
            ..Default::default()
        },
        None,
        Logger::silent(),
    )
    .await
    .unwrap();
    let control_port = edge.local_addr().unwrap().port();
    let edge_stop = CancellationToken::new();
    tokio::spawn(edge.serve(edge_stop.clone()));
    let client_stop = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let run = tokio::spawn(client::run(
        ClientConfig {
            server_addr: "127.0.0.1".into(),
            server_port: control_port,
            udp_idle: Duration::from_millis(250),
            heartbeat_interval: Duration::from_millis(100),
            heartbeat_timeout: Duration::from_secs(2),
            proxies: vec![
                ProxyCfg {
                    name: "udp-a".into(),
                    kind: "udp".into(),
                    local_ip: "127.0.0.1".into(),
                    local_port,
                    ..Default::default()
                },
                ProxyCfg {
                    name: "udp-b".into(),
                    kind: "udp".into(),
                    local_ip: "127.0.0.1".into(),
                    local_port,
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
        Logger::silent(),
        Some(tx),
        client_stop.clone(),
    ));
    let ports = timeout(Duration::from_secs(3), async {
        let mut found = std::collections::HashMap::new();
        while found.len() < 2 {
            if let Some(Event::ProxyStarted { name, remote_port }) = rx.recv().await {
                found.insert(name, remote_port);
            }
        }
        found
    })
    .await
    .unwrap();

    let first = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    first
        .send_to(b"first", ("127.0.0.1", ports["udp-a"]))
        .await
        .unwrap();
    let mut response = [0u8; 32];
    let (count, _) = timeout(Duration::from_secs(3), first.recv_from(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&response[..count], b"first");

    let second = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    second
        .send_to(b"blocked", ("127.0.0.1", ports["udp-b"]))
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_millis(100), second.recv_from(&mut response))
            .await
            .is_err()
    );

    tokio::time::sleep(Duration::from_millis(400)).await;
    second
        .send_to(b"released", ("127.0.0.1", ports["udp-b"]))
        .await
        .unwrap();
    let (count, _) = timeout(Duration::from_secs(3), second.recv_from(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&response[..count], b"released");

    client_stop.cancel();
    edge_stop.cancel();
    let _ = timeout(Duration::from_secs(2), run).await;
    echo.abort();
}
