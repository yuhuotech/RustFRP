//! 双向转发。
//!
//! 每个方向使用 8 KiB 缓冲（单连接合计 16 KiB），是「极致省内存」原则下的折中：
//! 缓冲随连接存在而存在，空闲连接约占 16 KiB + 任务开销。

use std::future::Future;

use tokio::io::{copy_bidirectional_with_sizes, AsyncRead, AsyncWrite};

/// 每方向缓冲大小。
pub const BUF_SIZE: usize = 8 * 1024;

/// 在 `a` 与 `b` 之间双向转发直到任一方关闭，返回 `(a→b 字节数, b→a 字节数)`。
pub async fn relay<A, B>(a: &mut A, b: &mut B) -> std::io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin + ?Sized,
    B: AsyncRead + AsyncWrite + Unpin + ?Sized,
{
    copy_bidirectional_with_sizes(a, b, BUF_SIZE, BUF_SIZE).await
}

/// 令牌桶限速器：一个代理的所有连接、两个方向共享同一个额度（与 frp 一致）。
///
/// 采用「先读后还债」的方式：每次读到 `n` 字节就扣 `n` 个令牌，余额为负时下一次读之前先等待，
/// 因此不需要额外缓冲，单次超出的量不超过一个读缓冲（8 KiB）。
#[derive(Debug)]
pub struct Limiter {
    rate: f64,
    burst: f64,
    state: std::sync::Mutex<(f64, std::time::Instant)>,
}

impl Limiter {
    /// 每秒 `bytes_per_sec` 字节；0 表示不限速（返回 `None`）。
    pub fn new(bytes_per_sec: u64) -> Option<std::sync::Arc<Limiter>> {
        if bytes_per_sec == 0 {
            return None;
        }
        let rate = bytes_per_sec as f64;
        // 突发额度 = 1 秒的量，至少容得下一个读缓冲。
        let burst = rate.max(BUF_SIZE as f64);
        Some(std::sync::Arc::new(Limiter {
            rate,
            burst,
            state: std::sync::Mutex::new((burst, std::time::Instant::now())),
        }))
    }

    /// 记账 `n` 字节，返回下一次读之前应等待多久。
    pub fn charge(&self, n: usize) -> std::time::Duration {
        let now = std::time::Instant::now();
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let elapsed = now.saturating_duration_since(st.1).as_secs_f64();
        st.0 = (st.0 + elapsed * self.rate).min(self.burst) - n as f64;
        st.1 = now;
        if st.0 >= 0.0 {
            std::time::Duration::ZERO
        } else {
            std::time::Duration::from_secs_f64(-st.0 / self.rate)
        }
    }

    /// 记账并等待（给手写读循环用）。
    pub async fn pace(&self, n: usize) {
        let wait = self.charge(n);
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

/// 读取限速包装：每次读到数据后向 [`Limiter`] 记账，必要时延后下一次读取。
pub struct Limited<T> {
    io: T,
    limiter: std::sync::Arc<Limiter>,
    sleep: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
    /// 写入也记账（给「自己产生响应数据」的本地插件用）。
    pace_writes: bool,
    write_sleep: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

impl<T> Limited<T> {
    /// 包装 `io`：只限读取方向。
    pub fn new(io: T, limiter: std::sync::Arc<Limiter>) -> Self {
        Self {
            io,
            limiter,
            sleep: None,
            pace_writes: false,
            write_sleep: None,
        }
    }

    /// 包装 `io`：读写两个方向共用同一个限速器。
    pub fn both(io: T, limiter: std::sync::Arc<Limiter>) -> Self {
        Self {
            pace_writes: true,
            ..Self::new(io, limiter)
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Limited<T> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if let Some(sleep) = self.sleep.as_mut() {
            if sleep.as_mut().poll(cx).is_pending() {
                return std::task::Poll::Pending;
            }
            self.sleep = None;
        }
        let before = buf.filled().len();
        let result = std::pin::Pin::new(&mut self.io).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = &result {
            let read = buf.filled().len() - before;
            let wait = self.limiter.charge(read);
            if !wait.is_zero() {
                self.sleep = Some(Box::pin(tokio::time::sleep(wait)));
            }
        }
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Limited<T> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if let Some(sleep) = self.write_sleep.as_mut() {
            if sleep.as_mut().poll(cx).is_pending() {
                return std::task::Poll::Pending;
            }
            self.write_sleep = None;
        }
        let result = std::pin::Pin::new(&mut self.io).poll_write(cx, buf);
        if let (true, std::task::Poll::Ready(Ok(n))) = (self.pace_writes, &result) {
            let wait = self.limiter.charge(*n);
            if !wait.is_zero() {
                self.write_sleep = Some(Box::pin(tokio::time::sleep(wait)));
            }
        }
        result
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// 每代理固定大小计数，不包含协议封装字节。
#[derive(Default, Debug)]
pub struct Traffic {
    bytes_in: std::sync::atomic::AtomicU64,
    bytes_out: std::sync::atomic::AtomicU64,
    connections: std::sync::atomic::AtomicU64,
    connections_total: std::sync::atomic::AtomicU64,
    parent: Option<std::sync::Arc<Traffic>>,
}

/// 实时计数快照；各字段独立采样。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TrafficSnapshot {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connections: u64,
    pub connections_total: u64,
}

impl Traffic {
    /// 建立代理计数，并将成功转发量同步累计到节点总量。
    pub fn child(parent: std::sync::Arc<Traffic>) -> Self {
        Self {
            parent: Some(parent),
            ..Self::default()
        }
    }
    /// 登记一个成功建立的转发会话。
    pub fn connection(&self) -> Connection<'_> {
        use std::sync::atomic::Ordering::Relaxed;
        let mut current = Some(self);
        while let Some(c) = current {
            c.connections.fetch_add(1, Relaxed);
            c.connections_total.fetch_add(1, Relaxed);
            current = c.parent.as_deref();
        }
        Connection(self)
    }

    /// 累计成功转发到服务端的数据报负载。
    pub fn record_in(&self, bytes: u64) {
        if let Some(parent) = &self.parent {
            parent.record_in(bytes);
        }
        self.bytes_in
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    /// 累计成功转发到访问者的数据报负载。
    pub fn record_out(&self, bytes: u64) {
        if let Some(parent) = &self.parent {
            parent.record_out(bytes);
        }
        self.bytes_out
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    /// 读取固定计数器，不分配内存。
    pub fn snapshot(&self) -> TrafficSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        TrafficSnapshot {
            bytes_in: self.bytes_in.load(Relaxed),
            bytes_out: self.bytes_out.load(Relaxed),
            connections: self.connections.load(Relaxed),
            connections_total: self.connections_total.load(Relaxed),
        }
    }
}

/// 活跃转发凭据，丢弃时归还连接计数。
pub struct Connection<'a>(&'a Traffic);
impl Drop for Connection<'_> {
    fn drop(&mut self) {
        let mut current = Some(self.0);
        while let Some(c) = current {
            c.connections
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            current = c.parent.as_deref();
        }
    }
}

struct Counted<'a, T: ?Sized> {
    io: &'a mut T,
    traffic: &'a Traffic,
    inbound: bool,
}

impl<T: AsyncRead + Unpin + ?Sized> AsyncRead for Counted<'_, T> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.io).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin + ?Sized> AsyncWrite for Counted<'_, T> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let result = std::pin::Pin::new(&mut *self.io).poll_write(cx, buf);
        if let std::task::Poll::Ready(Ok(n)) = result {
            if self.inbound {
                self.traffic.record_in(n as u64);
            } else {
                self.traffic.record_out(n as u64);
            }
        }
        result
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.io).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.io).poll_shutdown(cx)
    }
}

/// Edge 双向转发并即时累计成功写出字节。a 为访问者，b 为本机服务流。
/// 连接关闭、错误及 future 取消均通过栈上凭据归还连接计数。
pub async fn relay_counted<A, B>(
    a: &mut A,
    b: &mut B,
    traffic: &Traffic,
) -> std::io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin + ?Sized,
    B: AsyncRead + AsyncWrite + Unpin + ?Sized,
{
    relay_counted_prefix(a, b, traffic, &[]).await
}

/// 转发已读出的虚拟主机首部，然后继续实时计数转发。
pub async fn relay_counted_prefix<A, B>(
    a: &mut A,
    b: &mut B,
    traffic: &Traffic,
    prefix: &[u8],
) -> std::io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin + ?Sized,
    B: AsyncRead + AsyncWrite + Unpin + ?Sized,
{
    let _connection = traffic.connection();
    let mut a = Counted {
        io: a,
        traffic,
        inbound: false,
    };
    let mut b = Counted {
        io: b,
        traffic,
        inbound: true,
    };
    tokio::io::AsyncWriteExt::write_all(&mut b, prefix).await?;
    tokio::io::AsyncWriteExt::flush(&mut b).await?;
    relay(&mut a, &mut b).await
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

    #[test]
    fn limiter_accounts_for_bursts_and_debt() {
        assert!(Limiter::new(0).is_none());
        let l = Limiter::new(10_000).unwrap();
        // 突发额度内不等待；超出后按欠款 / 速率折算等待时间。
        assert!(l.charge(8_000).is_zero());
        let wait = l.charge(8_000);
        assert!(
            wait.as_secs_f64() > 0.5 && wait.as_secs_f64() < 0.7,
            "{wait:?}"
        );
        // 小速率也至少容得下一个读缓冲。
        let tiny = Limiter::new(100).unwrap();
        assert!(tiny.charge(BUF_SIZE).is_zero());
    }

    #[tokio::test]
    async fn both_directions_share_the_limiter_when_requested() {
        let limiter = Limiter::new(64 * 1024).unwrap();
        let (a, mut b) = duplex(1 << 20);
        let mut limited = Limited::both(a, limiter);
        let drain = tokio::spawn(async move {
            let mut total = 0;
            let mut buf = vec![0u8; 16 * 1024];
            while total < 128 * 1024 {
                total += b.read(&mut buf).await.unwrap();
            }
        });
        // 128 KiB 写入：前 64 KiB 是突发额度，其余约需 1 秒。
        let start = std::time::Instant::now();
        for _ in 0..(128 * 1024 / BUF_SIZE) {
            limited.write_all(&vec![1u8; BUF_SIZE]).await.unwrap();
        }
        drain.await.unwrap();
        assert!(
            start.elapsed() >= Duration::from_millis(700),
            "{:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn limited_reads_are_paced_and_writes_pass_through() {
        let limiter = Limiter::new(64 * 1024).unwrap();
        let (a, mut b) = duplex(1 << 20);
        let mut limited = Limited::new(a, limiter.clone());
        // 写不受限。
        limited.write_all(b"ping").await.unwrap();
        limited.flush().await.unwrap();
        let mut got = [0u8; 4];
        b.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
        // 读：前 64 KiB 是突发额度；再来 64 KiB 约需 1 秒。
        b.write_all(&vec![7u8; 128 * 1024]).await.unwrap();
        let start = std::time::Instant::now();
        // 与真实转发一致：按 8 KiB 的读缓冲分次读。
        let mut chunk = vec![0u8; BUF_SIZE];
        for _ in 0..(128 * 1024 / BUF_SIZE) {
            limited.read_exact(&mut chunk).await.unwrap();
        }
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(700), "{took:?}");
        assert!(took < Duration::from_secs(4), "{took:?}");
        limited.shutdown().await.unwrap();
        // pace 等待欠款：超出 1 秒突发额度 500 字节，需等约 50 ms。
        let slow = Limiter::new(10_000).unwrap();
        let start = std::time::Instant::now();
        slow.pace(10_500).await;
        assert!(
            start.elapsed() >= Duration::from_millis(40),
            "{:?}",
            start.elapsed()
        );
        slow.pace(0).await;
    }

    #[tokio::test]
    async fn relays_both_directions_and_counts_bytes() {
        let (mut left, mut left_peer) = duplex(64 * 1024);
        let (mut right, mut right_peer) = duplex(64 * 1024);
        let t = tokio::spawn(async move { relay(&mut left_peer, &mut right_peer).await.unwrap() });
        left.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        right.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
        right.write_all(b"world!!").await.unwrap();
        let mut buf = [0u8; 7];
        left.read_exact(&mut buf).await.unwrap();
        drop(left);
        drop(right);
        assert_eq!(t.await.unwrap(), (5, 7));
    }
    #[tokio::test]
    async fn counted_relay_updates_before_close_and_reclaims_on_cancel() {
        let traffic = std::sync::Arc::new(Traffic::default());
        assert_eq!(traffic.snapshot(), TrafficSnapshot::default());
        let (mut left, mut a) = duplex(4);
        let (mut right, mut b) = duplex(4);
        let counters = traffic.clone();
        let task = tokio::spawn(async move { relay_counted(&mut a, &mut b, &counters).await });
        left.write_all(b"abcd").await.unwrap();
        let mut buf = [0; 4];
        right.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"abcd");
        right.write_all(b"xy").await.unwrap();
        let mut reply = [0; 2];
        left.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"xy");
        assert_eq!(
            traffic.snapshot(),
            TrafficSnapshot {
                bytes_in: 4,
                bytes_out: 2,
                connections: 1,
                connections_total: 1
            }
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(traffic.snapshot().connections, 0);
        assert_eq!(traffic.snapshot().bytes_out, 2);
    }

    #[tokio::test]
    async fn counted_relay_eof_and_failed_write() {
        let traffic = Traffic::default();
        let (left, mut a) = duplex(4);
        let (right, mut b) = duplex(4);
        drop(left);
        drop(right);
        assert_eq!(
            relay_counted(&mut a, &mut b, &traffic).await.unwrap(),
            (0, 0)
        );
        assert_eq!(traffic.snapshot().connections, 0);
        assert_eq!(traffic.snapshot().connections_total, 1);

        let mut writer = Counted {
            io: &mut a,
            traffic: &traffic,
            inbound: true,
        };
        assert!(writer.write_all(b"lost").await.is_err());
        assert_eq!(traffic.snapshot().bytes_in, 0);
        writer.flush().await.unwrap();
        writer.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn datagram_and_prefix_counters_are_exact() {
        let traffic = Traffic::default();
        {
            let _connection = traffic.connection();
            traffic.record_in(7);
            traffic.record_out(11);
            assert_eq!(traffic.snapshot().connections, 1);
        }
        assert_eq!(
            traffic.snapshot(),
            TrafficSnapshot {
                bytes_in: 7,
                bytes_out: 11,
                connections: 0,
                connections_total: 1
            }
        );
        let (left, mut a) = duplex(4);
        let (mut right, mut b) = duplex(4);
        drop(left);
        let reader = tokio::spawn(async move {
            let mut out = Vec::new();
            right.read_to_end(&mut out).await.unwrap();
            out
        });
        assert_eq!(
            relay_counted_prefix(&mut a, &mut b, &traffic, b"header")
                .await
                .unwrap(),
            (0, 0)
        );
        assert_eq!(reader.await.unwrap(), b"header");
        assert_eq!(traffic.snapshot().bytes_in, 13);
        assert_eq!(traffic.snapshot().connections, 0);
    }
    #[test]
    fn child_counters_propagate_and_release_parent_connections() {
        let parent = std::sync::Arc::new(Traffic::default());
        let child = Traffic::child(parent.clone());
        let connection = child.connection();
        child.record_in(5);
        child.record_out(9);
        assert_eq!(parent.snapshot(), child.snapshot());
        assert!(format!("{child:?}").contains("bytes_in: 5"));
        drop(connection);
        assert_eq!(parent.snapshot().connections, 0);
        assert_eq!(parent.snapshot().connections_total, 1);
    }
}
