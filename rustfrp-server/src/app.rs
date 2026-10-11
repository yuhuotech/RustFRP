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

/// 运行中的 Controller。
pub struct Server {
    /// 管理 API / 面板的实际监听地址。
    pub http_addr: SocketAddr,
    /// 内置 Edge 的实际监听地址。
    pub edge_addr: SocketAddr,
    /// HTTP 虚拟主机的实际监听地址（未启用为 `None`）。
    pub vhost_http_addr: Option<SocketAddr>,
    /// 节点端口的实际监听地址（未启用为 `None`）。
    pub node_addr: Option<SocketAddr>,
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

fn material_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn pem_error(error: std::io::Error) -> String {
    format!("read edge TLS material: {error}")
}

async fn read_pem(path: &std::path::Path) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await.map_err(pem_error)?;
    let mut bytes = Vec::new();
    file.take(65537)
        .read_to_end(&mut bytes)
        .await
        .map_err(pem_error)?;
    if bytes.len() > 65536 {
        return Err("edge TLS material exceeds 64 KiB".into());
    }
    Ok(bytes)
}
async fn persistent_identity(cfg: &Config) -> Result<rustfrp_core::tls::Identity, String> {
    let dir = cfg.db_path.with_extension("edge-tls");
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    if !tokio::fs::try_exists(&dir).await.map_err(material_error)? {
        let names = vec!["rustfrp".to_string(), cfg.public_host.clone()];
        let (certificate, private) =
            rustfrp_core::tls::self_signed_pem(names).map_err(material_error)?;
        let staged = dir.with_extension(format!("edge-tls-{}.partial", crate::store::new_id()));
        let publish = tokio::task::spawn_blocking(move || -> Result<(), String> {
            use std::io::Write;
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&staged).map_err(material_error)?;
            let result = (|| {
                for (name, contents) in [("cert.pem", certificate), ("key.pem", private)] {
                    let mut options = std::fs::OpenOptions::new();
                    options.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    let mut file = options.open(staged.join(name)).map_err(material_error)?;
                    file.write_all(contents.as_bytes())
                        .map_err(material_error)?;
                    file.sync_all().map_err(material_error)?;
                }
                std::fs::rename(&staged, &dir).map_err(material_error)
            })();
            if result.is_err() {
                let _ = std::fs::remove_dir_all(staged);
            }
            result
        })
        .await
        .map_err(material_error)?;
        publish?;
    }
    rustfrp_core::tls::identity_from_pem(&read_pem(&cert).await?, &read_pem(&key).await?)
        .map_err(material_error)
}

async fn edge_identity(cfg: &Config) -> Result<Option<rustfrp_core::tls::Identity>, String> {
    match (&cfg.edge_tls_cert, &cfg.edge_tls_key) {
        (None, None) => persistent_identity(cfg).await.map(Some),
        (Some(cert), Some(key)) => {
            rustfrp_core::tls::identity_from_pem(&read_pem(cert).await?, &read_pem(key).await?)
                .map(Some)
                .map_err(|e| format!("edge TLS identity: {e}"))
        }
        _ => Err("edge TLS certificate and key must be configured together".into()),
    }
}

/// 启动 Controller（立即返回，后台运行）。
pub async fn start(mut cfg: Config, log: Logger) -> Result<Server, String> {
    let logs = Arc::new(rustfrp_agent::log_ring::LogRing::default());
    let log = log.with_ring(logs.clone());
    crate::process::mark_start();
    let db = Db::open(&cfg.db_path).map_err(|e| format!("open database: {e}"))?;
    let identity = edge_identity(&cfg).await?;
    let (hub, events) = Hub::new(db.clone());
    let cancel = CancellationToken::new();

    let (bind_addr, bind_port) = split_addr(&cfg.edge_addr)?;
    let edge_cfg = EdgeConfig {
        bind_addr,
        bind_port,
        proxy_bind_addr: cfg.proxy_bind_addr.clone(),
        allow_ports: cfg.allow_ports.clone(),
        vhost_http_port: cfg.vhost_http,
        vhost_https_port: cfg.vhost_https,
        sub_domain_host: cfg.subdomain_host.clone(),
        ..Default::default()
    };
    if !cfg.builtin_edge && cfg.node_addr.is_empty() {
        return Err(
            "the built-in edge is disabled and no node port is configured; set RUSTFRP_NODE_ADDR"
                .into(),
        );
    }
    let policy = Arc::new(DbPolicy {
        hub: hub.clone(),
        db: db.clone(),
    });
    let edge = if cfg.builtin_edge {
        let edge = Edge::bind_with_policy(edge_cfg, identity, policy, log.clone())
            .await
            .map_err(|e| format!("start edge: {e}"))?;
        cfg.edge_tls_fingerprint = edge.certificate_fingerprint().into();
        Some(edge)
    } else {
        // 不监听客户端接入端口，但仍需要证书指纹：节点端口复用同一张证书。
        cfg.edge_tls_fingerprint = identity
            .as_ref()
            .and_then(|i| i.certs.first())
            .map(rustfrp_core::tls::certificate_fingerprint)
            .unwrap_or_default();
        None
    };
    let edge_addr = match &edge {
        Some(e) => e.local_addr().map_err(material_error)?,
        None => SocketAddr::from(([0, 0, 0, 0], 0)),
    };
    let vhost_http_addr = edge.as_ref().and_then(Edge::http_addr);
    let metrics = edge.as_ref().map(Edge::metrics).unwrap_or_default();
    let listener = TcpListener::bind(&cfg.http_addr)
        .await
        .map_err(|e| format!("listen on {}: {e}", cfg.http_addr))?;
    let http_addr = listener.local_addr().map_err(material_error)?;
    let mut tasks = vec![tokio::spawn(hub.clone().run_events(events))];
    if let Some(edge) = edge {
        tasks.push(tokio::spawn(edge.serve(cancel.clone())));
    }
    let node_addr = if cfg.node_addr.is_empty() {
        None
    } else {
        // 节点端口复用内置 Edge 的证书，运维只需要管理一份证书。
        let id = edge_identity(&cfg)
            .await?
            .ok_or("node port needs the edge TLS identity")?;
        let acceptor = tokio_rustls::TlsAcceptor::from(
            rustfrp_core::tls::server_config(id).map_err(material_error)?,
        );
        let listener = TcpListener::bind(&cfg.node_addr)
            .await
            .map_err(|e| format!("listen on {}: {e}", cfg.node_addr))?;
        let addr = listener.local_addr().map_err(material_error)?;
        let ctx = crate::nodes::NodeServer {
            db: db.clone(),
            hub: hub.clone(),
            log: log.clone(),
            silence: crate::nodes::DEFAULT_SILENCE,
        };
        tasks.push(tokio::spawn(crate::nodes::serve(
            ctx,
            listener,
            acceptor,
            cancel.clone(),
        )));
        Some(addr)
    };
    tasks.push(tokio::spawn(crate::traffic::run(
        db.clone(),
        metrics.clone(),
        crate::store::new_id(),
        Duration::from_secs(5),
        cancel.clone(),
        log.clone(),
    )));

    let retention = (cfg.audit_retention_days, cfg.cleanup_batch);
    let state = AppState {
        db: db.clone(),
        hub: hub.clone(),
        cfg: Arc::new(cfg),
        limiter: Arc::new(LoginLimiter::new(5, 10 * 60 * 1000)),
        metrics,
        logs,
    };
    let app = router(state).into_make_service_with_connect_info::<SocketAddr>();
    let stop = cancel.clone();
    tasks.push(tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await;
    }));

    // 每分钟一批，避免长期持有单连接阻塞管理 API；每表最多 1000 行。
    tasks.push(tokio::spawn(crate::maintenance::run(
        db.clone(),
        retention.0,
        retention.1,
        Duration::from_secs(60),
        cancel.clone(),
        log.clone(),
    )));

    log.info(&format!(
        "rustfrp-server: management on http://{http_addr}, edge on {edge_addr}"
    ));
    Ok(Server {
        http_addr,
        edge_addr,
        vhost_http_addr,
        node_addr,
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
        for mut task in self.tasks {
            // 超时必须终止任务；丢弃 JoinHandle 会分离任务并保留 Hub/DB。
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
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
    #[tokio::test]
    async fn edge_pem_loading_is_bounded_and_explicit() {
        let dir = std::env::temp_dir().join(format!("rfrp-edge-pem-{}", crate::store::new_id()));
        tokio::fs::create_dir(&dir).await.unwrap();
        let cert = dir.join("cert.pem");
        let key = dir.join("key.pem");
        let mut cfg = Config {
            edge_tls_cert: Some(cert.clone()),
            edge_tls_key: Some(key.clone()),
            ..Default::default()
        };
        assert!(edge_identity(&cfg)
            .await
            .err()
            .unwrap()
            .contains("read edge TLS material"));
        tokio::fs::write(&cert, vec![0; 65537]).await.unwrap();
        assert!(edge_identity(&cfg).await.err().unwrap().contains("64 KiB"));
        tokio::fs::write(&cert, b"invalid").await.unwrap();
        tokio::fs::write(&key, b"invalid").await.unwrap();
        assert!(edge_identity(&cfg)
            .await
            .err()
            .unwrap()
            .contains("identity"));
        tokio::fs::write(
            &cert,
            include_bytes!("../../rustfrp-protocol/fixtures/edge-test-cert.pem"),
        )
        .await
        .unwrap();
        tokio::fs::write(
            &key,
            include_bytes!("../../rustfrp-protocol/fixtures/edge-test-key.pem"),
        )
        .await
        .unwrap();
        let identity = edge_identity(&cfg).await.unwrap().unwrap();
        assert_eq!(identity.certs.len(), 1);
        assert!(rustfrp_core::tls::server_config(identity).is_ok());
        cfg.edge_tls_key = None;
        assert!(edge_identity(&cfg)
            .await
            .err()
            .unwrap()
            .contains("together"));
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }
    #[tokio::test]
    async fn self_signed_identity_survives_restarts_and_rejects_corruption() {
        let parent = std::env::temp_dir().join(format!("rfrp-stable-{}", crate::store::new_id()));
        tokio::fs::create_dir(&parent).await.unwrap();
        let cfg = Config {
            db_path: parent.join("state.db"),
            public_host: "localhost".into(),
            ..Default::default()
        };
        let first = persistent_identity(&cfg).await.unwrap();
        let second = persistent_identity(&cfg).await.unwrap();
        assert_eq!(first.certs, second.certs);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(parent.join("state.edge-tls/key.pem"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        tokio::fs::write(parent.join("state.edge-tls/key.pem"), "corrupt")
            .await
            .unwrap();
        assert!(persistent_identity(&cfg).await.is_err());
        tokio::fs::remove_dir_all(parent).await.unwrap();
        assert!(persistent_identity(&cfg).await.is_err());
    }
}
