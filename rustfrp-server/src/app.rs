//! 组装并运行 Controller：数据库、Hub、内置 Edge 与管理 API。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustfrp_agent::edge::{Edge, EdgeConfig};
use rustfrp_agent::log::Logger;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::api::{router, AppState, LoginLimiter};
use crate::config::Config;
use crate::db::Db;
use crate::hub::{DbPolicy, Hub};
use crate::store;

/// 运行中的 Controller。
pub struct Server {
    /// 管理 API / 面板的实际监听地址。
    pub http_addr: SocketAddr,
    /// 内置 Edge 的实际监听地址。
    pub edge_addr: SocketAddr,
    /// 数据库。
    pub db: Db,
    /// Hub。
    pub hub: Arc<Hub>,
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

fn split_addr(s: &str) -> Result<(String, u16), String> {
    let (host, port) = s
        .rsplit_once(':')
        .ok_or_else(|| format!("invalid address \"{s}\", expected host:port"))?;
    let port = port
        .parse::<u16>()
        .map_err(|_| format!("invalid port in \"{s}\""))?;
    Ok((host.to_string(), port))
}

/// 启动 Controller（立即返回，后台运行）。
pub async fn start(cfg: Config, log: Logger) -> Result<Server, String> {
    let db = Db::open(&cfg.db_path).map_err(|e| format!("open database: {e}"))?;
    let (hub, events) = Hub::new(db.clone());
    let cancel = CancellationToken::new();
    let mut tasks = vec![tokio::spawn(hub.clone().run_events(events))];

    let (bind_addr, bind_port) = split_addr(&cfg.edge_addr)?;
    let edge_cfg = EdgeConfig {
        bind_addr,
        bind_port,
        proxy_bind_addr: cfg.proxy_bind_addr.clone(),
        allow_ports: cfg.allow_ports.clone(),
        ..Default::default()
    };
    let policy = Arc::new(DbPolicy {
        hub: hub.clone(),
        db: db.clone(),
    });
    let edge = Edge::bind_with_policy(edge_cfg, None, policy, log.clone())
        .await
        .map_err(|e| format!("start edge: {e}"))?;
    let edge_addr = edge.local_addr().map_err(|e| e.to_string())?;
    tasks.push(tokio::spawn(edge.serve(cancel.clone())));

    let listener = TcpListener::bind(&cfg.http_addr)
        .await
        .map_err(|e| format!("listen on {}: {e}", cfg.http_addr))?;
    let http_addr = listener.local_addr().map_err(|e| e.to_string())?;
    let state = AppState {
        db: db.clone(),
        hub: hub.clone(),
        cfg: Arc::new(cfg),
        limiter: Arc::new(LoginLimiter::new(5, 10 * 60 * 1000)),
    };
    let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
    let stop = cancel.clone();
    tasks.push(tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await;
    }));

    // 定期清理过期会话。
    let (purge_db, stop) = (db.clone(), cancel.clone());
    tasks.push(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(3600)) => {
                    let _ = purge_db.call(|c| store::purge_expired_sessions(c, store::now_ms())).await;
                }
            }
        }
    }));

    log.info(&format!(
        "rustfrp-server: management on http://{http_addr}, edge on {edge_addr}"
    ));
    Ok(Server {
        http_addr,
        edge_addr,
        db,
        hub,
        cancel,
        tasks,
    })
}

impl Server {
    /// 停止并等待后台任务结束。
    pub async fn stop(self) {
        self.cancel.cancel();
        for t in self.tasks {
            // 事件任务依赖发送端关闭才会结束，这里不无限等待。
            let _ = tokio::time::timeout(Duration::from_secs(3), t).await;
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn addr_splitting() {
        assert_eq!(
            split_addr("0.0.0.0:7000").unwrap(),
            ("0.0.0.0".into(), 7000)
        );
        assert_eq!(split_addr("[::1]:80").unwrap(), ("[::1]".into(), 80));
        assert!(split_addr("nope").is_err());
        assert!(split_addr("host:abc").is_err());
    }
}
