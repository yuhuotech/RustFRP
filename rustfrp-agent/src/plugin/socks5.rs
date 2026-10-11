//! SOCKS5（RFC 1928）：仅 CONNECT；配置了用户名 / 密码时要求 RFC 1929 认证。

use std::sync::Arc;
use std::time::Duration;

use rustfrp_core::error::TunnelError;
use rustfrp_core::http1::ct_eq;
use rustfrp_core::pipe::{self, Traffic};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{dial, Duplex, PluginCfg};

fn bad(m: &str) -> TunnelError {
    TunnelError::Wire(format!("socks5: {m}"))
}

async fn exact<S: Duplex>(s: &mut S, n: usize, t: Duration) -> Result<Vec<u8>, TunnelError> {
    let mut v = vec![0u8; n];
    tokio::time::timeout(t, s.read_exact(&mut v))
        .await
        .map_err(|_| TunnelError::Timeout("socks5 handshake"))??;
    Ok(v)
}

async fn byte<S: Duplex>(s: &mut S, t: Duration) -> Result<u8, TunnelError> {
    Ok(exact(s, 1, t).await?.first().copied().unwrap_or(0))
}

/// 目标地址 `host:port`（IPv6 加方括号）。
fn target(atyp: u8, addr: &[u8], port: u16) -> Result<String, TunnelError> {
    Ok(match atyp {
        1 => {
            let o: [u8; 4] = addr.try_into().map_err(|_| bad("bad ipv4"))?;
            format!("{}:{port}", std::net::Ipv4Addr::from(o))
        }
        4 => {
            let o: [u8; 16] = addr.try_into().map_err(|_| bad("bad ipv6"))?;
            format!("[{}]:{port}", std::net::Ipv6Addr::from(o))
        }
        _ => format!(
            "{}:{port}",
            std::str::from_utf8(addr).map_err(|_| bad("bad domain"))?
        ),
    })
}

pub(super) async fn serve<S: Duplex>(
    mut s: S,
    c: &PluginCfg,
    traffic: &Arc<Traffic>,
    t: Duration,
) -> Result<(), TunnelError> {
    // 问候：VER NMETHODS METHODS...
    let head = exact(&mut s, 2, t).await?;
    if head.first() != Some(&5) {
        return Err(bad("not a socks5 client"));
    }
    let n = usize::from(head.get(1).copied().unwrap_or(0));
    let methods = exact(&mut s, n, t).await?;
    let need_auth = !c.username.is_empty() || !c.password.is_empty();
    let method = if need_auth { 2u8 } else { 0u8 };
    if !methods.contains(&method) {
        s.write_all(&[5, 0xff]).await?;
        return Err(bad("no acceptable authentication method"));
    }
    s.write_all(&[5, method]).await?;
    if need_auth {
        // VER(1) ULEN UNAME PLEN PASSWD
        if byte(&mut s, t).await? != 1 {
            return Err(bad("bad auth version"));
        }
        let ulen = usize::from(byte(&mut s, t).await?);
        let user = exact(&mut s, ulen, t).await?;
        let plen = usize::from(byte(&mut s, t).await?);
        let pass = exact(&mut s, plen, t).await?;
        let ok = ct_eq(&String::from_utf8_lossy(&user), &c.username)
            & ct_eq(&String::from_utf8_lossy(&pass), &c.password);
        s.write_all(&[1, u8::from(!ok)]).await?;
        if !ok {
            return Err(bad("authentication failed"));
        }
    }
    // 请求：VER CMD RSV ATYP ADDR PORT
    let req = exact(&mut s, 4, t).await?;
    let (cmd, atyp) = (
        req.get(1).copied().unwrap_or(0),
        req.get(3).copied().unwrap_or(0),
    );
    let reply = |code: u8| [5, code, 0, 1, 0, 0, 0, 0, 0, 0];
    if cmd != 1 {
        s.write_all(&reply(7)).await?;
        return Err(bad("only CONNECT is supported"));
    }
    let addr = match atyp {
        1 => exact(&mut s, 4, t).await?,
        4 => exact(&mut s, 16, t).await?,
        3 => {
            let len = usize::from(byte(&mut s, t).await?);
            exact(&mut s, len, t).await?
        }
        _ => {
            s.write_all(&reply(8)).await?;
            return Err(bad("unsupported address type"));
        }
    };
    let p = exact(&mut s, 2, t).await?;
    let port = u16::from_be_bytes([
        p.first().copied().unwrap_or(0),
        p.get(1).copied().unwrap_or(0),
    ]);
    let dest = target(atyp, &addr, port)?;
    let mut remote = match dial(&dest, t).await {
        Ok(r) => r,
        Err(e) => {
            s.write_all(&reply(5)).await?;
            return Err(e);
        }
    };
    s.write_all(&reply(0)).await?;
    pipe::relay_counted(&mut s, &mut remote, traffic).await?;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::target;

    #[test]
    fn targets_format_each_address_type() {
        assert_eq!(target(1, &[10, 0, 0, 1], 80).unwrap(), "10.0.0.1:80");
        let mut v6 = [0u8; 16];
        v6[15] = 1;
        assert_eq!(target(4, &v6, 443).unwrap(), "[::1]:443");
        assert_eq!(target(3, b"example.com", 8080).unwrap(), "example.com:8080");
        for (atyp, addr) in [
            (1u8, &b"123"[..]),
            (4, &b"1234"[..]),
            (3, &[0xff, 0xfe][..]),
        ] {
            assert!(target(atyp, addr, 1).is_err(), "{atyp}");
        }
    }
}
