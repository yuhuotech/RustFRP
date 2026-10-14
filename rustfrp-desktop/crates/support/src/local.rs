//! Private Agent discovery and bounded line-based TCP control, never exposed as HTTP.

use crate::Error;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Deserialize)]
struct Discovery {
    version: u8,
    port: u16,
    token: String,
    controller: String,
}

fn unavailable() -> Error {
    Error {
        code: "agent_not_running".into(),
        ..Error::io("未发现正在运行的本机 Agent")
    }
}
fn invalid_reply(message: &str) -> Error {
    Error {
        code: "invalid_response".into(),
        ..Error::invalid(message)
    }
}

async fn discovery(path: &Path, identity: &str) -> Result<Discovery, Error> {
    let metadata = tokio::fs::symlink_metadata(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            unavailable()
        } else {
            Error::io("Agent 发现文件不可读")
        }
    })?;
    if !metadata.is_file() || metadata.len() > 2048 {
        return Err(Error::invalid("Agent 发现文件必须是有界普通文件"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::invalid("Agent 发现文件必须仅当前用户可读写"));
        }
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(Error::io_context("Agent 发现文件不可读"))?;
    let mut bytes = Vec::new();
    file.take(2049)
        .read_to_end(&mut bytes)
        .await
        .map_err(Error::io_context("读取 Agent 发现文件失败"))?;
    if bytes.len() > 2048 {
        return Err(Error::invalid("Agent 发现文件过大"));
    }
    let value: Discovery =
        serde_json::from_slice(&bytes).map_err(Error::invalid_context("Agent 发现文件无效"))?;
    if value.version != 1
        || value.port == 0
        || value.controller != identity
        || value.token.len() != 64
        || !value.token.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(Error::invalid(
            "状态目录属于其他控制器，或发现文件版本不兼容；请使用独立状态目录",
        ));
    }
    Ok(value)
}

/// Only absence and an explicitly refused connection prove the Agent is stopped.
pub async fn status(path: &Path, identity: &str) -> Result<Option<Value>, Error> {
    match request(path, identity, "status", None, None).await {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.code == "agent_not_running" => Ok(None),
        Err(error) => Err(error),
    }
}

fn validate_reply(command: &str, value: &Value) -> Result<(), Error> {
    let number = |key: &str| value.get(key).and_then(Value::as_u64);
    let valid = match command {
        "status" => {
            number("version") == Some(1)
                && number("pid").is_some_and(|pid| pid > 0 && pid <= u32::MAX.into())
                && value.get("connected").is_some_and(Value::is_boolean)
                && number("revision").is_some()
                && number("proxies").is_some_and(|n| n <= 1024)
                && value
                    .get("agent_version")
                    .is_none_or(|v| v.as_str().is_some_and(|s| s.len() <= 256))
                && value.get("traffic").is_none_or(|traffic| {
                    ["bytes_in", "bytes_out", "connections"]
                        .iter()
                        .all(|key| traffic.get(key).and_then(Value::as_u64).is_some())
                })
        }
        "stop" => value.get("ok") == Some(&Value::Bool(true)),
        "logs" => {
            value
                .get("items")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.len() <= 128
                        && items
                            .iter()
                            .all(|v| v.as_str().is_some_and(|s| s.len() <= 512))
                })
                && value
                    .get("cursor")
                    .is_none_or(|v| v.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991))
                && value.get("gap").is_none_or(Value::is_boolean)
        }
        "tunnels" => {
            number("revision").is_some()
                && value
                    .get("items")
                    .and_then(Value::as_array)
                    .is_some_and(|items| {
                        items.len() <= 32
                            && items.iter().all(|item| {
                                ["id", "kind", "local_ip"].iter().all(|key| {
                                    item.get(key)
                                        .and_then(Value::as_str)
                                        .is_some_and(|s| s.len() <= 256)
                                }) && ["local_port", "remote_port"].iter().all(|key| {
                                    item.get(key)
                                        .and_then(Value::as_u64)
                                        .is_some_and(|n| n <= 65535)
                                })
                            })
                    })
                && value
                    .get("next_offset")
                    .is_none_or(|v| v.is_null() || v.as_u64().is_some_and(|n| n <= 1024))
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid_reply("本机 Agent 响应不兼容，请更新 Agent 后重试"))
    }
}

/// Read bounded pages from one configuration revision without publishing secrets.
pub async fn tunnels(path: &Path, identity: &str) -> Result<Value, Error> {
    tokio::time::timeout(Duration::from_secs(10), tunnel_pages(path, identity))
        .await
        .map_err(|_| Error {
            code: "local_timeout".into(),
            ..Error::io("本机隧道读取超时，请刷新重试")
        })?
}
async fn tunnel_pages(path: &Path, identity: &str) -> Result<Value, Error> {
    let mut result = Vec::new();
    let mut offset = 0;
    let mut revision = None;
    loop {
        let value = request(path, identity, "tunnels", None, Some(offset)).await?;
        let current = value
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::invalid("本机隧道版本无效"))?;
        if revision.is_some_and(|revision| revision != current) {
            return Err(Error::invalid("读取过程中本机隧道配置发生变化，请刷新"));
        }
        revision = Some(current);
        let items = value
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::invalid("本机隧道列表无效"))?;
        if result.len() + items.len() > 1024 {
            return Err(Error::invalid("本机隧道超过容量限制"));
        }
        result.extend(items.iter().cloned());
        match value.get("next_offset").and_then(Value::as_u64) {
            None => return Ok(json!({"revision":current,"items":result})),
            Some(next) if items.len() == 32 && next == offset + 32 && next < 1024 => offset = next,
            _ => return Err(Error::invalid("本机隧道分页无效，请刷新")),
        }
    }
}

pub async fn request(
    path: &Path,
    identity: &str,
    command: &str,
    after: Option<u64>,
    offset: Option<u64>,
) -> Result<Value, Error> {
    if !matches!(command, "status" | "stop" | "logs" | "tunnels")
        || (after.is_some() && command != "logs")
        || (offset.is_some() && command != "tunnels")
        || offset.is_some_and(|offset| offset > 1024)
    {
        return Err(Error::invalid("不允许的本机 Agent 命令"));
    }
    let discovery = discovery(path, identity).await?;
    let limit = if matches!(command, "logs" | "tunnels") {
        512 * 1024
    } else {
        4096
    };
    let mut sent = false;
    let work = async {
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", discovery.port))
            .await
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::ConnectionRefused {
                    unavailable()
                } else {
                    Error::io("无法连接本机 Agent")
                }
            })?;
        let mut message = json!({"token":discovery.token,"command":command});
        if let Some(object) = message.as_object_mut() {
            if let Some(after) = after {
                object.insert("after".into(), json!(after));
            }
            if let Some(offset) = offset {
                object.insert("offset".into(), json!(offset));
            }
        }
        let mut request =
            serde_json::to_vec(&message).map_err(Error::invalid_context("本机请求编码失败"))?;
        request.push(b'\n');
        if request.len() > 1024 {
            return Err(Error::invalid("本机请求过大"));
        }
        sent = true;
        socket
            .write_all(&request)
            .await
            .map_err(Error::io_context("发送本机请求失败"))?;
        let mut reader = BufReader::new(socket).take(limit + 1);
        let mut reply = Vec::new();
        reader
            .read_until(b'\n', &mut reply)
            .await
            .map_err(Error::io_context("读取本机响应失败"))?;
        if reply.len() > limit as usize {
            return Err(invalid_reply("本机响应超过大小限制"));
        }
        if reply.last() != Some(&b'\n') {
            return Err(invalid_reply("本机响应不完整，请刷新确认状态"));
        }
        let value: Value =
            serde_json::from_slice(&reply).map_err(|_| invalid_reply("本机响应无效"))?;
        if let Some(error) = value.get("error") {
            return Err(Error::invalid(
                if error.as_str() == Some("invalid_request") {
                    "本机 Agent 版本不支持此命令，请更新 Agent"
                } else {
                    "本机控制认证失败，请核对身份与状态目录"
                },
            ));
        }
        validate_reply(command, &value)?;
        Ok(value)
    };
    let result = tokio::time::timeout(Duration::from_secs(2), work)
        .await
        .map_err(|_| Error {
            code: "local_timeout".into(),
            uncertain: command == "stop",
            ..Error::io("本机控制请求超时，请刷新确认状态")
        })?;
    result.map_err(|mut error| {
        if command == "stop"
            && sent
            && matches!(error.code.as_str(), "local_io" | "invalid_response")
        {
            error.uncertain = true;
        }
        error
    })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn loopback_control_authenticates_without_returning_discovery_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::fs::write(
            &path,
            json!({"version":1,"port":port,"token":"a".repeat(64),"controller":"host:7000"})
                .to_string(),
        )
        .await
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["token"], "a".repeat(64));
            assert_eq!(value["command"], "status");
            reader
                .get_mut()
                .write_all(
                    b"{\"version\":1,\"pid\":42,\"connected\":true,\"revision\":0,\"proxies\":0}\n",
                )
                .await
                .unwrap();
        });
        let status = request(&path, "host:7000", "status", None, None)
            .await
            .unwrap();
        assert_eq!(status["pid"], 42);
        assert!(status.get("token").is_none());
        server.await.unwrap();
        assert!(request(&path, "different", "status", None, None)
            .await
            .is_err());
        assert!(request(&path, "host:7000", "shell", None, None)
            .await
            .is_err());
    }

    async fn publish(path: &Path, port: u16) {
        tokio::fs::write(
            path,
            json!({"version":1,"port":port,"token":"a".repeat(64),"controller":"host:7000"})
                .to_string(),
        )
        .await
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    async fn fixture(path: &Path, replies: Vec<Value>) -> tokio::task::JoinHandle<Vec<Value>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        publish(path, listener.local_addr().unwrap().port()).await;
        tokio::spawn(async move {
            let mut requests = vec![];
            for reply in replies {
                let (socket, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(socket);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                requests.push(serde_json::from_str(&line).unwrap());
                reader
                    .get_mut()
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
            }
            requests
        })
    }
    fn tunnel(index: usize) -> Value {
        json!({"id":format!("t-{index}"),"kind":"tcp","local_ip":"127.0.0.1","local_port":80,"remote_port":0})
    }

    #[tokio::test]
    async fn discovery_errors_cannot_be_mistaken_for_a_stopped_agent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.json");
        assert_eq!(status(&path, "host:7000").await.unwrap(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        publish(&path, port).await;
        assert_eq!(status(&path, "host:7000").await.unwrap(), None);
        assert!(status(&path, "other").await.is_err());
        for bad in [
            "not json".to_string(),
            json!({"version":2,"port":port,"controller":"host:7000","token":"a".repeat(64)})
                .to_string(),
            "x".repeat(2049),
        ] {
            tokio::fs::write(&path, bad).await.unwrap();
            assert!(status(&path, "host:7000").await.is_err());
        }
        tokio::fs::remove_file(&path).await.unwrap();
        tokio::fs::create_dir(&path).await.unwrap();
        assert!(status(&path, "host:7000").await.is_err());
        tokio::fs::remove_dir(&path).await.unwrap();
        publish(&path, port).await;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(status(&path, "host:7000").await.is_err());
        }
        for (command, after, offset) in [
            ("shell", None, None),
            ("status", Some(1), None),
            ("stop", None, Some(0)),
            ("tunnels", None, Some(1025)),
        ] {
            assert!(request(&path, "host:7000", command, after, offset)
                .await
                .is_err());
        }
    }
    #[test]
    fn reply_bounds_preserve_zero_and_reject_untrusted_versions_ports_and_cursors() {
        let status = json!({"version":1,"pid":1,"connected":false,"revision":0,"proxies":0,"traffic":{"bytes_in":0,"bytes_out":0,"connections":0},"agent_version":"0.1.0"});
        validate_reply("status", &status).unwrap();
        for (key, value) in [
            ("pid", json!(0)),
            ("proxies", json!(1025)),
            ("connected", json!("false")),
            ("traffic", json!({})),
            ("agent_version", json!("x".repeat(257))),
        ] {
            let mut bad = status.clone();
            bad[key] = value;
            assert!(validate_reply("status", &bad).is_err());
        }
        for valid in [
            json!({"items":[],"cursor":0,"gap":false}),
            json!({"items":["legacy"]}),
        ] {
            validate_reply("logs", &valid).unwrap();
        }
        for bad in [
            json!({"items":vec!["line";129]}),
            json!({"items":["x".repeat(513)]}),
            json!({"items":[],"cursor":9007199254740992_u64}),
            json!({"items":[],"gap":1}),
        ] {
            assert!(validate_reply("logs", &bad).is_err());
        }
        validate_reply(
            "tunnels",
            &json!({"revision":0,"items":[tunnel(0)],"next_offset":null}),
        )
        .unwrap();
        let mut bad = tunnel(0);
        bad["remote_port"] = json!(65536);
        assert!(validate_reply("tunnels", &json!({"revision":0,"items":[bad]})).is_err());
        assert!(
            validate_reply("tunnels", &json!({"revision":0,"items":vec![tunnel(0);33]})).is_err()
        );
        assert!(validate_reply("stop", &json!({"ok":false})).is_err());
        assert!(validate_reply("unknown", &json!({})).is_err());
    }
    #[tokio::test]
    async fn applied_tunnels_aggregate_stable_pages_and_fail_on_revision_or_cursor_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.json");
        let first =
            json!({"revision":7,"items":(0..32).map(tunnel).collect::<Vec<_>>(),"next_offset":32});
        let server = fixture(
            &path,
            vec![
                first.clone(),
                json!({"revision":7,"items":[tunnel(32)],"next_offset":null}),
            ],
        )
        .await;
        let all = tunnels(&path, "host:7000").await.unwrap();
        assert_eq!(all["revision"], 7);
        assert_eq!(all["items"].as_array().unwrap().len(), 33);
        let requests = server.await.unwrap();
        assert_eq!(requests[0]["offset"], 0);
        assert_eq!(requests[1]["offset"], 32);
        let server = fixture(
            &path,
            vec![first, json!({"revision":8,"items":[],"next_offset":null})],
        )
        .await;
        assert!(tunnels(&path, "host:7000")
            .await
            .unwrap_err()
            .message
            .contains("配置发生变化"));
        server.await.unwrap();
        let server = fixture(
            &path,
            vec![json!({"revision":7,"items":[tunnel(0)],"next_offset":32})],
        )
        .await;
        assert!(tunnels(&path, "host:7000")
            .await
            .unwrap_err()
            .message
            .contains("分页"));
        server.await.unwrap();
        for error in ["invalid_request", "auth_failed"] {
            let server = fixture(&path, vec![json!({"error":error})]).await;
            let error = request(&path, "host:7000", "logs", Some(1), None)
                .await
                .unwrap_err();
            assert!(!error.uncertain);
            server.await.unwrap();
        }
        let server = fixture(&path, vec![json!({"ok":false})]).await;
        assert!(
            request(&path, "host:7000", "stop", None, None)
                .await
                .unwrap_err()
                .uncertain
        );
        server.await.unwrap();
    }
}
