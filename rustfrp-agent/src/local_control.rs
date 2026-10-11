//! 有界的回环控制通道；桌面关闭不影响隧道，重开可重新发现同一进程。
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustfrp_core::identity::DeviceKey;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::client::Event;

/// 不含秘密、由实际客户端事件更新的状态。
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// 本地协议版本。
    pub version: u8,
    /// 进程 ID（仅展示，不能用它终止进程）。
    pub pid: u32,
    /// 是否已登录 Edge。
    pub connected: bool,
    /// 最近应用的配置版本。
    pub revision: u64,
    /// 当前已应用的代理数量。
    pub proxies: usize,
}

/// 不含秘密的已应用隧道摘要。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LocalTunnel {
    pub id: String,
    pub kind: String,
    pub local_ip: String,
    pub local_port: u16,
    pub remote_port: u16,
}
/// 有界配置摘要，与转发配置共享生命周期。
pub type TunnelList = Arc<Mutex<(u64, Vec<LocalTunnel>)>>;

/// 验证后原子替换摘要，超限配置拒绝应用。
pub fn update_tunnels(
    list: &TunnelList,
    set: &rustfrp_core::wire::ConfigSet,
) -> Result<(), &'static str> {
    if set.proxies.len() > 1024
        || set
            .proxies
            .iter()
            .any(|p| p.name.len() > 256 || p.kind.len() > 256 || p.local_ip.len() > 256)
    {
        return Err("managed tunnel summary exceeds capacity");
    }
    let items = set
        .proxies
        .iter()
        .map(|p| LocalTunnel {
            id: p.name.clone(),
            kind: p.kind.clone(),
            local_ip: p.local_ip.clone(),
            local_port: p.local_port,
            remote_port: p.remote_port,
        })
        .collect();
    *list.lock().unwrap_or_else(PoisonError::into_inner) = (set.revision, items);
    Ok(())
}

/// 为独立 frpc 配置建立本机只读隧道摘要；不包含密码、令牌或证书私钥。
pub fn update_standalone_tunnels(
    status: &SharedStatus,
    list: &TunnelList,
    proxies: &[crate::client::ProxyCfg],
) -> Result<(), &'static str> {
    if proxies.len() > 1024
        || proxies
            .iter()
            .any(|p| p.name.len() > 256 || p.kind.len() > 256 || p.local_ip.len() > 256)
    {
        return Err("standalone tunnel summary exceeds capacity");
    }
    let items = proxies
        .iter()
        .map(|p| LocalTunnel {
            id: p.name.clone(),
            kind: p.kind.clone(),
            local_ip: p.local_ip.clone(),
            local_port: p.local_port,
            remote_port: p.remote_port,
        })
        .collect::<Vec<_>>();
    *list.lock().unwrap_or_else(PoisonError::into_inner) = (1, items);
    let mut current = status.lock().unwrap_or_else(PoisonError::into_inner);
    current.revision = 1;
    current.proxies = proxies.len();
    Ok(())
}

/// 共享状态只保存几个标量，不缓存配置或日志。
pub type SharedStatus = Arc<Mutex<Status>>;

/// 把连接事件同步到本地状态。
pub fn apply(status: &SharedStatus, event: &Event) {
    let mut s = status.lock().unwrap_or_else(PoisonError::into_inner);
    match event {
        Event::Connected => s.connected = true,
        Event::ConfigApplied { revision, proxies } => {
            s.revision = *revision;
            s.proxies = *proxies;
        }
        Event::ProxyStarted { .. } => {}
        Event::Disconnected(_) => {
            s.connected = false;
            s.proxies = 0;
        }
        _ => {}
    }
}

#[derive(Serialize, Deserialize)]
struct Discovery {
    version: u8,
    port: u16,
    token: String,
    controller: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    token: String,
    command: String,
    #[serde(default)]
    offset: usize,
}

/// 监听器持有端口直到 agent 停止。
pub struct LocalControl {
    listener: TcpListener,
    token: String,
    /// 可供客户端事件更新的状态。
    pub status: SharedStatus,
    /// 与客户端转发共享的固定计数器。
    pub traffic: Arc<rustfrp_core::pipe::Traffic>,
    /// 私有读取的有界日志。
    pub logs: Arc<crate::log_ring::LogRing>,
    /// 实际应用配置摘要。
    pub tunnels: TunnelList,
}

impl LocalControl {
    /// 创建私有发现文件。已有端口仍在监听或绑定其他 Controller 时拒绝启动。
    pub async fn bind(path: &str, controller: &str) -> io::Result<Self> {
        if path.is_empty() || path.len() > 1024 || controller.len() > 1024 {
            return Err(io::Error::other("local control identity is too long"));
        }
        let old = match tokio::fs::File::open(path).await {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(2049).read_to_end(&mut bytes).await?;
                if bytes.len() > 2048 {
                    return Err(io::Error::other("local discovery file too large"));
                }
                let old: Discovery = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if old.version != 1 || old.controller != controller || old.port == 0 {
                    return Err(io::Error::other(
                        "local state belongs to another controller or version",
                    ));
                }
                Some(old)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let existing = old.is_some();
        let listener = TcpListener::bind(("127.0.0.1", old.map_or(0, |o| o.port))).await?;
        // Fresh unpublished random key material; RNG failure is fatal, never a zero token.
        let token = DeviceKey::generate()
            .map_err(io::Error::other)?
            .public_hex();
        let discovery = Discovery {
            version: 1,
            port: listener.local_addr()?.port(),
            token: token.clone(),
            controller: controller.into(),
        };
        let bytes = serde_json::to_vec(&discovery).map_err(io::Error::other)?;
        let path = path.to_string();
        tokio::task::spawn_blocking(move || publish(&path, &bytes, existing))
            .await
            .map_err(io::Error::other)??;
        Ok(Self {
            listener,
            token,
            traffic: Arc::default(),
            logs: Arc::default(),
            tunnels: Arc::default(),
            status: Arc::new(Mutex::new(Status {
                version: 1,
                pid: std::process::id(),
                ..Status::default()
            })),
        })
    }

    /// 单请求缓冲最多 1025 字节，慢请求有超时，取消可立即中断。
    pub async fn serve(self, cancel: CancellationToken) {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                accepted = self.listener.accept() => {
                    let Ok((stream, _)) = accepted else { break };
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::timeout(Duration::from_secs(2), self.handle(stream, &cancel)) => {}
                    }
                }
            }
        }
    }

    async fn handle(&self, stream: TcpStream, cancel: &CancellationToken) -> io::Result<()> {
        let mut stream = BufReader::with_capacity(1024, stream);
        let mut bytes = Vec::with_capacity(1025);
        (&mut stream)
            .take(1025)
            .read_until(b'\n', &mut bytes)
            .await?;
        let request = serde_json::from_slice::<Request>(&bytes);
        let reply = match request {
            Ok(r) if bytes.len() <= 1024 && bytes.last() == Some(&b'\n') => {
                if !constant_time_eq(&r.token, &self.token) {
                    serde_json::json!({"error":"unauthorized"})
                } else {
                    match r.command.as_str() {
                        "status" => {
                            let s = self
                                .status
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .clone();
                            let t = self.traffic.snapshot();
                            serde_json::json!({"version":s.version,"agent_version":env!("CARGO_PKG_VERSION"),"pid":s.pid,"connected":s.connected,"revision":s.revision,"proxies":s.proxies,"traffic":{"bytes_in":t.bytes_in,"bytes_out":t.bytes_out,"connections":t.connections,"connections_total":t.connections_total}})
                        }
                        "tunnels" => {
                            let list = self.tunnels.lock().unwrap_or_else(PoisonError::into_inner);
                            let end = r.offset.saturating_add(32).min(list.1.len());
                            let items = list.1.get(r.offset..end).unwrap_or_default();
                            serde_json::json!({"revision":list.0,"items":items,"next_offset": if end<list.1.len() {Some(end)} else {None}})
                        }
                        "logs" => {
                            serde_json::json!({"items":self.logs.snapshot(),"capacity":crate::log_ring::CAPACITY,"line_bytes":crate::log_ring::LINE_BYTES})
                        }
                        "stop" => serde_json::json!({"ok":true}),
                        _ => serde_json::json!({"error":"invalid_request"}),
                    }
                }
            }
            _ => serde_json::json!({"error":"invalid_request"}),
        };
        let stop = reply.get("ok").is_some();
        let mut reply = serde_json::to_vec(&reply).map_err(io::Error::other)?;
        reply.push(b'\n');
        let result = stream.get_mut().write_all(&reply).await;
        if stop {
            crate::log::Logger::new(crate::log::Level::Info, Box::new(std::io::stderr()))
                .info("local control stop requested");
            cancel.cancel();
        }
        result
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |v, (x, y)| v | (x ^ y)) == 0
}

fn publish(path: &str, bytes: &[u8], existing: bool) -> io::Result<()> {
    use std::io::Write;
    let parent = Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::other("local discovery needs a parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true)
        .create(existing)
        .create_new(!existing)
        .truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    fn path() -> String {
        std::env::temp_dir()
            .join(format!("rustfrp-local-{}", rustfrp_core::auth::random_id()))
            .join("control.json")
            .to_string_lossy()
            .into_owned()
    }
    async fn request(d: &Discovery, text: &str) -> serde_json::Value {
        let mut stream = TcpStream::connect(("127.0.0.1", d.port)).await.unwrap();
        stream.write_all(text.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut result = String::new();
        stream.read_to_string(&mut result).await.unwrap();
        serde_json::from_str(&result).unwrap()
    }
    fn clean(p: &str) {
        std::fs::remove_dir_all(Path::new(p).parent().unwrap()).unwrap();
    }

    #[test]
    fn events_and_fixture() {
        let s = Arc::new(Mutex::new(Status::default()));
        apply(&s, &Event::Connected);
        apply(
            &s,
            &Event::ConfigApplied {
                revision: 7,
                proxies: 2,
            },
        );
        apply(
            &s,
            &Event::ProxyStarted {
                name: "a".into(),
                remote_port: 1,
            },
        );
        apply(
            &s,
            &Event::Enrolled {
                client_id: "id".into(),
            },
        );
        let mut actual = s.lock().unwrap().clone();
        actual.version = 1;
        actual.pid = 123;
        let fixture: Status = serde_json::from_str(include_str!(
            "../../rustfrp-protocol/fixtures/local-status.json"
        ))
        .unwrap();
        assert_eq!(actual, fixture);
        apply(&s, &Event::Disconnected("offline".into()));
        assert!(!s.lock().unwrap().connected);
        assert_eq!(s.lock().unwrap().proxies, 0);
        assert_eq!(s.lock().unwrap().revision, 7);
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("abc", "abd"));
    }

    #[tokio::test]
    async fn requests_lifecycle_and_exclusive_port() {
        let p = path();
        let control = LocalControl::bind(&p, "host:7000").await.unwrap();
        let d: Discovery = serde_json::from_slice(&tokio::fs::read(&p).await.unwrap()).unwrap();
        assert_eq!(d.token.len(), 64);
        assert_eq!(d.version, 1);
        assert_eq!(d.controller, "host:7000");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(LocalControl::bind(&p, "host:7000").await.is_err());
        assert!(LocalControl::bind(&p, "other:7000").await.is_err());
        assert!(LocalControl::bind(&p, &"x".repeat(1025)).await.is_err());
        control.logs.push("test log");
        update_tunnels(
            &control.tunnels,
            &rustfrp_core::wire::ConfigSet {
                revision: 3,
                proxies: vec![
                    rustfrp_core::wire::ManagedProxy {
                        name: "id".into(),
                        kind: "tcp".into(),
                        local_ip: "localhost".into(),
                        local_port: 22,
                        ..Default::default()
                    };
                    33
                ],
                ..Default::default()
            },
        )
        .unwrap();
        control.traffic.record_in(19);
        control.traffic.record_out(23);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(control.serve(cancel.clone()));
        let reply = request(
            &d,
            &format!("{{\"token\":\"{}\",\"command\":\"status\"}}\n", d.token),
        )
        .await;
        assert_eq!(reply["traffic"]["bytes_in"], 19);
        assert_eq!(reply["traffic"]["bytes_out"], 23);
        assert_eq!(reply["traffic"]["connections"], 0);
        assert_eq!(reply["connected"], false);
        assert_eq!(reply["pid"], std::process::id());
        for text in [
            "garbage\n",
            "{}\n",
            "{\"token\":\"bad\",\"command\":\"stop\"}\n",
        ] {
            let result = request(&d, text).await;
            assert!(result.get("error").is_some());
            assert!(!cancel.is_cancelled());
        }
        let command =
            |command: &str| format!("{{\"token\":\"{}\",\"command\":\"{command}\"}}\n", d.token);
        let page = request(&d, &command("tunnels")).await;
        assert_eq!(page["items"].as_array().unwrap().len(), 32);
        assert_eq!(page["next_offset"], 32);
        let last = request(
            &d,
            &format!(
                "{{\"token\":\"{}\",\"command\":\"tunnels\",\"offset\":32}}\n",
                d.token
            ),
        )
        .await;
        assert_eq!(last["items"].as_array().unwrap().len(), 1);
        assert!(last["next_offset"].is_null());
        assert_eq!(
            request(&d, &command("logs")).await["items"],
            serde_json::json!(["test log"])
        );
        assert_eq!(
            request(&d, &command("bad")).await["error"],
            "invalid_request"
        );
        assert_eq!(
            request(&d, command("status").trim_end()).await["error"],
            "invalid_request"
        );
        assert_eq!(
            request(&d, &format!("{}\n", " ".repeat(1025))).await["error"],
            "invalid_request"
        );
        assert_eq!(request(&d, &command("stop")).await["ok"], true);
        task.await.unwrap();
        assert!(cancel.is_cancelled());
        let restarted = LocalControl::bind(&p, "host:7000").await.unwrap();
        assert_ne!(d.token, restarted.token);
        drop(restarted);
        clean(&p);
    }

    #[tokio::test]
    async fn invalid_discovery_and_cancel_slow_request() {
        let p = path();
        tokio::fs::create_dir_all(Path::new(&p).parent().unwrap())
            .await
            .unwrap();
        for value in [
            "bad".to_string(),
            "x".repeat(2049),
            "{\"version\":2,\"port\":1,\"token\":\"t\",\"controller\":\"h\"}".into(),
            "{\"version\":1,\"port\":0,\"token\":\"t\",\"controller\":\"h\"}".into(),
        ] {
            tokio::fs::write(&p, value).await.unwrap();
            assert!(LocalControl::bind(&p, "h").await.is_err());
        }
        tokio::fs::remove_file(&p).await.unwrap();
        let c = LocalControl::bind(&p, "h").await.unwrap();
        let port = c.listener.local_addr().unwrap().port();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(c.serve(cancel.clone()));
        let _slow = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        clean(&p);
    }

    #[tokio::test]
    async fn filesystem_failures() {
        assert!(publish("no-parent", b"x", false).is_err());
        let p = path();
        publish(&p, b"x", false).unwrap();
        assert!(publish(&p, b"y", false).is_err());
        assert!(LocalControl::bind(&format!("{p}/child"), "h")
            .await
            .is_err());
        assert!(publish(&format!("{p}/child"), b"x", false).is_err());
        clean(&p);
    }
    #[test]
    fn bounded_applied_summary_rejects_without_replacing() {
        use rustfrp_core::wire::{ConfigSet, ManagedProxy};
        let list: TunnelList = Arc::default();
        let p = ManagedProxy {
            name: "id".into(),
            kind: "tcp".into(),
            local_ip: "127.0.0.1".into(),
            local_port: 22,
            remote_port: 6000,
            ..Default::default()
        };
        update_tunnels(
            &list,
            &ConfigSet {
                revision: 7,
                proxies: vec![p.clone()],
                ..Default::default()
            },
        )
        .unwrap();
        let saved = list.lock().unwrap();
        assert_eq!(saved.0, 7);
        assert_eq!(saved.1[0].local_port, 22);
        drop(saved);
        assert!(update_tunnels(
            &list,
            &ConfigSet {
                revision: 8,
                proxies: vec![p.clone(); 1025],
                ..Default::default()
            }
        )
        .is_err());
        for field in 0..3 {
            let mut p = p.clone();
            match field {
                0 => p.name = "x".repeat(257),
                1 => p.kind = "x".repeat(257),
                _ => p.local_ip = "x".repeat(257),
            };
            assert!(update_tunnels(
                &list,
                &ConfigSet {
                    revision: 8,
                    proxies: vec![p],
                    ..Default::default()
                }
            )
            .is_err());
        }
        assert_eq!(list.lock().unwrap().0, 7);
        update_tunnels(&list, &ConfigSet::default()).unwrap();
        assert!(list.lock().unwrap().1.is_empty());
    }

    #[test]
    fn standalone_tunnel_summary_excludes_plugin_credentials() {
        let status = Arc::new(Mutex::new(Status {
            version: 1,
            pid: 123,
            ..Default::default()
        }));
        let list = Arc::new(Mutex::new((0, Vec::new())));
        let proxy = crate::client::ProxyCfg {
            name: "local-socks".into(),
            kind: "tcp".into(),
            local_ip: "127.0.0.1".into(),
            local_port: 1080,
            remote_port: 6080,
            plugin: Some(crate::plugin::PluginCfg {
                kind: "socks5".into(),
                username: "alice".into(),
                password: "never-in-summary".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        update_standalone_tunnels(&status, &list, std::slice::from_ref(&proxy)).unwrap();
        let state = status.lock().unwrap().clone();
        assert_eq!((state.revision, state.proxies), (1, 1));
        let snapshot = serde_json::to_string(&list.lock().unwrap().1).unwrap();
        assert!(snapshot.contains("local-socks"));
        assert!(!snapshot.contains("never-in-summary"));
        assert!(update_standalone_tunnels(&status, &list, &vec![proxy; 1025]).is_err());
        let oversized = crate::client::ProxyCfg {
            name: "x".repeat(257),
            ..Default::default()
        };
        assert!(update_standalone_tunnels(&status, &list, &[oversized]).is_err());
        assert_eq!(list.lock().unwrap().1.len(), 1);
    }
}
