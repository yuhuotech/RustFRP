//! 双向转发。
//!
//! 每个方向使用 8 KiB 缓冲（单连接合计 16 KiB），是「极致省内存」原则下的折中：
//! 缓冲随连接存在而存在，空闲连接约占 16 KiB + 任务开销。

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

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

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
}
