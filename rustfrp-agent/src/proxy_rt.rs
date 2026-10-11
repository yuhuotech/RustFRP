//! Edge 侧的代理运行时：虚拟主机路由（HTTP Host / HTTPS SNI）与 UDP 转发。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustfrp_core::error::TunnelError;
use rustfrp_core::mux::Session;
use rustfrp_core::pipe;
use rustfrp_core::udpframe::{read_datagram, write_datagram};
use rustfrp_core::vhost::{
    domain_matches, head_complete, http_host, rewrite_host, tls_sni, Sni, MAX_HEAD,
};
use rustfrp_core::wire::{write_frame, StartWork};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::log::Logger;

/// 一条虚拟主机路由。
#[derive(Clone)]
pub struct Route {
    /// 路由编号（每次登记唯一；清理时按编号删除，避免新旧同名代理互相误删）。
    pub id: u64,
    /// 代理名。
    pub proxy: String,
    /// 通往客户端的会话。
    pub session: Session,
    /// 非空时改写转发请求的 Host 头（仅 HTTP）。
    pub rewrite: String,
}

type Table = Mutex<Vec<(String, Route)>>;

/// HTTP / HTTPS 路由表：域名模式 → 路由。
#[derive(Default)]
pub struct Routes {
    http: Table,
    https: Table,
    next_id: std::sync::atomic::AtomicU64,
}

impl Routes {
    fn table(&self, https: bool) -> &Table {
        if https {
            &self.https
        } else {
            &self.http
        }
    }

    /// 为一个代理登记全部域名；任一域名已被占用则整体失败（不留下半条路由）。
    pub fn add(&self, https: bool, hosts: &[String], route: &Route) -> Result<u64, String> {
        let mut t = self
            .table(https)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for h in hosts {
            if t.iter().any(|(p, _)| p.eq_ignore_ascii_case(h)) {
                return Err(format!("custom domain [{h}] is already used"));
            }
        }
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        t.extend(hosts.iter().map(|h| {
            (
                h.to_ascii_lowercase(),
                Route {
                    id,
                    ..route.clone()
                },
            )
        }));
        Ok(id)
    }

    /// 移除一次登记产生的全部路由。
    pub fn remove(&self, id: u64) {
        for https in [false, true] {
            self.table(https)
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|(_, r)| r.id != id);
        }
    }

    /// 精确匹配优先，其次是最长的通配模式。
    pub fn lookup(&self, https: bool, host: &str) -> Option<Route> {
        let t = self
            .table(https)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        t.iter()
            .find(|(p, _)| p.eq_ignore_ascii_case(host))
            .or_else(|| {
                t.iter()
                    .filter(|(p, _)| p.starts_with("*.") && domain_matches(p, host))
                    .max_by_key(|(p, _)| p.len())
            })
            .map(|(_, r)| r.clone())
    }
}

/// 虚拟主机服务的参数。
#[derive(Clone)]
pub struct VhostOpts {
    /// 等待请求头 / ClientHello 的超时。
    pub head_timeout: Duration,
    /// 向客户端开流的超时。
    pub open_timeout: Duration,
}

const NOT_FOUND: &[u8] = b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 18\r\nConnection: close\r\n\r\nthe page not found";
const BAD_REQUEST: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// 接受虚拟主机连接，直到 `cancel`。
pub async fn serve_vhost(
    listener: TcpListener,
    https: bool,
    routes: Arc<Routes>,
    opts: VhostOpts,
    log: Logger,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((tcp, peer)) = accepted else { continue };
                let (routes, opts, log) = (routes.clone(), opts.clone(), log.clone());
                tokio::spawn(async move {
                    if let Err(e) = vhost_conn(tcp, peer, https, &routes, &opts).await {
                        log.debug(&format!("vhost {peer}: {e}"));
                    }
                });
            }
        }
    }
}

/// 读到足以判定主机名为止，返回（已读字节, 主机名）。
async fn read_head(
    tcp: &mut TcpStream,
    https: bool,
) -> Result<(Vec<u8>, Option<String>), TunnelError> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        let n = tcp.read(&mut chunk).await?;
        if n == 0 {
            return Ok((buf, None));
        }
        buf.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
        if https {
            match tls_sni(&buf) {
                Sni::Found(h) => return Ok((buf, Some(h))),
                Sni::None => return Ok((buf, None)),
                Sni::Incomplete => {}
            }
        } else if head_complete(&buf) {
            let host = http_host(&buf);
            return Ok((buf, host));
        }
        if buf.len() >= MAX_HEAD {
            return Ok((buf, None));
        }
    }
}

async fn vhost_conn(
    mut tcp: TcpStream,
    peer: SocketAddr,
    https: bool,
    routes: &Routes,
    opts: &VhostOpts,
) -> Result<(), TunnelError> {
    let _ = tcp.set_nodelay(true);
    let (head, host) = timeout(opts.head_timeout, read_head(&mut tcp, https))
        .await
        .map_err(|_| TunnelError::Timeout("read request head"))??;
    let Some(host) = host else {
        if !https {
            tcp.write_all(BAD_REQUEST).await?;
        }
        return Ok(());
    };
    let Some(route) = routes.lookup(https, &host) else {
        if !https {
            tcp.write_all(NOT_FOUND).await?;
        }
        return Ok(());
    };
    let mut stream = timeout(opts.open_timeout, route.session.open())
        .await
        .map_err(|_| TunnelError::Timeout("open work stream"))??;
    write_frame(
        &mut stream,
        &StartWork {
            proxy: route.proxy.clone(),
            src: peer.to_string(),
        },
    )
    .await?;
    let first = if !https && !route.rewrite.is_empty() {
        rewrite_host(&head, &route.rewrite)
    } else {
        head
    };
    stream.write_all(&first).await?;
    pipe::relay(&mut tcp, &mut stream).await?;
    Ok(())
}

/// UDP 代理的参数。
#[derive(Clone)]
pub struct UdpOpts {
    /// 同时跟踪的访问者数量上限（防止被海量源地址撑大内存）。
    pub max_peers: usize,
    /// 访问者空闲多久后回收。
    pub idle: Duration,
    /// 接收缓冲（frp 的 `udpPacketSize`）。
    pub packet_size: usize,
    /// 向客户端开流的超时。
    pub open_timeout: Duration,
}

type PeerMap = Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>>;

/// 在公网 UDP 端口上转发：每个访问者地址对应客户端那边的一条流。
pub async fn udp_proxy(
    sock: UdpSocket,
    name: String,
    session: Session,
    opts: UdpOpts,
    log: Logger,
    cancel: CancellationToken,
) {
    let sock = Arc::new(sock);
    let peers: PeerMap = Arc::default();
    let mut buf = vec![0u8; opts.packet_size.max(512)];
    loop {
        let (n, peer) = tokio::select! {
            _ = cancel.cancelled() => break,
            r = sock.recv_from(&mut buf) => match r { Ok(v) => v, Err(_) => continue },
        };
        let data = buf.get(..n).unwrap_or(&[]).to_vec();
        let existing = peers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .cloned();
        let tx = match existing {
            Some(tx) => tx,
            None => {
                let mut map = peers.lock().unwrap_or_else(PoisonError::into_inner);
                if map.len() >= opts.max_peers {
                    continue;
                }
                let (tx, rx) = mpsc::channel(64);
                map.insert(peer, tx.clone());
                drop(map);
                let (sock, peers, session, name, opts, log, cancel) = (
                    sock.clone(),
                    peers.clone(),
                    session.clone(),
                    name.clone(),
                    opts.clone(),
                    log.clone(),
                    cancel.clone(),
                );
                tokio::spawn(async move {
                    if let Err(e) = udp_peer(sock, peer, rx, &session, &name, &opts, &cancel).await
                    {
                        log.debug(&format!("udp proxy [{name}] peer {peer}: {e}"));
                    }
                    peers
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&peer);
                });
                tx
            }
        };
        // 队列满了就丢包——UDP 本来就允许丢包，且不能让慢客户端无限堆积内存。
        let _ = tx.try_send(data);
    }
}

async fn udp_peer(
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
    mut rx: mpsc::Receiver<Vec<u8>>,
    session: &Session,
    name: &str,
    opts: &UdpOpts,
    cancel: &CancellationToken,
) -> Result<(), TunnelError> {
    let mut stream = timeout(opts.open_timeout, session.open())
        .await
        .map_err(|_| TunnelError::Timeout("open work stream"))??;
    write_frame(
        &mut stream,
        &StartWork {
            proxy: name.to_string(),
            src: peer.to_string(),
        },
    )
    .await?;
    let (mut rd, mut wr) = tokio::io::split(stream);
    let back = sock.clone();
    let mut reader = tokio::spawn(async move {
        let mut buf = Vec::new();
        while let Ok(Some(_)) = read_datagram(&mut rd, &mut buf).await {
            let _ = back.send_to(&buf, peer).await;
        }
    });
    let result = loop {
        tokio::select! {
            _ = cancel.cancelled() => break Ok(()),
            _ = tokio::time::sleep(opts.idle) => break Ok(()),
            _ = &mut reader => break Ok(()),
            data = rx.recv() => match data {
                Some(d) => if let Err(e) = write_datagram(&mut wr, &d).await { break Err(e) },
                None => break Ok(()),
            },
        }
    };
    reader.abort();
    result
}
