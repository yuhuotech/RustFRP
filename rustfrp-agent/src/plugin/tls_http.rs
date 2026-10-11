//! `http2https` / `https2http` / `https2https` / `tls2raw`：在隧道流与本地服务之间做 TLS 终结 / 发起，
//! HTTP 类按请求改写 Host 与请求头（响应原样回传）。

use std::sync::Arc;
use std::time::Duration;

use rustfrp_core::error::TunnelError;
use rustfrp_core::http1::{parse_request, rewrite_request, ChunkedScanner, Framing};
use rustfrp_core::pipe::{self, Traffic};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use super::{dial, Duplex, PluginCfg};

type Conn = Box<dyn Duplex>;
const MAX_HEAD: usize = rustfrp_core::vhost::MAX_HEAD;

pub(super) async fn serve<S: Duplex + 'static>(
    stream: S,
    c: &PluginCfg,
    acceptor: Option<&TlsAcceptor>,
    connector: Option<&TlsConnector>,
    traffic: &Arc<Traffic>,
    t: Duration,
) -> Result<(), TunnelError> {
    let mut inbound: Conn = match acceptor {
        Some(a) => Box::new(
            tokio::time::timeout(t, a.accept(stream))
                .await
                .map_err(|_| TunnelError::Timeout("plugin tls handshake"))??,
        ),
        None => Box::new(stream),
    };
    let tcp = dial(&c.local_addr, t).await?;
    let mut outbound: Conn = match connector {
        Some(cn) => {
            let host = c
                .local_addr
                .rsplit_once(':')
                .map_or(c.local_addr.as_str(), |(h, _)| h)
                .trim_matches(['[', ']']);
            let name = ServerName::try_from(host.to_string())
                .or_else(|_| ServerName::try_from("localhost".to_string()))
                .map_err(|e| TunnelError::Tls(e.to_string()))?;
            Box::new(
                tokio::time::timeout(t, cn.connect(name, tcp))
                    .await
                    .map_err(|_| TunnelError::Timeout("plugin tls connect"))??,
            )
        }
        None => Box::new(tcp),
    };
    if c.kind == "tls2raw" {
        pipe::relay_counted(&mut *inbound, &mut *outbound, traffic).await?;
        return Ok(());
    }
    let _connection = traffic.connection();
    let (mut ir, mut iw) = tokio::io::split(inbound);
    let (mut or, mut ow) = tokio::io::split(outbound);
    let mut down = Box::pin(async {
        let mut buf = vec![0u8; pipe::BUF_SIZE];
        loop {
            let n = or.read(&mut buf).await?;
            if n == 0 {
                let _ = iw.shutdown().await;
                return Ok::<(), TunnelError>(());
            }
            iw.write_all(buf.get(..n).unwrap_or(&[])).await?;
            traffic.record_out(n as u64);
        }
    });
    let mut up = Box::pin(requests(&mut ir, &mut ow, c, traffic));
    tokio::select! {
        r = &mut down => r,
        r = &mut up => match r {
            // 访问者不再发请求：让本地服务把剩下的响应发完。
            Ok(()) => {
                drop(up);
                down.await
            }
            Err(e) => Err(e),
        },
    }
}

/// 访问者 → 本地服务：逐个请求解析并改写头部，正文按分帧原样转发。
async fn requests<R, W>(
    ir: &mut R,
    ow: &mut W,
    c: &PluginCfg,
    traffic: &Traffic,
) -> Result<(), TunnelError>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; pipe::BUF_SIZE];
    loop {
        let head = loop {
            match parse_request(&buf) {
                Ok(Some(h)) => break h,
                Ok(None) => {}
                Err(_) => return Err(TunnelError::Wire("plugin: malformed request".into())),
            }
            if buf.len() >= MAX_HEAD {
                return Err(TunnelError::Wire("plugin: request head too large".into()));
            }
            let n = ir.read(&mut tmp).await?;
            if n == 0 {
                return if buf.is_empty() {
                    Ok(())
                } else {
                    Err(TunnelError::Wire(
                        "connection closed inside request head".into(),
                    ))
                };
            }
            buf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
            traffic.record_in(n as u64);
        };
        let raw = buf.get(..head.head_len).unwrap_or(&[]);
        if c.host_header_rewrite.is_empty() && c.request_headers.is_empty() {
            ow.write_all(raw).await?;
        } else {
            ow.write_all(&rewrite_request(
                raw,
                &c.host_header_rewrite,
                &c.request_headers,
            ))
            .await?;
        }
        buf.drain(..head.head_len);
        if head.upgrade {
            // 协议升级之后原样透传。
            loop {
                if !buf.is_empty() {
                    ow.write_all(&buf).await?;
                    buf.clear();
                }
                let n = ir.read(&mut tmp).await?;
                if n == 0 {
                    return Ok(());
                }
                traffic.record_in(n as u64);
                buf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
            }
        }
        let mut scanner = ChunkedScanner::new();
        let mut left = match head.framing {
            Framing::None => 0,
            Framing::Length(n) => n,
            Framing::Chunked => 0,
        };
        let mut body = head.framing != Framing::None && !matches!(head.framing, Framing::Length(0));
        while body {
            if buf.is_empty() {
                let n = ir.read(&mut tmp).await?;
                if n == 0 {
                    return Err(TunnelError::Wire(
                        "connection closed inside request body".into(),
                    ));
                }
                traffic.record_in(n as u64);
                buf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
            }
            let (take, finished) = if head.framing == Framing::Chunked {
                scanner
                    .feed(&buf)
                    .map_err(|e| TunnelError::Wire(format!("{e:?}")))?
            } else {
                let take = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
                left -= take as u64;
                (take, left == 0)
            };
            ow.write_all(buf.get(..take).unwrap_or(&[])).await?;
            buf.drain(..take);
            body = !finished;
        }
    }
}
