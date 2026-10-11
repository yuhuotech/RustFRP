//! 本地服务健康检查（frp 的 `healthCheck`）：周期探测，状态变化时通知 Edge。

use std::time::Duration;

use rustfrp_core::wire::HealthSpec;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// 单次探测：tcp 能连上即健康；http 要求 `GET path` 返回 2xx。
pub async fn probe(spec: &HealthSpec, host: &str, port: u16) -> bool {
    let limit = Duration::from_secs(u64::from(spec.timeout_secs.max(1)));
    timeout(limit, async {
        let mut s = TcpStream::connect((host, port)).await.ok()?;
        if spec.kind != "http" {
            return Some(true);
        }
        let mut req = format!(
            "GET {} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n",
            if spec.path.is_empty() {
                "/"
            } else {
                &spec.path
            }
        );
        for (k, v) in &spec.headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).await.ok()?;
        let mut head = [0u8; 64];
        let mut n = 0;
        // 只需要状态行：`HTTP/1.1 200 ...`。
        while n < 12 {
            let k = s.read(head.get_mut(n..)?).await.ok()?;
            if k == 0 {
                break;
            }
            n += k;
        }
        let line = head.get(..n)?;
        let code = line.get(9..12)?;
        Some(line.starts_with(b"HTTP/") && code.first() == Some(&b'2'))
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}

/// 启动一个监控任务：状态变化（初始假定不健康）时向 `tx` 发送（代理名, 是否健康）。
pub fn spawn(
    name: String,
    spec: HealthSpec,
    host: String,
    port: u16,
    tx: mpsc::Sender<(String, bool)>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let every = Duration::from_secs(u64::from(spec.interval_secs.max(1)));
        let max_failed = spec.max_failed.max(1);
        // 初始假定不健康（与 Edge 一致）；第一次探测结果一定通知 Edge，让它回报准确状态。
        let (mut healthy, mut failed, mut first) = (false, 0u32, true);
        loop {
            let ok = probe(&spec, &host, port).await;
            if ok {
                failed = 0;
            } else {
                failed = failed.saturating_add(1);
            }
            let next = if ok {
                true
            } else if failed >= max_failed {
                false
            } else {
                healthy
            };
            if next != healthy || first {
                healthy = next;
                first = false;
                if tx.send((name.clone(), healthy)).await.is_err() {
                    return;
                }
            }
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(every) => {}
            }
        }
    });
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tokio::net::TcpListener;

    fn spec(kind: &str, path: &str) -> HealthSpec {
        HealthSpec {
            kind: kind.into(),
            timeout_secs: 1,
            max_failed: 2,
            interval_secs: 1,
            path: path.into(),
            headers: BTreeMap::new(),
        }
    }

    /// 回一段固定内容（原样写出）后关闭；返回端口与收到的请求。
    async fn server(reply: &'static [u8]) -> (u16, tokio::sync::mpsc::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let mut b = [0u8; 1024];
                let n = s.read(&mut b).await.unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&b[..n]).into_owned()).await;
                let _ = s.write_all(reply).await;
            }
        });
        (port, rx)
    }

    #[tokio::test]
    async fn probes_judge_by_connect_or_a_2xx_status() {
        let (ok, mut seen) = server(b"HTTP/1.1 204 No Content\r\n\r\n").await;
        let mut with_headers = spec("http", "");
        with_headers.headers.insert("X-Probe".into(), "yes".into());
        assert!(probe(&with_headers, "127.0.0.1", ok).await);
        let req = seen.recv().await.unwrap();
        assert!(
            req.starts_with("GET / HTTP/1.1\r\n"),
            "empty path means /: {req}"
        );
        assert!(req.contains("X-Probe: yes\r\n") && req.contains("Connection: close"));
        let (bad, _) = server(b"HTTP/1.1 503 Busy\r\n\r\n").await;
        assert!(!probe(&spec("http", "/h"), "127.0.0.1", bad).await);
        // 协议不对 / 提前关闭：不健康；tcp 只看能否连上。
        let (junk, _) = server(b"nope").await;
        assert!(!probe(&spec("http", "/h"), "127.0.0.1", junk).await);
        let (closed, _) = server(b"").await;
        assert!(!probe(&spec("http", "/h"), "127.0.0.1", closed).await);
        assert!(probe(&spec("tcp", ""), "127.0.0.1", closed).await);
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead = l.local_addr().unwrap().port();
        drop(l);
        assert!(!probe(&spec("tcp", ""), "127.0.0.1", dead).await);
    }

    #[tokio::test]
    async fn monitor_reports_first_result_then_only_changes_after_max_failed() {
        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        spawn(
            "p".into(),
            spec("tcp", ""),
            "127.0.0.1".into(),
            port,
            tx,
            cancel.clone(),
        );
        // 第一次探测成功：立即通知「健康」。
        assert_eq!(rx.recv().await, Some(("p".to_string(), true)));
        // 服务消失：一次失败还不够（maxFailed = 2），第二次才变不健康。
        let started = tokio::time::Instant::now();
        drop(l);
        assert_eq!(rx.recv().await, Some(("p".to_string(), false)));
        assert!(
            started.elapsed() >= Duration::from_millis(900),
            "needs two failed rounds"
        );
        cancel.cancel();
        // 通道关闭后监控任务退出（不 panic）。
        let (tx2, rx2) = mpsc::channel(1);
        drop(rx2);
        spawn(
            "q".into(),
            spec("tcp", ""),
            "127.0.0.1".into(),
            1,
            tx2,
            CancellationToken::new(),
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
