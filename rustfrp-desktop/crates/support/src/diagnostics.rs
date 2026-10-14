//! Diagnostics use an explicit field whitelist and never include endpoint or configuration secrets.
use crate::Error;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub fn contents(status: &Value, logs: &Value, captured_at: u64) -> Result<Value, Error> {
    let status = status
        .as_object()
        .ok_or_else(|| Error::invalid("诊断状态无效"))?;
    let logs = logs
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::invalid("诊断日志无效"))?;
    if logs.len() > 128 {
        return Err(Error::invalid("诊断日志超过容量"));
    }
    let mut output = Vec::with_capacity(logs.len());
    for line in logs {
        let line = line
            .as_str()
            .filter(|line| line.len() <= 512)
            .ok_or_else(|| Error::invalid("诊断日志格式或长度无效"))?;
        let lower = line.to_ascii_lowercase();
        output.push(
            if [
                "token",
                "password",
                "secret",
                "authorization",
                "private key",
                "/",
                "\\",
                ":",
            ]
            .iter()
            .any(|part| lower.contains(part))
            {
                "[redacted]"
            } else {
                line
            },
        );
    }
    let selected: Map<String, Value> = ["pid", "connected", "revision", "proxies"]
        .into_iter()
        .filter_map(|key| {
            status
                .get(key)
                .map(|value| (key.to_string(), value.clone()))
        })
        .collect();
    let traffic = status
        .get("traffic")
        .and_then(Value::as_object)
        .map(|traffic| {
            ["bytes_in", "bytes_out", "connections"]
                .into_iter()
                .filter_map(|key| {
                    traffic
                        .get(key)
                        .filter(|value| value.is_u64())
                        .map(|value| (key.to_string(), value.clone()))
                })
                .collect::<Map<String, Value>>()
        });
    Ok(
        json!({"schema_version":1,"captured_at":captured_at,"agent_version":status.get("agent_version").and_then(Value::as_str).unwrap_or(""),"status":selected,"traffic":traffic,"logs":output,"limitations":"状态与最近日志分别读取；进程重启后累计流量归零；路径与端点相关日志已隐藏。"}),
    )
}
pub fn export(root: &Path, value: &Value, captured_at: u64) -> Result<PathBuf, Error> {
    if !root.is_absolute() {
        return Err(Error::invalid("诊断导出目录须为绝对路径"));
    }
    std::fs::create_dir_all(root).map_err(Error::io_context("无法创建诊断目录"))?;
    let bytes = serde_json::to_vec_pretty(value).map_err(Error::invalid_context("无法编码诊断"))?;
    if bytes.len() > 128 * 1024 {
        return Err(Error::invalid("诊断超过大小限制"));
    }
    let destination = root.join(format!(
        "diagnostics-{captured_at}-{}-{}.json",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&destination)
        .map_err(Error::io_context("无法创建私有诊断文件"))?;
    if file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        let _ = std::fs::remove_file(&destination);
        return Err(Error::io("诊断保存失败"));
    }
    Ok(destination)
}
#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_redact_secrets_paths_and_endpoints_and_export_without_overwriting() {
        let status = json!({"pid":42,"connected":true,"revision":2,"proxies":1,"agent_version":"test","controller":"secret-host","token":"secret-token","traffic":{"bytes_in":0,"bytes_out":2,"connections":1,"extra":"secret"}});
        let logs = json!({"items":["agent ready","TOKEN secret","password=p","/private/path","host:7000"]});
        let value = contents(&status, &logs, 1).unwrap();
        let text = value.to_string();
        assert!(!text.contains("secret"));
        assert!(text.contains("[redacted]"));
        assert_eq!(value["traffic"]["bytes_in"], 0);
        assert_eq!(value["logs"][0], "agent ready");
        let root = tempfile::tempdir().unwrap();
        let first = export(root.path(), &value, 1).unwrap();
        let second = export(root.path(), &value, 1).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            std::fs::read(first).unwrap(),
            serde_json::to_vec_pretty(&value).unwrap()
        );
        assert!(contents(&Value::Null, &logs, 1).is_err());
        assert!(contents(&status, &json!({"items":[1]}), 1).is_err());
        assert!(contents(&status, &json!({"items":["x".repeat(513)]}), 1).is_err());
        assert!(contents(&status, &json!({"items":vec!["x";129]}), 1).is_err());
        assert!(export(Path::new("relative"), &value, 1).is_err());
    }
}
