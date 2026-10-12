//! frps 独立模式的只读 Web 仪表盘与 JSON 接口。

use std::path::{Component, Path};
use std::sync::Arc;
use std::time::Duration;

use rustfrp_core::http1::ct_eq;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::log::Logger;
use crate::metrics::{Metrics, Sample};

const MAX_HEAD: usize = 16 * 1024;
const MAX_ASSET: u64 = 16 * 1024 * 1024;
const UNAUTHORIZED: &str = "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"frps dashboard\"\r\nContent-Length: 12\r\nConnection: close\r\nContent-Type: text/plain\r\n\r\nUnauthorized";
const NOT_FOUND: &str = "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\nContent-Type: text/plain\r\n\r\nnot found";
const EMBEDDED: &str = "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>RustFRP Edge Dashboard</title><style>body{font:14px system-ui;margin:32px;color:#172033}h1{font-size:22px}pre{white-space:pre-wrap;background:#f5f7fa;padding:16px;border-radius:8px}</style><h1>RustFRP Edge Dashboard</h1><p>服务端状态与代理流量</p><pre id=\"data\">加载中…</pre><script>Promise.all([fetch('/api/serverinfo'),fetch('/api/proxy')]).then(async r=>{document.getElementById('data').textContent=JSON.stringify({server:await r[0].json(),proxies:await r[1].json()},null,2)}).catch(e=>document.getElementById('data').textContent=String(e))</script></html>";

#[derive(Clone)]
pub struct Dashboard {
    pub addr: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub assets_dir: String,
    pub metrics: Arc<Metrics>,
}

pub async fn serve(
    listener: TcpListener,
    acceptor: Option<TlsAcceptor>,
    dashboard: Dashboard,
    log: Logger,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((tcp, peer)) = accepted else { continue };
                let (acceptor, dashboard, log) = (acceptor.clone(), dashboard.clone(), log.clone());
                tokio::spawn(async move {
                    let result = match acceptor {
                        Some(tls) => match tls.accept(tcp).await {
                            Ok(stream) => handle(stream, &dashboard).await,
                            Err(error) => Err(error.to_string()),
                        },
                        None => handle(tcp, &dashboard).await,
                    };
                    if let Err(error) = result {
                        log.debug(&format!("dashboard {peer}: {error}"));
                    }
                });
            }
        }
    }
}

async fn handle<S: tokio::io::AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    d: &Dashboard,
) -> Result<(), String> {
    let mut input = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    let request = loop {
        match rustfrp_core::http1::parse_request(&input) {
            Ok(Some(head)) => break head,
            Err(_) => return respond(&mut stream, 400, "text/plain", b"bad request", "").await,
            Ok(None) => {}
        }
        if input.len() >= MAX_HEAD {
            return respond(&mut stream, 400, "text/plain", b"bad request", "").await;
        }
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
            .await
            .map_err(|_| "request header timed out".to_string())?
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        input.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
    };
    if !d.user.is_empty() || !d.password.is_empty() {
        let ok = request
            .basic
            .as_ref()
            .is_some_and(|(u, p)| ct_eq(u, &d.user) & ct_eq(p, &d.password));
        if !ok {
            return write(&mut stream, UNAUTHORIZED.as_bytes()).await;
        }
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/serverinfo") => {
            let live = d.metrics.total().snapshot();
            let body = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "bind_port": d.port,
                "proxy_count": d.metrics.samples().iter().filter(|sample| sample.active).count(),
                "connections": live.connections,
                "traffic_in": live.bytes_in,
                "traffic_out": live.bytes_out,
            });
            respond_json(&mut stream, &body).await
        }
        ("GET", "/api/proxy") => {
            let rows = d.metrics.samples().iter().map(proxy_json).collect::<Vec<_>>();
            respond_json(&mut stream, &rows).await
        }
        ("GET", path) if path.starts_with("/api/proxy/") => {
            let mut parts = path.trim_start_matches("/api/proxy/").split('/');
            let kind = parts.next().unwrap_or_default();
            let name = parts.next();
            let rows = d.metrics.samples();
            let found = rows.iter().filter(|sample| sample.kind == kind && name.is_none_or(|n| sample.name == n)).map(proxy_json).collect::<Vec<_>>();
            respond_json(&mut stream, &found).await
        }
        ("GET", "/") if d.assets_dir.is_empty() => respond(&mut stream, 200, "text/html; charset=utf-8", EMBEDDED.as_bytes(), "Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'\r\n").await,
        ("GET", path) if !d.assets_dir.is_empty() => serve_asset(&mut stream, Path::new(&d.assets_dir), path).await,
        _ => write(&mut stream, NOT_FOUND.as_bytes()).await,
    }
}

fn proxy_json(sample: &Sample) -> serde_json::Value {
    let state = if sample.active { "running" } else { "stopped" };
    serde_json::json!({
        "name": sample.name,
        "type": sample.kind,
        "status": state,
        "local_addr": "",
        "remote_addr": "",
        "traffic_in": sample.traffic.bytes_in,
        "traffic_out": sample.traffic.bytes_out,
        "cur_conns": sample.traffic.connections,
        "total_conns": sample.traffic.connections_total,
    })
}

async fn respond_json<S: AsyncWrite + Unpin>(
    stream: &mut S,
    body: &impl serde::Serialize,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    respond(
        stream,
        200,
        "application/json",
        &bytes,
        "Cache-Control: no-store\r\n",
    )
    .await
}

async fn respond<S: AsyncWrite + Unpin>(
    stream: &mut S,
    code: u16,
    kind: &str,
    body: &[u8],
    extra: &str,
) -> Result<(), String> {
    let phrase = match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let header = format!("HTTP/1.1 {code} {phrase}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n{extra}\r\n", body.len());
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    write(stream, body).await
}

async fn write<S: AsyncWrite + Unpin>(stream: &mut S, body: &[u8]) -> Result<(), String> {
    stream.write_all(body).await.map_err(|e| e.to_string())?;
    stream.shutdown().await.map_err(|e| e.to_string())
}

async fn serve_asset<S: AsyncWrite + Unpin>(
    stream: &mut S,
    root: &Path,
    path: &str,
) -> Result<(), String> {
    let root = match tokio::fs::canonicalize(root).await {
        Ok(root) => root,
        Err(_) => return write(stream, NOT_FOUND.as_bytes()).await,
    };
    let relative = path.strip_prefix('/').unwrap_or(path);
    if Path::new(relative)
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return write(stream, NOT_FOUND.as_bytes()).await;
    }
    let mut candidate = root.join(if relative.is_empty() {
        "index.html"
    } else {
        relative
    });
    let mut canonical = match tokio::fs::canonicalize(&candidate).await {
        Ok(file) => file,
        Err(_) => {
            candidate = root.join("index.html");
            match tokio::fs::canonicalize(&candidate).await {
                Ok(file) => file,
                Err(_) => return write(stream, NOT_FOUND.as_bytes()).await,
            }
        }
    };
    if !canonical.starts_with(&root) {
        return write(stream, NOT_FOUND.as_bytes()).await;
    }
    if tokio::fs::metadata(&canonical)
        .await
        .is_ok_and(|m| m.is_dir())
    {
        canonical.push("index.html");
    }
    let metadata = match tokio::fs::metadata(&canonical).await {
        Ok(m) if m.is_file() && m.len() <= MAX_ASSET => m,
        _ => return write(stream, NOT_FOUND.as_bytes()).await,
    };
    let _ = metadata;
    let body = match tokio::fs::read(&canonical).await {
        Ok(bytes) => bytes,
        Err(_) => return write(stream, NOT_FOUND.as_bytes()).await,
    };
    let kind = match canonical
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        _ => "application/octet-stream",
    };
    respond(stream, 200, kind, &body, "").await
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    async fn direct_request(dashboard: &Dashboard, request: &[u8]) -> Vec<u8> {
        let (mut client, mut server) = tokio::io::duplex(64 * 1024);
        let dashboard = dashboard.clone();
        let task = tokio::spawn(async move { handle(&mut server, &dashboard).await });
        client.write_all(request).await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        task.await.unwrap().unwrap();
        response
    }

    async fn asset_response(root: &Path, path: &str) -> Vec<u8> {
        let (mut client, mut server) = tokio::io::duplex(64 * 1024);
        let root = root.to_path_buf();
        let path = path.to_string();
        let task = tokio::spawn(async move { serve_asset(&mut server, &root, &path).await });
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        task.await.unwrap().unwrap();
        response
    }

    fn temporary_directory(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rustfrp-dashboard-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[tokio::test]
    async fn dashboard_auth_json_api_and_assets_are_available() {
        let root = std::env::temp_dir().join(format!("rustfrp-dashboard-{}", std::process::id()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(root.join("index.html"), "dashboard asset")
            .await
            .unwrap();
        let metrics = Arc::new(Metrics::default());
        assert!(metrics.claim("ssh"));
        metrics.set_kind("ssh", "tcp");
        assert!(metrics.claim("stopped"));
        metrics.set_kind("stopped", "tcp");
        metrics.release("stopped");
        let dashboard = Dashboard {
            addr: "127.0.0.1".into(),
            port: 0,
            user: "admin".into(),
            password: "secret".into(),
            assets_dir: root.to_string_lossy().into_owned(),
            metrics,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let cancel = CancellationToken::new();
        tokio::spawn(serve(
            listener,
            None,
            dashboard,
            Logger::silent(),
            cancel.clone(),
        ));
        let addr = format!("127.0.0.1:{port}");
        let (status, html) = crate::admin::request(&addr, "/", "admin", "secret")
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert!(html.contains("dashboard asset"));
        let (status, body) = crate::admin::request(&addr, "/api/serverinfo", "admin", "secret")
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("\"proxy_count\":1"), "{body}");
        let (status, proxies) = crate::admin::request(&addr, "/api/proxy/tcp", "admin", "secret")
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert!(proxies.contains("ssh"), "{proxies}");
        let (status, all_proxies) = crate::admin::request(&addr, "/api/proxy", "admin", "secret")
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert!(all_proxies.contains("ssh"));
        assert!(all_proxies.contains("stopped"));
        let (status, _) = crate::admin::request(&addr, "/api/serverinfo", "admin", "bad")
            .await
            .unwrap();
        assert_eq!(status, 401);
        let no_credentials = direct_request(
            &Dashboard {
                addr: "127.0.0.1".into(),
                port,
                user: String::new(),
                password: String::new(),
                assets_dir: root.to_string_lossy().into_owned(),
                metrics: Arc::new(Metrics::default()),
            },
            b"GET /api/serverinfo HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        let no_credentials = String::from_utf8(no_credentials).unwrap();
        assert!(no_credentials.starts_with("HTTP/1.1 200 OK"));
        assert!(no_credentials.contains("\"proxy_count\":0"));
        cancel.cancel();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn edge_binds_dashboard_with_its_own_tls_identity() {
        let (certificate, private_key) =
            rustfrp_core::tls::self_signed_pem(vec!["localhost".into()]).unwrap();
        let identity =
            rustfrp_core::tls::identity_from_pem(certificate.as_bytes(), private_key.as_bytes())
                .unwrap();
        let pin = rustfrp_core::tls::certificate_fingerprint(identity.certs.first().unwrap());
        let port_probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dashboard_port = port_probe.local_addr().unwrap().port();
        drop(port_probe);
        let edge = crate::edge::Edge::bind_with_dashboard_token(
            crate::edge::EdgeConfig {
                bind_addr: "127.0.0.1".into(),
                bind_port: 0,
                proxy_bind_addr: "127.0.0.1".into(),
                dashboard_addr: "127.0.0.1".into(),
                dashboard_port,
                dashboard_user: "ops".into(),
                dashboard_password: "secret".into(),
                ..Default::default()
            },
            None,
            Some(identity),
            Logger::silent(),
        )
        .await
        .unwrap();
        let cancel = CancellationToken::new();
        tokio::spawn(edge.serve(cancel.clone()));
        let mut invalid_tls = tokio::net::TcpStream::connect(("127.0.0.1", dashboard_port))
            .await
            .unwrap();
        invalid_tls
            .write_all(b"not a TLS ClientHello")
            .await
            .unwrap();
        let mut ignored = Vec::new();
        let _ = invalid_tls.read_to_end(&mut ignored).await;
        let connector = tokio_rustls::TlsConnector::from(
            rustfrp_core::tls::app_client_config_pinned(None, Some(&pin)).unwrap(),
        );
        let (status, body) = crate::admin::request_tls(
            &format!("127.0.0.1:{dashboard_port}"),
            "/api/serverinfo",
            "ops",
            "secret",
            connector,
        )
        .await
        .unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("proxy_count"));
        cancel.cancel();
    }

    #[tokio::test]
    async fn public_dashboard_routes_handle_embedded_index_bad_requests_and_api_filters() {
        let metrics = Arc::new(Metrics::default());
        assert!(metrics.claim("ssh"));
        metrics.set_kind("ssh", "tcp");
        let dashboard = Dashboard {
            addr: "127.0.0.1".into(),
            port: 7000,
            user: String::new(),
            password: String::new(),
            assets_dir: String::new(),
            metrics,
        };

        let response =
            direct_request(&dashboard, b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Security-Policy"));

        let response = direct_request(
            &dashboard,
            b"GET /api/proxy/tcp/ssh HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        let response = String::from_utf8(response).unwrap();
        assert!(response.contains("HTTP/1.1 200 OK"));
        assert!(response.contains("\"name\":\"ssh\""));

        let response = direct_request(
            &dashboard,
            b"GET /api/proxy/udp/missing HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        assert!(String::from_utf8(response).unwrap().ends_with("[]"));

        let response = direct_request(
            &dashboard,
            b"GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        assert!(String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 404 Not Found"));
        let response =
            direct_request(&dashboard, b"POST / HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        assert!(String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 404 Not Found"));

        let response = direct_request(&dashboard, b"not an HTTP request\r\n\r\n").await;
        assert!(String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 400 Bad Request"));

        let large = format!("GET / HTTP/1.1\r\nX-Large: {}", "a".repeat(MAX_HEAD));
        let response = direct_request(&dashboard, large.as_bytes()).await;
        assert!(String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 400 Bad Request"));

        let response = direct_request(&dashboard, b"").await;
        assert!(response.is_empty());
    }

    #[tokio::test]
    async fn assets_cover_directory_fallback_mime_and_filesystem_boundaries() {
        let root = temporary_directory("assets");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("index.html"), "root index").unwrap();
        std::fs::write(root.join("docs/index.html"), "docs index").unwrap();
        for (extension, mime) in [
            ("css", "text/css"),
            ("js", "text/javascript"),
            ("json", "application/json"),
            ("svg", "image/svg+xml"),
            ("png", "image/png"),
            ("bin", "application/octet-stream"),
        ] {
            std::fs::write(root.join(format!("asset.{extension}")), b"asset").unwrap();
            let response = asset_response(&root, &format!("/asset.{extension}")).await;
            let response = String::from_utf8_lossy(&response);
            assert!(response.starts_with("HTTP/1.1 200 OK"));
            assert!(
                response.contains(&format!("Content-Type: {mime}")),
                "{extension}"
            );
        }
        let response = asset_response(&root, "/docs/").await;
        assert!(String::from_utf8_lossy(&response).contains("docs index"));
        let response = asset_response(&root, "/missing-route").await;
        assert!(String::from_utf8_lossy(&response).contains("root index"));
        let response = asset_response(&root, "/../outside").await;
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 404"));

        let large = root.join("large.bin");
        std::fs::File::create(&large)
            .unwrap()
            .set_len(MAX_ASSET + 1)
            .unwrap();
        let response = asset_response(&root, "/large.bin").await;
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 404"));

        let missing_root = root.join("does-not-exist");
        let response = asset_response(&missing_root, "/").await;
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 404"));
        let empty_directory = root.join("empty");
        std::fs::create_dir(&empty_directory).unwrap();
        let response = asset_response(&root, "/empty/").await;
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 404"));

        #[cfg(unix)]
        {
            let outside = temporary_directory("outside");
            std::fs::write(outside.join("secret.txt"), "secret").unwrap();
            std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("escape.txt"))
                .unwrap();
            let response = asset_response(&root, "/escape.txt").await;
            assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 404"));
            std::fs::remove_dir_all(outside).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn response_writer_formats_fallback_status_codes() {
        let (mut client, mut server) = tokio::io::duplex(256);
        let task =
            tokio::spawn(async move { respond(&mut server, 500, "text/plain", b"oops", "").await });
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        task.await.unwrap().unwrap();
        assert!(String::from_utf8(bytes)
            .unwrap()
            .starts_with("HTTP/1.1 500 Error"));
    }
}
