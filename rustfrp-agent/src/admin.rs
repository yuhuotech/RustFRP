//! 客户端管理接口（frp 的 `webServer`）：`/healthz`、`/api/status`、`/api/reload`、`/api/stop`、`/api/config`，
//! 以及 `reload` / `status` / `stop` 命令行用的请求端。只提供 JSON / 文本接口，不带网页界面。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustfrp_core::http1::ct_eq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
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
        let mut t = table.lock().unwrap_or_else(PoisonError::into_inner);
        match e {
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

async fn respond(s: &mut TcpStream, status: &str, ctype: &str, body: &str, extra: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes()).await;
    let _ = s.write_all(body.as_bytes()).await;
    let _ = s.shutdown().await;
}

async fn handle(mut s: TcpStream, admin: Arc<Admin>) {
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
                Ok(()) => match tokio::time::timeout(Duration::from_secs(30), rx).await {
                    Ok(Ok(r)) => r,
                    Ok(Err(_)) => Err("reload was abandoned".to_string()),
                    Err(_) => Err("reload timed out".to_string()),
                },
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
        _ => respond(&mut s, "404 Not Found", "text/plain", "not found", "").await,
    }
}

/// 在 `listener` 上提供管理接口，直到 `cancel`。
pub async fn serve(listener: TcpListener, admin: Arc<Admin>, cancel: CancellationToken) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            accepted = listener.accept() => {
                let Ok((conn, _)) = accepted else { continue };
                tokio::spawn(handle(conn, admin.clone()));
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
    let mut s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| format!("connect {addr}: timeout"))?
        .map_err(|e| format!("connect {addr}: {e}"))?;
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
}
