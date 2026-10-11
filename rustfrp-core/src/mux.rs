//! 多路复用：把一条（TLS）连接变成可随时开启新流的会话。
//!
//! 基于 yamux。驱动任务统一轮询连接：既接受对端开的流，也响应本端的「开流」请求。
//! 相比 frp 的「预建工作连接池」，开一条流只是一个帧，没有预建连接的内存与延迟开销。

use std::future::poll_fn;

use futures::io::{AsyncRead as FAsyncRead, AsyncWrite as FAsyncWrite};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

use crate::error::TunnelError;

/// 一条逻辑流（tokio 读写接口）。
pub type Stream = Compat<yamux::Stream>;

/// 多路复用参数。
///
/// 注意：yamux 要求「连接接收窗口 ≥ 256 KiB × 最大流数」，它只是上限而非预分配；
/// 实际占用取决于「已到达但尚未被读走」的数据量，转发任务会立即读走，所以常态很小。
/// 恶意的已认证对端理论上可让每条流缓冲到 256 KiB，因此默认流数上限取 4096（frp 无此限制，过低会让合法的高并发被拒绝）。
/// 若后续基准显示不可接受，可在本模块内替换为固定小窗口的自研多路复用。
#[derive(Debug, Clone, Copy)]
pub struct MuxConfig {
    /// 最大并发流数。
    pub max_streams: usize,
}

impl Default for MuxConfig {
    fn default() -> Self {
        Self { max_streams: 4096 }
    }
}

type OpenReply = oneshot::Sender<Result<Stream, TunnelError>>;

/// 会话句柄：可克隆的「开流」入口。
#[derive(Clone)]
pub struct Session {
    open_tx: mpsc::Sender<OpenReply>,
}

impl Session {
    /// 向对端开一条新流。
    pub async fn open(&self) -> Result<Stream, TunnelError> {
        let (tx, rx) = oneshot::channel();
        self.open_tx
            .send(tx)
            .await
            .map_err(|_| TunnelError::Closed)?;
        rx.await.map_err(|_| TunnelError::Closed)?
    }
}

/// 在 `io` 上建立会话。返回（开流句柄，对端开来的流，驱动任务）。
///
/// 驱动任务在连接关闭 / 出错后结束，此时 inbound 通道关闭，`open` 返回 `Closed`。
pub fn spawn<T>(
    io: T,
    server: bool,
    cfg: MuxConfig,
) -> (Session, mpsc::Receiver<Stream>, JoinHandle<()>)
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    spawn_futures_io(io.compat(), server, cfg)
}

fn spawn_futures_io<T>(
    io: T,
    server: bool,
    cfg: MuxConfig,
) -> (Session, mpsc::Receiver<Stream>, JoinHandle<()>)
where
    T: FAsyncRead + FAsyncWrite + Unpin + Send + 'static,
{
    let mut ycfg = yamux::Config::default();
    ycfg.set_max_num_streams(cfg.max_streams);
    ycfg.set_max_connection_receive_window(Some(
        cfg.max_streams
            .saturating_mul(yamux::DEFAULT_CREDIT as usize),
    ));
    let mode = if server {
        yamux::Mode::Server
    } else {
        yamux::Mode::Client
    };
    let conn = yamux::Connection::new(io, ycfg, mode);
    let (open_tx, open_rx) = mpsc::channel(16);
    let (in_tx, in_rx) = mpsc::channel(16);
    let task = tokio::spawn(drive(conn, open_rx, in_tx));
    (Session { open_tx }, in_rx, task)
}

async fn drive<T>(
    mut conn: yamux::Connection<T>,
    mut open_rx: mpsc::Receiver<OpenReply>,
    in_tx: mpsc::Sender<Stream>,
) where
    T: FAsyncRead + FAsyncWrite + Unpin + Send + 'static,
{
    loop {
        tokio::select! {
            req = open_rx.recv() => {
                let Some(reply) = req else { break };
                let r = poll_fn(|cx| conn.poll_new_outbound(cx)).await;
                // 达到流数上限只是本次开流失败，会话仍然可用；其余错误意味着连接已不可用。
                let fatal = matches!(&r, Err(e) if !matches!(e, yamux::ConnectionError::TooManyStreams));
                let _ = reply.send(r.map(|s| s.compat()).map_err(|e| TunnelError::Mux(e.to_string())));
                if fatal { break }
            }
            inbound = poll_fn(|cx| conn.poll_next_inbound(cx)) => {
                match inbound {
                    Some(Ok(s)) => {
                        if in_tx.send(s.compat()).await.is_err() { break }
                    }
                    _ => break,
                }
            }
        }
    }
    let _ = poll_fn(|cx| conn.poll_close(cx)).await;
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

    fn pair() -> (
        (Session, mpsc::Receiver<Stream>),
        (Session, mpsc::Receiver<Stream>),
    ) {
        let (a, b) = duplex(256 * 1024);
        let (cs, ci, _) = spawn(a, false, MuxConfig::default());
        let (ss, si, _) = spawn(b, true, MuxConfig::default());
        ((cs, ci), (ss, si))
    }

    #[tokio::test]
    async fn server_opens_stream_to_client_and_data_flows_both_ways() {
        let ((_cs, mut client_in), (ss, _si)) = pair();
        let mut s_stream = ss.open().await.unwrap();
        s_stream.write_all(b"ping").await.unwrap();
        s_stream.flush().await.unwrap();
        let mut c_stream = client_in.recv().await.unwrap();
        let mut buf = [0u8; 4];
        c_stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
        c_stream.write_all(b"pong").await.unwrap();
        c_stream.flush().await.unwrap();
        s_stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }

    #[tokio::test]
    async fn many_streams_in_parallel() {
        let ((_cs, mut client_in), (ss, _si)) = pair();
        let echo = tokio::spawn(async move {
            while let Some(mut s) = client_in.recv().await {
                tokio::spawn(async move {
                    let mut b = [0u8; 8];
                    if s.read_exact(&mut b).await.is_ok() {
                        let _ = s.write_all(&b).await;
                        let _ = s.flush().await;
                    }
                });
            }
        });
        let mut handles = Vec::new();
        for i in 0..20u64 {
            let ss = ss.clone();
            handles.push(tokio::spawn(async move {
                let mut s = ss.open().await.unwrap();
                s.write_all(&i.to_be_bytes()).await.unwrap();
                s.flush().await.unwrap();
                let mut b = [0u8; 8];
                s.read_exact(&mut b).await.unwrap();
                assert_eq!(u64::from_be_bytes(b), i);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        echo.abort();
    }

    #[tokio::test]
    async fn open_fails_after_peer_goes_away() {
        let (a, b) = duplex(64 * 1024);
        let (session, _inbound, task) = spawn(a, true, MuxConfig::default());
        drop(b);
        task.await.unwrap();
        assert!(matches!(session.open().await, Err(TunnelError::Closed)));
    }

    #[tokio::test]
    async fn driver_stops_when_inbound_receiver_is_dropped() {
        let ((cs, ci), (ss, _si)) = pair();
        drop(ci);
        let mut s = ss.open().await.unwrap();
        s.write_all(b"x").await.unwrap();
        s.flush().await.unwrap();
        // 对端开流后本端无人接收 → 驱动退出 → 之后开流返回 Closed。
        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if matches!(cs.open().await, Err(TunnelError::Closed)) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(closed.is_ok());
    }

    #[tokio::test]
    async fn driver_stops_when_all_sessions_dropped() {
        let (a, _b) = duplex(64 * 1024);
        let (session, inbound, task) = spawn(a, false, MuxConfig::default());
        drop(session);
        task.await.unwrap();
        drop(inbound);
    }

    #[tokio::test]
    async fn opening_beyond_the_stream_limit_is_an_error() {
        let (a, b) = duplex(64 * 1024);
        let cfg = MuxConfig { max_streams: 1 };
        let (cs, _ci, _) = spawn(a, false, cfg);
        let (_ss, mut si, _) = spawn(b, true, cfg);
        // yamux 的流在首次写入时才通知对端。
        let mut first = cs.open().await.unwrap();
        first.write_all(b"x").await.unwrap();
        first.flush().await.unwrap();
        let _accepted = si.recv().await.unwrap();
        let err = cs.open().await.err().unwrap();
        assert_eq!(err.to_string(), "mux: maximum number of streams reached");
    }
}
