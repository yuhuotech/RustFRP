//! UDP 数据报在字节流上的封装：`u16` 大端长度 + 载荷。

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::TunnelError;

/// UDP 载荷上限。
pub const MAX_DATAGRAM: usize = 65_507;

/// 写一个数据报。
pub async fn write_datagram<W: AsyncWrite + Unpin>(
    w: &mut W,
    data: &[u8],
) -> Result<(), TunnelError> {
    let len =
        u16::try_from(data.len()).map_err(|_| TunnelError::Wire("datagram too large".into()))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(data).await?;
    w.flush().await?;
    Ok(())
}

/// 读一个数据报；对端正常关闭返回 `None`。
pub async fn read_datagram<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
) -> Result<Option<usize>, TunnelError> {
    let mut len = [0u8; 2];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let n = usize::from(u16::from_be_bytes(len));
    buf.resize(n, 0);
    r.read_exact(buf).await?;
    Ok(Some(n))
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn roundtrip_and_eof() {
        let (mut a, mut b) = duplex(1 << 20);
        write_datagram(&mut a, b"hello").await.unwrap();
        write_datagram(&mut a, b"").await.unwrap();
        write_datagram(&mut a, &vec![7u8; MAX_DATAGRAM])
            .await
            .unwrap();
        let mut buf = Vec::new();
        assert_eq!(read_datagram(&mut b, &mut buf).await.unwrap(), Some(5));
        assert_eq!(buf, b"hello");
        assert_eq!(read_datagram(&mut b, &mut buf).await.unwrap(), Some(0));
        assert_eq!(
            read_datagram(&mut b, &mut buf).await.unwrap(),
            Some(MAX_DATAGRAM)
        );
        drop(a);
        assert_eq!(read_datagram(&mut b, &mut buf).await.unwrap(), None);
    }

    #[tokio::test]
    async fn errors() {
        let (mut a, _b) = duplex(1 << 20);
        assert!(matches!(
            write_datagram(&mut a, &vec![0u8; 70_000]).await,
            Err(TunnelError::Wire(_))
        ));
        // 长度头之后连接断开 → I/O 错误而不是静默成功。
        let (mut a, mut b) = duplex(64);
        a.write_all(&[0, 10, 1, 2]).await.unwrap();
        drop(a);
        let mut buf = Vec::new();
        assert!(matches!(
            read_datagram(&mut b, &mut buf).await,
            Err(TunnelError::Io(_))
        ));
        let (a, mut b) = duplex(8);
        drop(a);
        assert_eq!(read_datagram(&mut b, &mut buf).await.unwrap(), None);
        let (mut a, b) = duplex(1);
        drop(b);
        assert!(write_datagram(&mut a, b"x").await.is_err());
    }
}
