//! 客户端管理接口（frp 的 `webServer`）：`/healthz`、`/api/status`、`/api/reload`、`/api/stop`、`/api/config`，
//! 以及 `reload` / `status` / `stop` 命令行用的请求端。只提供 JSON / 文本接口，不带网页界面。

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustfrp_core::http1::ct_eq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::sync::CancellationToken;

use crate::client::Event;

/// 一个代理的当前状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyStatus {
    /// 类型。
    pub kind: String,
    /// `wait start` / `running` / `start error`。
    pub state: String,
    /// 错误原因。
    pub error: String,
    /// 本地目标。
    pub local: String,
    /// 对外地址（端口）。
    pub remote: String,
}

/// 共享状态表。
pub type StatusTable = Arc<Mutex<BTreeMap<String, ProxyStatus>>>;

/// 一次重载请求；`reply` 返回结果（失败时带原因）。
pub struct ReloadReq {
    /// 应答。
    pub reply: oneshot::Sender<Result<(), String>>,
}

/// 管理接口的运行参数。
pub struct Admin {
    /// 用户名（与密码都为空表示不认证）。
    pub user: String,
    /// 密码。
    pub password: String,
    /// 静态仪表盘资源目录；为空时只提供 JSON 管理接口。
    pub assets_dir: Option<PathBuf>,
    /// 当前代理状态。
    pub status: StatusTable,
    /// 配置文件原文（`/api/config`）。
    pub config_text: Arc<Mutex<String>>,
    /// 重载请求通道。
    pub reload: mpsc::Sender<ReloadReq>,
    /// 退出整个客户端。
    pub stop: CancellationToken,
}

/// 用事件更新状态表，直到通道关闭。
pub async fn track(mut rx: mpsc::Receiver<Event>, table: StatusTable) {
    while let Some(e) = rx.recv().await {
        apply_event(&table, e);
    }
}

/// 把一个客户端事件应用到共享状态表。
pub fn apply_event(table: &StatusTable, event: Event) {
    let mut t = table.lock().unwrap_or_else(PoisonError::into_inner);
    match event {
        Event::ProxyStarted { name, remote_port } => {
            if let Some(s) = t.get_mut(&name) {
                s.state = "running".into();
                s.error.clear();
                s.remote = if remote_port == 0 {
                    String::new()
                } else {
                    format!(":{remote_port}")
                };
            }
        }
        Event::ProxyFailed { name, error } => {
            if let Some(s) = t.get_mut(&name) {
                s.state = "start error".into();
                s.error = error;
            }
        }
        Event::Disconnected(_) => {
            for s in t.values_mut() {
                s.state = "wait start".into();
            }
        }
        _ => {}
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// frp 的 `/api/status` 形状：按类型分组的代理数组。
fn status_json(table: &StatusTable) -> String {
    let t = table.lock().unwrap_or_else(PoisonError::into_inner);
    let mut groups: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (name, s) in t.iter() {
        groups.entry(&s.kind).or_default().push(format!(
            "{{\"name\":\"{}\",\"type\":\"{}\",\"status\":\"{}\",\"err\":\"{}\",\"local_addr\":\"{}\",\"plugin\":\"\",\"remote_addr\":\"{}\"}}",
            json_escape(name),
            json_escape(&s.kind),
            json_escape(&s.state),
            json_escape(&s.error),
            json_escape(&s.local),
            json_escape(&s.remote)
        ));
    }
    let body: Vec<String> = groups
        .into_iter()
        .map(|(k, v)| format!("\"{}\":[{}]", json_escape(k), v.join(",")))
        .collect();
    format!("{{{}}}", body.join(","))
}

async fn respond<S: tokio::io::AsyncWrite + Unpin>(
    s: &mut S,
    status: &str,
    ctype: &str,
    body: &str,
    extra: &str,
) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes()).await;
    let _ = s.write_all(body.as_bytes()).await;
    let _ = s.shutdown().await;
}

async fn respond_bytes<S: tokio::io::AsyncWrite + Unpin>(
    s: &mut S,
    status: &str,
    ctype: &str,
    body: &[u8],
    extra: &str,
) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n{extra}\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes()).await;
    let _ = s.write_all(body).await;
    let _ = s.shutdown().await;
}

fn asset_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}

async fn serve_asset<S: tokio::io::AsyncWrite + Unpin>(s: &mut S, root: &Path, request_path: &str) {
    let root = match tokio::fs::canonicalize(root).await {
        Ok(root) => root,
        Err(_) => return respond(s, "404 Not Found", "text/plain", "not found", "").await,
    };
    let relative = request_path.strip_prefix('/').unwrap_or(request_path);
    if Path::new(relative)
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return respond(s, "404 Not Found", "text/plain", "not found", "").await;
    }
    let mut candidate = root.join(if relative.is_empty() {
        "index.html"
    } else {
        relative
    });
    let mut canonical = match tokio::fs::canonicalize(&candidate).await {
        Ok(path) => path,
        Err(_) => {
            candidate = root.join("index.html");
            match tokio::fs::canonicalize(&candidate).await {
                Ok(path) => path,
                Err(_) => return respond(s, "404 Not Found", "text/plain", "not found", "").await,
            }
        }
    };
    if !canonical.starts_with(&root) {
        return respond(s, "404 Not Found", "text/plain", "not found", "").await;
    }
    if let Ok(meta) = tokio::fs::metadata(&canonical).await {
        if meta.is_dir() {
            canonical.push("index.html");
        }
    }
    let metadata = match tokio::fs::metadata(&canonical).await {
        Ok(meta) if meta.is_file() && meta.len() <= 16 * 1024 * 1024 => meta,
        _ => return respond(s, "404 Not Found", "text/plain", "not found", "").await,
    };
    let _ = metadata;
    match tokio::fs::read(&canonical).await {
        Ok(bytes) => respond_bytes(s, "200 OK", asset_content_type(&canonical), &bytes, "").await,
        Err(_) => respond(s, "404 Not Found", "text/plain", "not found", "").await,
    }
}

async fn handle(mut s: rustfrp_core::transform::BoxAsyncIo, admin: Arc<Admin>) {
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    let head = loop {
        match rustfrp_core::http1::parse_request(&buf) {
            Ok(Some(h)) => break h,
            Ok(None) => {}
            Err(_) => {
                return respond(&mut s, "400 Bad Request", "text/plain", "bad request", "").await
            }
        }
        if buf.len() > 16 * 1024 {
            return;
        }
        match tokio::time::timeout(Duration::from_secs(5), s.read(&mut tmp)).await {
            Ok(Ok(n)) if n > 0 => buf.extend_from_slice(tmp.get(..n).unwrap_or(&[])),
            _ => return,
        }
    };
    if head.path == "/healthz" {
        return respond(&mut s, "200 OK", "text/plain", "", "").await;
    }
    if !admin.user.is_empty() || !admin.password.is_empty() {
        let ok = head
            .basic
            .as_ref()
            .is_some_and(|(u, p)| ct_eq(u, &admin.user) & ct_eq(p, &admin.password));
        if !ok {
            return respond(
                &mut s,
                "401 Unauthorized",
                "text/plain",
                "Unauthorized",
                "WWW-Authenticate: Basic realm=\"RustFRP\"\r\n",
            )
            .await;
        }
    }
    match (head.method.as_str(), head.path.as_str()) {
        ("GET", "/api/status") => {
            respond(
                &mut s,
                "200 OK",
                "application/json",
                &status_json(&admin.status),
                "",
            )
            .await
        }
        ("GET", "/api/config") => {
            let text = admin
                .config_text
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            respond(&mut s, "200 OK", "text/plain; charset=utf-8", &text, "").await
        }
        ("GET", "/api/reload") => {
            let (tx, rx) = oneshot::channel();
            let result = match admin.reload.send(ReloadReq { reply: tx }).await {
                Ok(()) => await_reload(rx, Duration::from_secs(30)).await,
                Err(_) => Err("reload is not available".to_string()),
            };
            match result {
                Ok(()) => respond(&mut s, "200 OK", "text/plain", "", "").await,
                Err(e) => respond(&mut s, "400 Bad Request", "text/plain", &e, "").await,
            }
        }
        ("GET", "/api/stop") => {
            respond(&mut s, "200 OK", "text/plain", "", "").await;
            admin.stop.cancel();
        }
        ("GET", _) => {
            if let Some(root) = admin.assets_dir.as_deref() {
                serve_asset(&mut s, root, &head.path).await;
            } else {
                respond(&mut s, "404 Not Found", "text/plain", "not found", "").await;
            }
        }
        _ => respond(&mut s, "404 Not Found", "text/plain", "not found", "").await,
    }
}

async fn await_reload(
    reply: oneshot::Receiver<Result<(), String>>,
    timeout: Duration,
) -> Result<(), String> {
    match tokio::time::timeout(timeout, reply).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("reload was abandoned".to_string()),
        Err(_) => Err("reload timed out".to_string()),
    }
}

/// 在 `listener` 上提供管理接口，直到 `cancel`。
pub async fn serve(
    listener: TcpListener,
    admin: Arc<Admin>,
    acceptor: Option<TlsAcceptor>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            accepted = listener.accept() => {
                let Ok((conn, _)) = accepted else { continue };
                let acceptor = acceptor.clone();
                let admin = admin.clone();
                tokio::spawn(async move {
                    let stream: rustfrp_core::transform::BoxAsyncIo = match acceptor {
                        Some(acceptor) => match acceptor.accept(conn).await {
                            Ok(tls) => Box::new(tls),
                            Err(_) => return,
                        },
                        None => Box::new(conn),
                    };
                    handle(stream, admin).await;
                });
            }
        }
    }
}

/// 命令行端：向 `addr` 发一次带 Basic 认证的 GET，返回（状态码, 正文）。
pub async fn request(
    addr: &str,
    path: &str,
    user: &str,
    password: &str,
) -> Result<(u16, String), String> {
    let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| format!("connect {addr}: timeout"))?
        .map_err(|e| format!("connect {addr}: {e}"))?;
    request_stream(stream, addr, path, user, password).await
}

/// TLS 加密的命令行管理请求。
pub async fn request_tls(
    addr: &str,
    path: &str,
    user: &str,
    password: &str,
    connector: TlsConnector,
) -> Result<(u16, String), String> {
    let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| format!("connect {addr}: timeout"))?
        .map_err(|e| format!("connect {addr}: {e}"))?;
    let host = addr
        .rsplit_once(':')
        .map_or(addr, |(host, _)| host)
        .trim_matches(['[', ']']);
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| "invalid webServer TLS host name".to_string())?;
    let stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("webServer TLS: {e}"))?;
    request_stream(stream, addr, path, user, password).await
}

async fn request_stream<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut s: S,
    addr: &str,
    path: &str,
    user: &str,
    password: &str,
) -> Result<(u16, String), String> {
    let auth = if user.is_empty() && password.is_empty() {
        String::new()
    } else {
        format!(
            "Authorization: Basic {}\r\n",
            base64(&format!("{user}:{password}"))
        )
    };
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n");
    s.write_all(req.as_bytes())
        .await
        .map_err(|e| format!("send: {e}"))?;
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(35), s.read_to_end(&mut out))
        .await
        .map_err(|_| "read: timeout".to_string())?
        .map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&out).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or("malformed response")?;
    let body = text
        .split_once("\r\n\r\n")
        .map_or(String::new(), |(_, b)| b.to_string());
    Ok((status, body))
}

fn base64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in s.as_bytes().chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let pick =
            |shift: u32| char::from(T.get(((n >> shift) & 63) as usize).copied().unwrap_or(b'A'));
        out.push(pick(18));
        out.push(pick(12));
        out.push(if chunk.len() > 1 { pick(6) } else { '=' });
        out.push(if chunk.len() > 2 { pick(0) } else { '=' });
    }
    out
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn basic_credentials_roundtrip_for_every_padding_length() {
        for (u, p) in [("u", "p:w"), ("ab", "c"), ("abc", "def"), ("", "")] {
            let header = format!("Basic {}", base64(&format!("{u}:{p}")));
            assert_eq!(
                rustfrp_core::http1::parse_basic(&header),
                Some((u.to_string(), p.to_string()))
            );
        }
    }

    #[tokio::test]
    async fn tracking_follows_proxy_events_and_reconnects() {
        let table: StatusTable = Arc::default();
        let status = |kind: &str| ProxyStatus {
            kind: kind.into(),
            state: "wait start".into(),
            error: String::new(),
            local: "127.0.0.1:1".into(),
            remote: String::new(),
        };
        table.lock().unwrap().extend([
            ("a".to_string(), status("tcp")),
            ("b".to_string(), status("udp")),
        ]);
        let (tx, rx) = mpsc::channel(8);
        let task = tokio::spawn(track(rx, table.clone()));
        for e in [
            Event::ProxyStarted {
                name: "a".into(),
                remote_port: 6000,
            },
            Event::ProxyStarted {
                name: "b".into(),
                remote_port: 0,
            },
            Event::ProxyStarted {
                name: "unknown".into(),
                remote_port: 1,
            },
            Event::ProxyFailed {
                name: "b".into(),
                error: "boom".into(),
            },
            Event::ProxyFailed {
                name: "unknown".into(),
                error: "x".into(),
            },
            Event::Connected,
        ] {
            tx.send(e).await.unwrap();
        }
        drop(tx);
        task.await.unwrap();
        {
            let t = table.lock().unwrap();
            assert_eq!(
                (t["a"].state.as_str(), t["a"].remote.as_str()),
                ("running", ":6000")
            );
            assert_eq!(
                (t["b"].state.as_str(), t["b"].error.as_str()),
                ("start error", "boom")
            );
        }
        let json = status_json(&table);
        assert!(
            json.contains("\"tcp\":[") && json.contains("\"udp\":["),
            "{json}"
        );
        // 断线后全部回到等待启动。
        let (tx, rx) = mpsc::channel(8);
        tx.send(Event::Disconnected("lost".into())).await.unwrap();
        drop(tx);
        track(rx, table.clone()).await;
        assert!(table
            .lock()
            .unwrap()
            .values()
            .all(|s| s.state == "wait start"));
    }

    #[test]
    fn json_escaping_covers_control_characters() {
        assert_eq!(
            json_escape("a\"b\\c\n\r\t\u{1}é"),
            "a\\\"b\\\\c\\n\\r\\t\\u0001é"
        );
    }

    #[tokio::test]
    async fn reload_wait_handles_success_abandonment_and_timeout() {
        let (tx, rx) = oneshot::channel();
        tx.send(Ok(())).unwrap();
        assert_eq!(await_reload(rx, Duration::from_secs(1)).await, Ok(()));

        let (tx, rx) = oneshot::channel();
        drop(tx);
        assert_eq!(
            await_reload(rx, Duration::from_secs(1)).await,
            Err("reload was abandoned".into())
        );

        let (_tx, rx) = oneshot::channel();
        assert_eq!(
            await_reload(rx, Duration::from_millis(1)).await,
            Err("reload timed out".into())
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod coverage_tests {
    use super::*;

    async fn handle_response(admin: Arc<Admin>, request: Vec<u8>) -> Vec<u8> {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(async move {
            let stream: rustfrp_core::transform::BoxAsyncIo = Box::new(server);
            handle(stream, admin).await;
        });
        client.write_all(&request).await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        task.await.unwrap();
        response
    }

    fn admin(reload: mpsc::Sender<ReloadReq>, stop: CancellationToken) -> Arc<Admin> {
        Arc::new(Admin {
            user: "admin".into(),
            password: "secret".into(),
            assets_dir: None,
            status: Arc::default(),
            config_text: Arc::new(Mutex::new("frpc config".into())),
            reload,
            stop,
        })
    }

    fn get(path: &str, credentials: bool) -> Vec<u8> {
        let auth = if credentials {
            format!("Authorization: Basic {}\r\n", base64("admin:secret"))
        } else {
            String::new()
        };
        format!("GET {path} HTTP/1.1\r\nHost: local\r\n{auth}Connection: close\r\n\r\n")
            .into_bytes()
    }

    async fn asset_response(root: &Path, path: &str) -> Vec<u8> {
        let (mut client, mut server) = tokio::io::duplex(64 * 1024);
        let root = root.to_path_buf();
        let path = path.to_string();
        let task = tokio::spawn(async move { serve_asset(&mut server, &root, &path).await });
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        task.await.unwrap();
        response
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "rustfrp-admin-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn dashboard_assets_cover_mime_fallback_and_safe_boundaries() {
        let root = temp_dir("assets");
        std::fs::write(root.join("index.html"), "root dashboard").unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/index.html"), "docs dashboard").unwrap();

        for (extension, mime) in [
            ("html", "text/html; charset=utf-8"),
            ("css", "text/css; charset=utf-8"),
            ("js", "text/javascript; charset=utf-8"),
            ("mjs", "text/javascript; charset=utf-8"),
            ("json", "application/json"),
            ("map", "application/json"),
            ("svg", "image/svg+xml"),
            ("png", "image/png"),
            ("jpg", "image/jpeg"),
            ("jpeg", "image/jpeg"),
            ("woff", "font/woff"),
            ("woff2", "font/woff2"),
            ("ttf", "font/ttf"),
            ("unknown", "application/octet-stream"),
        ] {
            let file = root.join(format!("asset.{extension}"));
            std::fs::write(&file, "asset").unwrap();
            assert_eq!(asset_content_type(&file), mime, "{extension}");
        }

        let response = asset_response(&root, "/docs/").await;
        assert!(String::from_utf8_lossy(&response).contains("docs dashboard"));
        let fallback = asset_response(&root, "/client/route").await;
        assert!(String::from_utf8_lossy(&fallback).contains("root dashboard"));
        for path in ["/../secret", "/docs/../../secret"] {
            assert!(String::from_utf8_lossy(&asset_response(&root, path).await)
                .starts_with("HTTP/1.1 404"));
        }

        let empty = root.join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(
            String::from_utf8_lossy(&asset_response(&root, "/empty/").await)
                .starts_with("HTTP/1.1 404")
        );
        let oversized = root.join("oversized.bin");
        std::fs::File::create(&oversized)
            .unwrap()
            .set_len(16 * 1024 * 1024 + 1)
            .unwrap();
        assert!(
            String::from_utf8_lossy(&asset_response(&root, "/oversized.bin").await)
                .starts_with("HTTP/1.1 404")
        );
        assert!(
            String::from_utf8_lossy(&asset_response(&root.join("missing"), "/").await)
                .starts_with("HTTP/1.1 404")
        );

        #[cfg(unix)]
        {
            let outside = temp_dir("outside");
            std::fs::write(outside.join("secret.txt"), "secret").unwrap();
            std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("escape.txt"))
                .unwrap();
            assert!(
                String::from_utf8_lossy(&asset_response(&root, "/escape.txt").await)
                    .starts_with("HTTP/1.1 404")
            );
            std::fs::remove_dir_all(outside).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn dashboard_routes_cover_auth_reload_stop_and_unknown_requests() {
        let (reload, receiver) = mpsc::channel(1);
        drop(receiver);
        let stop = CancellationToken::new();
        let api = admin(reload, stop.clone());
        let health = handle_response(api.clone(), get("/healthz", false)).await;
        assert!(String::from_utf8_lossy(&health).starts_with("HTTP/1.1 200"));
        let denied = handle_response(api.clone(), get("/api/status", false)).await;
        assert!(String::from_utf8_lossy(&denied).starts_with("HTTP/1.1 401"));
        let status = handle_response(api.clone(), get("/api/status", true)).await;
        assert!(String::from_utf8_lossy(&status).contains("{}"));
        let config = handle_response(api.clone(), get("/api/config", true)).await;
        assert!(String::from_utf8_lossy(&config).contains("frpc config"));
        let unknown = handle_response(api.clone(), get("/unknown", true)).await;
        assert!(String::from_utf8_lossy(&unknown).starts_with("HTTP/1.1 404"));
        let unavailable = handle_response(api, get("/api/reload", true)).await;
        assert!(String::from_utf8_lossy(&unavailable).starts_with("HTTP/1.1 400"));

        let (reload, mut receiver) = mpsc::channel(1);
        let success_api = admin(reload, CancellationToken::new());
        let success_task = tokio::spawn(handle_response(
            success_api.clone(),
            get("/api/reload", true),
        ));
        let request = receiver.recv().await.unwrap();
        request.reply.send(Ok(())).unwrap();
        let response = success_task.await.unwrap();
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));

        let (reload, mut receiver) = mpsc::channel(1);
        let abandoned_api = admin(reload, CancellationToken::new());
        let abandoned_task = tokio::spawn(handle_response(abandoned_api, get("/api/reload", true)));
        drop(receiver.recv().await.unwrap());
        let response = abandoned_task.await.unwrap();
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 400"));

        let (reload, _receiver) = mpsc::channel(1);
        let stopped_api = admin(reload, stop.clone());
        let stopped_response = handle_response(stopped_api, get("/api/stop", true)).await;
        assert!(String::from_utf8_lossy(&stopped_response).starts_with("HTTP/1.1 200"));
        assert!(stop.is_cancelled());
    }
}
