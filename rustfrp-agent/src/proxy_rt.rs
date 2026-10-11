//! Edge 侧的代理运行时：虚拟主机路由（HTTP Host / HTTPS SNI）与 UDP 转发。

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustfrp_core::error::TunnelError;
use rustfrp_core::http1::{
    ct_eq, location_matches, parse_request, rewrite_request, valid_header, ChunkedScanner, Framing,
    RequestHead, ResponseTracker,
};
use rustfrp_core::mux::Session;
use rustfrp_core::pipe;
use rustfrp_core::udpframe::{read_datagram, write_datagram};
use rustfrp_core::vhost::{domain_matches, tls_sni, Sni, MAX_HEAD};
use rustfrp_core::wire::{write_frame, HttpOpts, StartWork};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
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
    /// Edge 实时计数。
    pub traffic: Arc<pipe::Traffic>,
    /// http 路由选项（路径 / 认证 / 请求头）。
    pub http: Arc<HttpRoute>,
    /// 服务端限速；`None` = 不限。
    pub limiter: Option<Arc<pipe::Limiter>>,
}

/// http 代理在 Edge 上生效的选项（已校验、已规范化）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRoute {
    /// 路径前缀，至少一个（默认 `/`）。
    pub locations: Vec<String>,
    /// Basic 认证用户名 / 密码；都为空表示不认证。
    pub user: String,
    /// Basic 认证密码。
    pub password: String,
    /// 按请求里的 Basic 用户名选路。
    pub route_by_user: String,
    /// 转发前设置的请求头。
    pub set_headers: Vec<(String, String)>,
    /// 转发给访问者前设置的响应头。
    pub response_headers: Vec<(String, String)>,
}

impl Default for HttpRoute {
    fn default() -> Self {
        Self {
            locations: vec!["/".into()],
            user: String::new(),
            password: String::new(),
            route_by_user: String::new(),
            set_headers: Vec::new(),
            response_headers: Vec::new(),
        }
    }
}

impl HttpRoute {
    /// 校验并规范化 http 选项；不合法返回原因（注册时拒绝，而不是运行时悄悄丢弃）。
    pub fn from_opts(o: &HttpOpts) -> Result<Self, String> {
        let mut locations = Vec::new();
        for l in &o.locations {
            if !l.starts_with('/') || l.chars().any(|c| c.is_control() || c == ' ') {
                return Err(format!("invalid location \"{l}\" (must start with /)"));
            }
            if !locations.contains(l) {
                locations.push(l.clone());
            }
        }
        if locations.is_empty() {
            locations.push("/".into());
        }
        let mut set_headers = Vec::new();
        for (k, v) in &o.request_headers {
            if !valid_header(k, v) {
                return Err(format!("invalid request header \"{k}\""));
            }
            set_headers.push((k.clone(), v.clone()));
        }
        let mut response_headers = Vec::new();
        for (k, v) in &o.response_headers {
            // 改写分帧 / 连接管理头会让代理无法再判断响应边界。
            let framing = [
                "content-length",
                "transfer-encoding",
                "connection",
                "upgrade",
            ]
            .iter()
            .any(|f| k.eq_ignore_ascii_case(f));
            if framing || !valid_header(k, v) {
                return Err(format!("invalid response header \"{k}\""));
            }
            response_headers.push((k.clone(), v.clone()));
        }
        Ok(Self {
            locations,
            user: o.http_user.clone(),
            password: o.http_password.clone(),
            route_by_user: o.route_by_http_user.clone(),
            set_headers,
            response_headers,
        })
    }
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
    /// 同一域名可被多个代理共享，只要它们的路径前缀不重叠，或按 `routeByHTTPUser` 区分。
    pub fn add(&self, https: bool, hosts: &[String], route: &Route) -> Result<u64, String> {
        let mut t = self
            .table(https)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for h in hosts {
            if t.iter().any(|(p, r)| {
                p.eq_ignore_ascii_case(h)
                    && r.http.route_by_user == route.http.route_by_user
                    && r.http
                        .locations
                        .iter()
                        .any(|l| route.http.locations.contains(l))
            }) {
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

    /// 同步移除某个代理的全部路由（重启代理时必须先于重新登记，否则会与自己的旧路由冲突）。
    pub fn remove_proxy(&self, proxy: &str) {
        for https in [false, true] {
            self.table(https)
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|(_, r)| r.proxy != proxy);
        }
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

impl Routes {
    /// HTTP 选路（frp 语义）：先精确域名，再由具体到宽泛的通配域名；
    /// 同一域名内取匹配路径前缀最长者，优先 `routeByHTTPUser` 与请求 Basic 用户名一致的，其次未限定用户的。
    pub fn lookup_http(&self, host: &str, path: &str, user: &str) -> Option<Route> {
        let t = self
            .table(false)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut tiers: Vec<&str> = Vec::new();
        for (p, _) in t.iter() {
            if (p.eq_ignore_ascii_case(host) || (p.starts_with("*.") && domain_matches(p, host)))
                && !tiers.iter().any(|x| x.eq_ignore_ascii_case(p))
            {
                tiers.push(p);
            }
        }
        // 精确域名在前，其余按模式长度（越具体越前）。
        tiers.sort_by_key(|p| (!p.eq_ignore_ascii_case(host), std::cmp::Reverse(p.len())));
        for tier in tiers {
            let users: &[&str] = if user.is_empty() { &[""] } else { &[user, ""] };
            for want_user in users.iter().copied() {
                let best = t
                    .iter()
                    .filter(|(p, r)| {
                        p.eq_ignore_ascii_case(tier) && r.http.route_by_user == want_user
                    })
                    .filter_map(|(_, r)| {
                        r.http
                            .locations
                            .iter()
                            .filter(|l| location_matches(path, l))
                            .map(String::len)
                            .max()
                            .map(|len| (len, r))
                    })
                    .max_by_key(|(len, _)| *len);
                if let Some((_, r)) = best {
                    return Some(r.clone());
                }
            }
        }
        None
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
                    let result = if https {
                        https_conn(tcp, peer, &routes, &opts).await
                    } else {
                        http_conn(tcp, peer, &routes, &opts).await
                    };
                    if let Err(e) = result {
                        log.debug(&format!("vhost {peer}: {e}"));
                    }
                });
            }
        }
    }
}

/// 读到足以判定 SNI 为止，返回（已读字节, 主机名）。
async fn read_sni(tcp: &mut TcpStream) -> Result<(Vec<u8>, Option<String>), TunnelError> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        let n = tcp.read(&mut chunk).await?;
        if n == 0 {
            return Ok((buf, None));
        }
        buf.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
        match tls_sni(&buf) {
            Sni::Found(h) => return Ok((buf, Some(h))),
            Sni::None => return Ok((buf, None)),
            Sni::Incomplete => {}
        }
        if buf.len() >= MAX_HEAD {
            return Ok((buf, None));
        }
    }
}

/// HTTPS：按 SNI 选路后原样透传（不解密）。
async fn https_conn(
    mut tcp: TcpStream,
    peer: SocketAddr,
    routes: &Routes,
    opts: &VhostOpts,
) -> Result<(), TunnelError> {
    let _ = tcp.set_nodelay(true);
    let (head, host) = timeout(opts.head_timeout, read_sni(&mut tcp))
        .await
        .map_err(|_| TunnelError::Timeout("read request head"))??;
    let Some(route) = host.and_then(|h| routes.lookup(true, &h)) else {
        return Ok(());
    };
    let dst = tcp.local_addr().map(|a| a.to_string()).unwrap_or_default();
    let mut stream = open_upstream(&route, peer, &dst, opts.open_timeout).await?;
    match &route.limiter {
        Some(l) => {
            let mut v = pipe::Limited::new(tcp, l.clone());
            let mut w = pipe::Limited::new(stream, l.clone());
            pipe::relay_counted_prefix(&mut v, &mut w, &route.traffic, &head).await?;
        }
        None => {
            pipe::relay_counted_prefix(&mut tcp, &mut stream, &route.traffic, &head).await?;
        }
    }
    Ok(())
}

/// 向后端客户端开一条流并发出 `StartWork`。
async fn open_upstream(
    route: &Route,
    peer: SocketAddr,
    dst: &str,
    open_timeout: Duration,
) -> Result<rustfrp_core::mux::Stream, TunnelError> {
    let mut stream = timeout(open_timeout, route.session.open())
        .await
        .map_err(|_| TunnelError::Timeout("open work stream"))??;
    write_frame(
        &mut stream,
        &StartWork {
            proxy: route.proxy.clone(),
            src: peer.to_string(),
            dst: dst.to_string(),
        },
    )
    .await?;
    Ok(stream)
}

const UNAUTHORIZED: &[u8] = b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"Restricted\"\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 12\r\nConnection: close\r\n\r\nUnauthorized";
/// 单个访问者连接最多同时连着的后端流数（不同路由各占一条）。
const MAX_UPSTREAMS: usize = 8;

enum Reject {
    BadRequest,
    NotFound,
    Unauthorized,
}

/// 选路并校验 Basic 认证。
fn resolve(routes: &Routes, head: &RequestHead) -> Result<Route, Reject> {
    let host = head.host.as_deref().ok_or(Reject::BadRequest)?;
    let user = head.basic.as_ref().map_or("", |(u, _)| u.as_str());
    let route = routes
        .lookup_http(host, &head.path, user)
        .ok_or(Reject::NotFound)?;
    let h = &route.http;
    if !h.user.is_empty() || !h.password.is_empty() {
        let ok = head
            .basic
            .as_ref()
            .is_some_and(|(u, p)| ct_eq(u, &h.user) & ct_eq(p, &h.password));
        if !ok {
            return Err(Reject::Unauthorized);
        }
    }
    Ok(route)
}

type ClientWriter = Arc<tokio::sync::Mutex<OwnedWriteHalf>>;

/// 一条后端流上的响应进度：请求侧登记、响应泵侧推进，变化通过 `watch` 通知（未完成数, 是否无法判定）。
struct Tracked {
    inner: std::sync::Mutex<ResponseTracker>,
    tx: tokio::sync::watch::Sender<(usize, bool)>,
}

impl Tracked {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(ResponseTracker::new()),
            tx: tokio::sync::watch::channel((0, false)).0,
        })
    }
    fn pending(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending()
    }
    fn closing(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closing()
    }
    fn update(&self, f: impl FnOnce(&mut ResponseTracker)) {
        let mut t = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut t);
        self.tx.send_replace((t.pending(), t.is_opaque()));
    }
    /// 等到这条流上已发出的请求都收到完整响应（流结束时登记被清空）；
    /// 无法判定边界、超时或访问者连接已结束返回 `false`。
    async fn settled(&self, done: &CancellationToken) -> bool {
        let mut rx = self.tx.subscribe();
        let wait = async {
            loop {
                let (pending, opaque) = *rx.borrow_and_update();
                if pending == 0 {
                    return true;
                }
                if opaque {
                    return false;
                }
                tokio::select! {
                    _ = rx.changed() => {}
                    _ = done.cancelled() => return false,
                }
            }
        };
        timeout(Duration::from_secs(30), wait)
            .await
            .unwrap_or(false)
    }
}

/// 一条通往某个代理客户端的后端流（写半部）；读半部由响应泵处理。
struct Upstream {
    limiter: Option<Arc<pipe::Limiter>>,
    route_id: u64,
    uid: u64,
    write: tokio::io::WriteHalf<rustfrp_core::mux::Stream>,
    alive: Arc<std::sync::atomic::AtomicBool>,
    traffic: Arc<pipe::Traffic>,
    tracked: Arc<Tracked>,
}

/// 响应泵共享的连接级状态。
struct PumpCtx {
    client: ClientWriter,
    traffic: Arc<pipe::Traffic>,
    alive: Arc<std::sync::atomic::AtomicBool>,
    tracked: Arc<Tracked>,
    /// 转发给访问者前设置的响应头。
    response_headers: Arc<HttpRoute>,
    /// 本后端流的编号。
    limiter: Option<Arc<pipe::Limiter>>,
    uid: u64,
    /// 访问者最近一个请求所用后端流的编号。
    current: Arc<std::sync::atomic::AtomicU64>,
    done: CancellationToken,
}

/// 响应泵：后端 → 访问者。当前使用中的后端关闭时，关闭访问者连接（让「以关闭为界」的响应正常结束）。
async fn pump_response(mut read: tokio::io::ReadHalf<rustfrp_core::mux::Stream>, c: PumpCtx) {
    use std::sync::atomic::Ordering::Relaxed;
    let _connection = c.traffic.connection();
    let mut buf = [0u8; pipe::BUF_SIZE];
    loop {
        let n = match read.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let chunk = buf.get(..n).unwrap_or(&[]);
        let mut w = c.client.lock().await;
        // 要改写响应头时，先经跟踪器得到实际输出（头未收齐则为空），再写给访问者。
        let rewritten = (!c.response_headers.response_headers.is_empty()).then(|| {
            let mut out = Vec::with_capacity(chunk.len() + 64);
            c.tracked
                .update(|t| t.rewrite(chunk, &c.response_headers.response_headers, &mut out));
            out
        });
        let wrote = match &rewritten {
            Some(out) => w.write_all(out).await,
            None => w.write_all(chunk).await,
        };
        if wrote.is_err() {
            c.tracked.update(ResponseTracker::close);
            c.done.cancel();
            return;
        }
        c.traffic
            .record_out(rewritten.as_ref().map_or(n, Vec::len) as u64);
        drop(w);
        if let Some(l) = &c.limiter {
            l.pace(n).await;
        }
        if rewritten.is_none() {
            c.tracked.update(|t| t.feed(chunk));
        }
    }
    c.alive.store(false, Relaxed);
    // 后端在响应完整送达之后才关闭（保持连接被后端回收）：访问者连接不受影响；
    // 否则（响应以关闭为界、后端声明 `Connection: close`，或中途断开）必须关闭访问者连接，让对端看到结束。
    let must_close = c.tracked.pending() > 0 || c.tracked.closing();
    c.tracked.update(ResponseTracker::close);
    if must_close && c.current.load(Relaxed) == c.uid {
        let _ = c.client.lock().await.shutdown().await;
        c.done.cancel();
    }
}

enum HeadRead {
    Eof,
    Head(RequestHead),
    Bad,
}

/// 读取下一个请求头。第一个请求从连接建立起计时；之后的请求从收到第一个字节起计时（保持连接可以空闲）。
async fn read_request_head(
    tr: &mut OwnedReadHalf,
    buf: &mut Vec<u8>,
    first: bool,
    head_timeout: Duration,
    done: &CancellationToken,
) -> Result<HeadRead, TunnelError> {
    let mut deadline = first.then(|| tokio::time::Instant::now() + head_timeout);
    let mut tmp = [0u8; 4096];
    loop {
        match parse_request(buf) {
            Ok(Some(h)) => return Ok(HeadRead::Head(h)),
            Err(_) => return Ok(HeadRead::Bad),
            Ok(None) => {}
        }
        if buf.len() >= MAX_HEAD {
            return Ok(HeadRead::Bad);
        }
        let read = async {
            tokio::select! {
                r = tr.read(&mut tmp) => r.map(Some),
                _ = done.cancelled() => Ok(None),
            }
        };
        let got = match deadline {
            Some(d) => tokio::time::timeout_at(d, read)
                .await
                .map_err(|_| TunnelError::Timeout("read request head"))??,
            None => read.await?,
        };
        let Some(n) = got else {
            return Ok(HeadRead::Eof);
        };
        if n == 0 {
            return if buf.is_empty() {
                Ok(HeadRead::Eof)
            } else {
                Err(TunnelError::Wire(
                    "connection closed inside request head".into(),
                ))
            };
        }
        if deadline.is_none() {
            deadline = Some(tokio::time::Instant::now() + head_timeout);
        }
        buf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
    }
}

/// 把 `buf` 里和后续读到的字节按分帧方式转发到后端，正文不缓冲。
async fn forward_body(
    tr: &mut OwnedReadHalf,
    up: &mut Upstream,
    buf: &mut Vec<u8>,
    framing: Framing,
    done: &CancellationToken,
) -> Result<(), TunnelError> {
    let mut scanner = ChunkedScanner::new();
    let mut left = match framing {
        Framing::None => return Ok(()),
        Framing::Length(n) => n,
        Framing::Chunked => 0,
    };
    loop {
        if buf.is_empty() {
            let mut tmp = [0u8; pipe::BUF_SIZE];
            let n = tokio::select! {
                r = tr.read(&mut tmp) => r?,
                _ = done.cancelled() => return Err(TunnelError::Closed),
            };
            if n == 0 {
                return Err(TunnelError::Wire(
                    "connection closed inside request body".into(),
                ));
            }
            buf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
            if let Some(l) = &up.limiter {
                l.pace(n).await;
            }
        }
        let (take, finished) = match framing {
            Framing::Chunked => scanner
                .feed(buf)
                .map_err(|e| TunnelError::Wire(format!("{e:?}")))?,
            _ => {
                let take = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
                left -= take as u64;
                (take, left == 0)
            }
        };
        up.write.write_all(buf.get(..take).unwrap_or(&[])).await?;
        up.traffic.record_in(take as u64);
        buf.drain(..take);
        if finished {
            return Ok(());
        }
    }
}

/// 协议升级（WebSocket 等）之后：原样转发访问者的字节。
async fn forward_raw(
    tr: &mut OwnedReadHalf,
    up: &mut Upstream,
    buf: &mut Vec<u8>,
    done: &CancellationToken,
) -> Result<(), TunnelError> {
    let mut tmp = [0u8; pipe::BUF_SIZE];
    loop {
        if !buf.is_empty() {
            up.write.write_all(buf).await?;
            up.traffic.record_in(buf.len() as u64);
            buf.clear();
        }
        let n = tokio::select! {
            r = tr.read(&mut tmp) => r?,
            _ = done.cancelled() => return Ok(()),
        };
        if n == 0 {
            return Ok(());
        }
        if let Some(l) = &up.limiter {
            l.pace(n).await;
        }
        buf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
    }
}

async fn reply(client: &ClientWriter, bytes: &[u8]) {
    let mut w = client.lock().await;
    let _ = w.write_all(bytes).await;
    let _ = w.shutdown().await;
}

/// HTTP：逐请求选路、认证、改写请求头，并按分帧转发正文。
async fn http_conn(
    tcp: TcpStream,
    peer: SocketAddr,
    routes: &Routes,
    opts: &VhostOpts,
) -> Result<(), TunnelError> {
    use std::sync::atomic::Ordering::Relaxed;
    let _ = tcp.set_nodelay(true);
    let dst = tcp.local_addr().map(|a| a.to_string()).unwrap_or_default();
    let (mut tr, tw) = tcp.into_split();
    let client: ClientWriter = Arc::new(tokio::sync::Mutex::new(tw));
    let done = CancellationToken::new();
    let current = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut pumps = tokio::task::JoinSet::new();
    let mut ups: HashMap<u64, Upstream> = HashMap::new();
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut next_uid = 1u64;
    let mut first = true;

    // `true` = 访问者正常结束（等待响应送完）；`false` = 立即中止。
    let drain = 'conn: loop {
        let head = match read_request_head(&mut tr, &mut buf, first, opts.head_timeout, &done).await
        {
            Ok(HeadRead::Head(h)) => h,
            Ok(HeadRead::Eof) => break 'conn true,
            Ok(HeadRead::Bad) => {
                if first {
                    reply(&client, BAD_REQUEST).await;
                }
                break 'conn false;
            }
            Err(e) => {
                abort_all(&mut pumps, &mut ups).await;
                return Err(e);
            }
        };
        let route = match resolve(routes, &head) {
            Ok(r) => r,
            Err(rej) => {
                if first {
                    reply(
                        &client,
                        match rej {
                            Reject::BadRequest => BAD_REQUEST,
                            Reject::NotFound => NOT_FOUND,
                            Reject::Unauthorized => UNAUTHORIZED,
                        },
                    )
                    .await;
                }
                break 'conn false;
            }
        };
        ups.retain(|_, u| u.alive.load(Relaxed));
        // 换了后端：先等上一条后端把已发出请求的响应送完，保证响应顺序（流水线请求不会串位）。
        let prev = current.load(Relaxed);
        if let Some(p) = ups
            .values()
            .find(|u| u.uid == prev && u.route_id != route.id)
        {
            if !p.tracked.settled(&done).await {
                break 'conn false;
            }
        }
        let full = ups.len() >= MAX_UPSTREAMS;
        let up = match ups.entry(route.id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(_) if full => break 'conn false,
            Entry::Vacant(v) => {
                let Ok(stream) = open_upstream(&route, peer, &dst, opts.open_timeout).await else {
                    break 'conn false;
                };
                let (read, write) = tokio::io::split(stream);
                let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let tracked = Tracked::new();
                let uid = next_uid;
                next_uid += 1;
                pumps.spawn(pump_response(
                    read,
                    PumpCtx {
                        client: client.clone(),
                        traffic: route.traffic.clone(),
                        alive: alive.clone(),
                        tracked: tracked.clone(),
                        response_headers: route.http.clone(),
                        limiter: route.limiter.clone(),
                        uid,
                        current: current.clone(),
                        done: done.clone(),
                    },
                ));
                v.insert(Upstream {
                    limiter: route.limiter.clone(),
                    route_id: route.id,
                    uid,
                    write,
                    alive,
                    traffic: route.traffic.clone(),
                    tracked,
                })
            }
        };
        current.store(up.uid, Relaxed);
        up.tracked
            .update(|t| t.on_request(head.method.eq_ignore_ascii_case("HEAD")));

        let raw = buf.get(..head.head_len).unwrap_or(&[]);
        let out = if route.rewrite.is_empty() && route.http.set_headers.is_empty() {
            raw.to_vec()
        } else {
            rewrite_request(raw, &route.rewrite, &route.http.set_headers)
        };
        if up.write.write_all(&out).await.is_err() {
            break 'conn false;
        }
        up.traffic.record_in(out.len() as u64);
        buf.drain(..head.head_len);
        first = false;
        if head.upgrade {
            break 'conn forward_raw(&mut tr, up, &mut buf, &done).await.is_ok();
        }
        if forward_body(&mut tr, up, &mut buf, head.framing, &done)
            .await
            .is_err()
        {
            break 'conn false;
        }
    };

    if drain {
        // 访问者不再发请求：告知后端，并等响应送完（或连接关闭）。
        for u in ups.values_mut() {
            let _ = u.write.shutdown().await;
        }
        tokio::select! {
            _ = async { while pumps.join_next().await.is_some() {} } => {}
            _ = done.cancelled() => {}
        }
    }
    abort_all(&mut pumps, &mut ups).await;
    let _ = client.lock().await.shutdown().await;
    Ok(())
}

async fn abort_all(pumps: &mut tokio::task::JoinSet<()>, ups: &mut HashMap<u64, Upstream>) {
    pumps.abort_all();
    ups.clear();
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
    /// Edge 实时计数。
    pub traffic: Arc<pipe::Traffic>,
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
            dst: String::new(),
        },
    )
    .await?;
    let _connection = opts.traffic.connection();
    let traffic = opts.traffic.clone();
    let (mut rd, mut wr) = tokio::io::split(stream);
    let back = sock.clone();
    let mut reader = tokio::spawn(async move {
        let mut buf = Vec::new();
        while let Ok(Some(_)) = read_datagram(&mut rd, &mut buf).await {
            if let Ok(n) = back.send_to(&buf, peer).await {
                traffic.record_out(n as u64);
            }
        }
    });
    let result = loop {
        tokio::select! {
            _ = cancel.cancelled() => break Ok(()),
            _ = tokio::time::sleep(opts.idle) => break Ok(()),
            _ = &mut reader => break Ok(()),
            data = rx.recv() => match data {
                Some(d) => {
                    if let Err(e) = write_datagram(&mut wr, &d).await { break Err(e); }
                    opts.traffic.record_in(d.len() as u64);
                },
                None => break Ok(()),
            },
        }
    };
    reader.abort();
    result
}
