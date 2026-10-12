//! 访问者（frp 的 `[[visitors]]`，stcp / sudp）：在本机监听，把连接经 Edge 送到对端的同名代理。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rustfrp_core::error::TunnelError;
use rustfrp_core::mux::Session;
use rustfrp_core::pipe::{self, Traffic};
use rustfrp_core::wire::VisitorSpec;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::Event;
use crate::log::Logger;
use crate::proxy_rt::{udp_proxy, visitor_handshake, UdpOpts, VisitorTarget};

/// 访问者运行所需的客户端参数。
#[derive(Clone)]
pub struct VisitorEnv {
    /// 本客户端的用户名（用于拼目标代理的完整名）。
    pub user: String,
    /// 握手 / 开流超时。
    pub open_timeout: Duration,
    /// UDP 参数。
    pub udp_packet_size: usize,
    /// UDP 会话空闲回收时间。
    pub udp_idle: Duration,
    /// 实时计数。
    pub traffic: Arc<Traffic>,
    /// 日志。
    pub log: Logger,
    /// 事件。
    pub events: Option<mpsc::Sender<Event>>,
}

/// 目标代理的完整名：显式 `serverUser` 优先，否则沿用本客户端的用户。
pub fn full_name(spec: &VisitorSpec, own_user: &str) -> String {
    let user = if spec.server_user.is_empty() {
        own_user
    } else {
        &spec.server_user
    };
    if user.is_empty() {
        spec.server_name.clone()
    } else {
        format!("{user}.{}", spec.server_name)
    }
}

/// 一个会话里的全部访问者；按规格增删。
pub struct Visitors {
    running: HashMap<String, (VisitorSpec, CancellationToken)>,
    parent: CancellationToken,
    session: Session,
    env: VisitorEnv,
}

impl Visitors {
    /// 新建（`parent` 取消时全部停止）。
    pub fn new(session: Session, env: VisitorEnv, parent: CancellationToken) -> Self {
        Self {
            running: HashMap::new(),
            parent,
            session,
            env,
        }
    }

    /// 让运行中的访问者与期望一致。
    pub fn sync(&mut self, wanted: &[VisitorSpec]) {
        self.running.retain(|name, (spec, stop)| {
            let keep = wanted.contains(spec);
            if !keep {
                stop.cancel();
                self.env.log.info(&format!("visitor [{name}] stopped"));
            }
            keep
        });
        for spec in wanted {
            if self.running.contains_key(&spec.name) {
                continue;
            }
            let stop = self.parent.child_token();
            tokio::spawn(run(
                spec.clone(),
                self.session.clone(),
                self.env.clone(),
                stop.clone(),
            ));
            self.running.insert(spec.name.clone(), (spec.clone(), stop));
        }
    }
}

fn emit(env: &VisitorEnv, e: Event) {
    if let Some(tx) = &env.events {
        let _ = tx.try_send(e);
    }
}

async fn run(spec: VisitorSpec, session: Session, env: VisitorEnv, stop: CancellationToken) {
    let addr = if spec.bind_addr.is_empty() {
        "127.0.0.1"
    } else {
        spec.bind_addr.as_str()
    };
    let target = VisitorTarget {
        server_name: full_name(&spec, &env.user),
        sk: spec.sk.clone(),
        use_encryption: spec.use_encryption,
        use_compression: spec.use_compression,
    };
    let fail = |error: String| {
        env.log
            .error(&format!("visitor [{}] start error: {error}", spec.name));
        emit(
            &env,
            Event::VisitorFailed {
                name: spec.name.clone(),
                error,
            },
        );
    };
    match spec.kind.as_str() {
        "stcp" => {
            let listener = match bind_tcp(addr, spec.bind_port, &stop).await {
                Ok(Some(l)) => l,
                Ok(None) => return,
                Err(e) => return fail(e.to_string()),
            };
            let port = listener.local_addr().map_or(spec.bind_port, |a| a.port());
            env.log.info(&format!(
                "visitor [{}] (stcp) listening on {addr}:{port}",
                spec.name
            ));
            emit(
                &env,
                Event::VisitorStarted {
                    name: spec.name.clone(),
                    port,
                },
            );
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    accepted = listener.accept() => {
                        let Ok((conn, peer)) = accepted else { continue };
                        let (session, target, env) = (session.clone(), target.clone(), env.clone());
                        let name = spec.name.clone();
                        tokio::spawn(async move {
                            if let Err(e) = stcp_conn(conn, &session, &target, &env).await {
                                env.log.debug(&format!("visitor [{name}] {peer}: {e}"));
                            }
                        });
                    }
                }
            }
        }
        _ => {
            let sock = match UdpSocket::bind((addr, spec.bind_port)).await {
                Ok(s) => s,
                Err(e) => return fail(e.to_string()),
            };
            let port = sock.local_addr().map_or(spec.bind_port, |a| a.port());
            env.log.info(&format!(
                "visitor [{}] (sudp) listening on {addr}:{port}",
                spec.name
            ));
            emit(
                &env,
                Event::VisitorStarted {
                    name: spec.name.clone(),
                    port,
                },
            );
            let opts = UdpOpts {
                max_peers: 256,
                idle: env.udp_idle,
                packet_size: env.udp_packet_size,
                open_timeout: env.open_timeout,
                traffic: env.traffic.clone(),
                visitor: Some(target),
                limits: crate::edge::ConnectionLimits::unlimited(),
            };
            udp_proxy(
                sock,
                spec.name.clone(),
                session,
                opts,
                env.log.clone(),
                stop,
            )
            .await;
        }
    }
}

/// 绑定 TCP 端口；旧实例刚停止时地址可能还没释放，短暂重试。
async fn bind_tcp(
    addr: &str,
    port: u16,
    stop: &CancellationToken,
) -> std::io::Result<Option<TcpListener>> {
    let mut attempts = 0;
    loop {
        match TcpListener::bind((addr, port)).await {
            Ok(l) => return Ok(Some(l)),
            Err(e) if attempts >= 20 => return Err(e),
            Err(_) => attempts += 1,
        }
        tokio::select! {
            _ = stop.cancelled() => return Ok(None),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

async fn stcp_conn(
    mut conn: tokio::net::TcpStream,
    session: &Session,
    target: &VisitorTarget,
    env: &VisitorEnv,
) -> Result<(), TunnelError> {
    let _ = conn.set_nodelay(true);
    let mut stream = tokio::time::timeout(env.open_timeout, session.open())
        .await
        .map_err(|_| TunnelError::Timeout("open visitor stream"))??;
    let transform = visitor_handshake(&mut stream, target, env.open_timeout).await?;
    let mut stream = rustfrp_core::transform::wrap(
        Box::new(stream),
        transform.use_encryption,
        transform.use_compression,
        &transform.encryption_key,
        true,
    )
    .map_err(|e| TunnelError::Wire(e.to_string()))?;
    pipe::relay_counted(&mut conn, &mut stream, &env.traffic).await?;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    fn env(tx: mpsc::Sender<Event>) -> VisitorEnv {
        VisitorEnv {
            user: "me".into(),
            open_timeout: Duration::from_millis(200),
            udp_packet_size: 1500,
            udp_idle: Duration::from_secs(1),
            traffic: Arc::default(),
            log: Logger::silent(),
            events: Some(tx),
        }
    }

    fn spec(name: &str, kind: &str, port: u16) -> VisitorSpec {
        VisitorSpec {
            name: name.into(),
            kind: kind.into(),
            server_name: "target".into(),
            sk: "k".into(),
            bind_port: port,
            ..Default::default()
        }
    }

    #[test]
    fn full_names_follow_the_user_rules() {
        let mut v = spec("v", "stcp", 0);
        assert_eq!(full_name(&v, ""), "target");
        assert_eq!(full_name(&v, "me"), "me.target");
        v.server_user = "alice".into();
        assert_eq!(full_name(&v, "me"), "alice.target");
        assert_eq!(full_name(&v, ""), "alice.target");
    }

    #[tokio::test]
    async fn sync_starts_replaces_and_stops_visitors_and_reports_bind_failures() {
        let (a, _b) = tokio::io::duplex(1024);
        let session = rustfrp_core::mux::spawn(a, true, Default::default()).0;
        let (tx, mut rx) = mpsc::channel(16);
        let parent = CancellationToken::new();
        let mut visitors = Visitors::new(session, env(tx), parent.clone());
        // 空监听地址落到 127.0.0.1；已存在且未变化的访问者不会被重启。
        let tcp = spec("t", "stcp", 0);
        let udp = spec("u", "sudp", 0);
        visitors.sync(&[tcp.clone(), udp.clone()]);
        visitors.sync(&[tcp.clone(), udp.clone()]);
        let mut ports = std::collections::HashMap::new();
        for _ in 0..2 {
            let Some(Event::VisitorStarted { name, port }) = rx.recv().await else {
                panic!("expected a started event")
            };
            ports.insert(name, port);
        }
        assert!(ports["t"] != 0 && ports["u"] != 0);
        assert!(rx.try_recv().is_err(), "unchanged visitors keep running");
        // 只留 sudp：stcp 的监听随之关闭。
        visitors.sync(std::slice::from_ref(&udp));
        let tcp_port = ports["t"];
        let mut closed = false;
        for _ in 0..50 {
            if TcpListener::bind(("127.0.0.1", tcp_port)).await.is_ok() {
                closed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(closed, "stopped visitors release their port");
        // 端口被占用：stcp 重试后失败；sudp 直接失败。
        let held = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let held_port = held.local_addr().unwrap().port();
        let held_udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let held_udp_port = held_udp.local_addr().unwrap().port();
        visitors.sync(&[
            spec("busy-tcp", "stcp", held_port),
            spec("busy-udp", "sudp", held_udp_port),
        ]);
        let mut failed = std::collections::HashSet::new();
        while failed.len() < 2 {
            if let Some(Event::VisitorFailed { name, .. }) = rx.recv().await {
                failed.insert(name);
            }
        }
        // 重试期间被停止：安静退出，不报失败。
        visitors.sync(&[spec("again", "stcp", held_port)]);
        tokio::time::sleep(Duration::from_millis(100)).await;
        visitors.sync(&[]);
        tokio::time::sleep(Duration::from_millis(200)).await;
        parent.cancel();
        drop(held);
    }
}
