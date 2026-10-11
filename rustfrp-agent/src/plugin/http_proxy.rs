//! HTTP 正向代理：`CONNECT` 隧道与绝对形式的明文请求；可选 `Proxy-Authorization` Basic 认证。

use std::sync::Arc;
use std::time::Duration;

use rustfrp_core::error::TunnelError;
use rustfrp_core::http1::{ct_eq, parse_basic};
use rustfrp_core::pipe::{self, Traffic};
use tokio::io::AsyncWriteExt;

use super::{dial, read_head, Duplex, PluginCfg};

const HEAD_LIMIT: usize = 16 * 1024;
const UNAUTHORIZED: &[u8] = b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"Restricted\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_REQUEST: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_GATEWAY: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// `http://host[:port]/path` → （`host:port`, 原样路径）。
fn split_absolute(target: &str) -> Option<(String, String)> {
    let rest = target.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest.get(..i)?, rest.get(i..)?),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    let has_port = authority.rsplit_once(':').is_some_and(|(h, p)| {
        !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && !h.is_empty()
    }) && !authority.ends_with(']');
    let host = if has_port {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };
    Some((host, path.to_string()))
}

pub(super) async fn serve<S: Duplex>(
    mut s: S,
    c: &PluginCfg,
    traffic: &Arc<Traffic>,
    t: Duration,
) -> Result<(), TunnelError> {
    let (head_len, buf) = read_head(&mut s, HEAD_LIMIT, t).await?;
    let head = buf.get(..head_len).unwrap_or(&[]);
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    let parsed = req.parse(head);
    let (Ok(httparse::Status::Complete(_)), Some(method), Some(target), Some(minor)) =
        (parsed, req.method, req.path, req.version)
    else {
        s.write_all(BAD_REQUEST).await?;
        return Err(TunnelError::Wire("http_proxy: malformed request".into()));
    };
    if !c.http_user.is_empty() || !c.http_password.is_empty() {
        let ok = req
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("proxy-authorization"))
            .and_then(|h| std::str::from_utf8(h.value).ok())
            .and_then(parse_basic)
            .is_some_and(|(u, p)| ct_eq(&u, &c.http_user) & ct_eq(&p, &c.http_password));
        if !ok {
            s.write_all(UNAUTHORIZED).await?;
            return Err(TunnelError::Auth("http_proxy: bad credentials".into()));
        }
    }
    let leftover = buf.get(head_len..).unwrap_or(&[]);
    if method.eq_ignore_ascii_case("CONNECT") {
        let dest = if target.contains(':') {
            target.to_string()
        } else {
            format!("{target}:443")
        };
        let mut remote = match dial(&dest, t).await {
            Ok(r) => r,
            Err(e) => {
                s.write_all(BAD_GATEWAY).await?;
                return Err(e);
            }
        };
        s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        pipe::relay_counted_prefix(&mut s, &mut remote, traffic, leftover).await?;
        return Ok(());
    }
    let Some((dest, path)) = split_absolute(target) else {
        s.write_all(BAD_REQUEST).await?;
        return Err(TunnelError::Wire(
            "http_proxy: only absolute http:// targets are proxied".into(),
        ));
    };
    let mut remote = match dial(&dest, t).await {
        Ok(r) => r,
        Err(e) => {
            s.write_all(BAD_GATEWAY).await?;
            return Err(e);
        }
    };
    // 改写成源站形式的请求；每个连接只代理一个请求（Connection: close）。
    let mut out = format!("{method} {path} HTTP/1.{minor}\r\n").into_bytes();
    for h in req.headers.iter() {
        let n = h.name.to_ascii_lowercase();
        if ["proxy-authorization", "proxy-connection", "connection"].contains(&n.as_str()) {
            continue;
        }
        out.extend_from_slice(h.name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(h.value);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    out.extend_from_slice(leftover);
    pipe::relay_counted_prefix(&mut s, &mut remote, traffic, &out).await?;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::split_absolute;

    #[test]
    fn absolute_targets_split_into_authority_and_path() {
        let s = |t: &str| split_absolute(t);
        assert_eq!(
            s("http://a.com/x?y=1"),
            Some(("a.com:80".into(), "/x?y=1".into()))
        );
        assert_eq!(s("http://a.com"), Some(("a.com:80".into(), "/".into())));
        assert_eq!(
            s("http://a.com:8080/p"),
            Some(("a.com:8080".into(), "/p".into()))
        );
        assert_eq!(s("http://[::1]/p"), Some(("[::1]:80".into(), "/p".into())));
        assert_eq!(
            s("http://[::1]:81/p"),
            Some(("[::1]:81".into(), "/p".into()))
        );
        assert_eq!(s("http:///p"), None);
        assert_eq!(s("https://a.com/"), None);
        assert_eq!(s("/relative"), None);
        // 端口为空或不是数字：当作主机名的一部分，补默认端口。
        assert_eq!(
            s("http://a.com:/p"),
            Some(("a.com::80".into(), "/p".into()))
        );
    }
}
