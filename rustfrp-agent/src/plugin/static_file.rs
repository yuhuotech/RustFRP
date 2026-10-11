//! 静态文件服务（frp 的 `static_file`）：只读、拒绝路径穿越与符号链接逃逸；每个连接只服务一个请求。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustfrp_core::error::TunnelError;
use rustfrp_core::http1::{ct_eq, parse_request};
use rustfrp_core::pipe::Traffic;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{read_head, Duplex, PluginCfg};

const HEAD_LIMIT: usize = 16 * 1024;
const MAX_LISTING: usize = 1000;

fn mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("txt" | "log" | "md") => "text/plain; charset=utf-8",
        Some("xml") => "text/xml; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("pdf") => "application/pdf",
        Some("wasm") => "application/wasm",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("mp4") => "video/mp4",
        Some("mp3") => "audio/mpeg",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// 百分号解码；非法编码或 NUL 返回 `None`。
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    if out.contains(&0) {
        return None;
    }
    String::from_utf8(out).ok()
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_url(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// 把请求路径解析成根目录内的安全路径；任何 `..` 段、越界或符号链接逃逸都返回 `None`。
async fn resolve(root: &Path, c: &PluginCfg, request_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(request_path)?;
    let mut rel = decoded.as_str();
    let strip = c.strip_prefix.trim_matches('/');
    if !strip.is_empty() {
        rel = rel.strip_prefix('/')?.strip_prefix(strip)?;
        if !rel.is_empty() && !rel.starts_with('/') {
            return None;
        }
    }
    let mut full = root.to_path_buf();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => return None,
            s if s.contains('\\') => return None,
            s => full.push(s),
        }
    }
    let real = tokio::fs::canonicalize(&full).await.ok()?;
    real.starts_with(root).then_some(real)
}

async fn respond<S: Duplex>(
    s: &mut S,
    traffic: &Traffic,
    status: &str,
    extra: &str,
    body: &[u8],
    head_only: bool,
) -> Result<(), TunnelError> {
    let head = format!(
        "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).await?;
    traffic.record_out(head.len() as u64);
    if !head_only {
        s.write_all(body).await?;
        traffic.record_out(body.len() as u64);
    }
    s.flush().await?;
    Ok(())
}

async fn listing(dir: &Path, shown: &str) -> std::io::Result<Vec<u8>> {
    let mut names = Vec::new();
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(e) = rd.next_entry().await? {
        if names.len() >= MAX_LISTING {
            break;
        }
        let mut name = e.file_name().to_string_lossy().into_owned();
        if e.file_type().await.is_ok_and(|t| t.is_dir()) {
            name.push('/');
        }
        names.push(name);
    }
    names.sort();
    let mut html = format!(
        "<!doctype html><meta charset=utf-8><title>Index of {0}</title><h1>Index of {0}</h1><pre>\n",
        escape_html(shown)
    );
    for n in names {
        html.push_str(&format!(
            "<a href=\"{}\">{}</a>\n",
            escape_url(n.trim_end_matches('/')) + if n.ends_with('/') { "/" } else { "" },
            escape_html(&n)
        ));
    }
    html.push_str("</pre>");
    Ok(html.into_bytes())
}

pub(super) async fn serve<S: Duplex>(
    mut s: S,
    c: &PluginCfg,
    traffic: &Arc<Traffic>,
    t: Duration,
) -> Result<(), TunnelError> {
    let _connection = traffic.connection();
    let (head_len, buf) = read_head(&mut s, HEAD_LIMIT, t).await?;
    traffic.record_in(head_len as u64);
    let head = match parse_request(buf.get(..head_len).unwrap_or(&[])) {
        Ok(Some(h)) => h,
        _ => {
            return respond(&mut s, traffic, "400 Bad Request", "", b"", false).await;
        }
    };
    if !c.http_user.is_empty() || !c.http_password.is_empty() {
        let ok = head
            .basic
            .as_ref()
            .is_some_and(|(u, p)| ct_eq(u, &c.http_user) & ct_eq(p, &c.http_password));
        if !ok {
            return respond(
                &mut s,
                traffic,
                "401 Unauthorized",
                "WWW-Authenticate: Basic realm=\"Restricted\"\r\n",
                b"Unauthorized",
                false,
            )
            .await;
        }
    }
    let head_only = head.method == "HEAD";
    if !head_only && head.method != "GET" {
        return respond(
            &mut s,
            traffic,
            "405 Method Not Allowed",
            "Allow: GET, HEAD\r\n",
            b"",
            false,
        )
        .await;
    }
    let Ok(root) = tokio::fs::canonicalize(&c.local_path).await else {
        return respond(
            &mut s,
            traffic,
            "404 Not Found",
            "",
            b"404 page not found",
            head_only,
        )
        .await;
    };
    let Some(mut path) = resolve(&root, c, &head.path).await else {
        return respond(
            &mut s,
            traffic,
            "404 Not Found",
            "",
            b"404 page not found",
            head_only,
        )
        .await;
    };
    let meta = tokio::fs::metadata(&path).await;
    if meta.as_ref().is_ok_and(|m| m.is_dir()) {
        if !head.path.ends_with('/') {
            let loc = format!("Location: {}/\r\n", head.path);
            return respond(
                &mut s,
                traffic,
                "301 Moved Permanently",
                &loc,
                b"",
                head_only,
            )
            .await;
        }
        let index = path.join("index.html");
        if tokio::fs::metadata(&index).await.is_ok_and(|m| m.is_file()) {
            path = index;
        } else {
            return match listing(&path, &head.path).await {
                Ok(html) => {
                    respond(
                        &mut s,
                        traffic,
                        "200 OK",
                        "Content-Type: text/html; charset=utf-8\r\n",
                        &html,
                        head_only,
                    )
                    .await
                }
                Err(_) => {
                    respond(
                        &mut s,
                        traffic,
                        "403 Forbidden",
                        "",
                        b"403 forbidden",
                        head_only,
                    )
                    .await
                }
            };
        }
    }
    let Ok(mut file) = tokio::fs::File::open(&path).await else {
        return respond(
            &mut s,
            traffic,
            "404 Not Found",
            "",
            b"404 page not found",
            head_only,
        )
        .await;
    };
    let len = file.metadata().await?.len();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        mime(&path)
    );
    s.write_all(header.as_bytes()).await?;
    traffic.record_out(header.len() as u64);
    if !head_only {
        let mut chunk = vec![0u8; 16 * 1024];
        let mut sent = 0u64;
        while sent < len {
            let n = file.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            // 文件在读取期间变长时不超出已声明的长度。
            let take = usize::try_from(len - sent).unwrap_or(usize::MAX).min(n);
            s.write_all(chunk.get(..take).unwrap_or(&[])).await?;
            sent += take as u64;
            traffic.record_out(take as u64);
        }
    }
    s.flush().await?;
    Ok(())
}
