//! 虚拟主机路由所需的纯解析逻辑：HTTP `Host`、TLS `SNI`、域名匹配与 Host 改写。
//! 全部是无副作用的字节处理，面对任意输入不得 panic。

/// 读取请求头 / ClientHello 的最大字节数。
pub const MAX_HEAD: usize = 16 * 1024;

fn lower_host(raw: &str) -> Option<String> {
    let t = raw.trim();
    // 去掉端口；IPv6 字面量 `[::1]:80` 保留方括号内的内容。
    let host = if let Some(rest) = t.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        t.rsplit_once(':').map_or(t, |(h, p)| {
            if p.bytes().all(|b| b.is_ascii_digit()) {
                h
            } else {
                t
            }
        })
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// 请求头是否已完整（出现空行）。
pub fn head_complete(buf: &[u8]) -> bool {
    buf.windows(4).any(|w| w == b"\r\n\r\n")
}

/// 从 HTTP 请求头里取 `Host`（小写、无端口）。
pub fn http_host(head: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(head).ok().or_else(|| {
        // 请求体里可能有非 UTF-8；只看头部。
        let end = head
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map_or(head.len(), |p| p + 4);
        std::str::from_utf8(head.get(..end)?).ok()
    })?;
    text.split("\r\n")
        .skip(1)
        .take_while(|l| !l.is_empty())
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("host")
                .then(|| lower_host(v))
                .flatten()
        })
}

/// 把请求头里的 `Host` 改写为 `new_host`；没有 Host 行则原样返回。只处理头部，正文字节不动。
pub fn rewrite_host(head: &[u8], new_host: &str) -> Vec<u8> {
    let end = head
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(head.len(), |p| p + 4);
    let (h, rest) = head.split_at(end);
    let Ok(text) = std::str::from_utf8(h) else {
        return head.to_vec();
    };
    let mut out = String::with_capacity(text.len() + new_host.len());
    let mut first = true;
    for line in text.split_inclusive("\r\n") {
        let is_host = !first
            && line
                .split_once(':')
                .is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case("host"));
        first = false;
        if is_host {
            out.push_str("Host: ");
            out.push_str(new_host);
            out.push_str("\r\n");
        } else {
            out.push_str(line);
        }
    }
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(rest);
    bytes
}

/// SNI 解析结果。
#[derive(Debug, PartialEq, Eq)]
pub enum Sni {
    /// 找到了服务器名（小写）。
    Found(String),
    /// 数据还不够，继续读。
    Incomplete,
    /// 不是 TLS ClientHello，或没有 SNI 扩展。
    None,
}

struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (a, b) = self.0.split_at_checked(n)?;
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<usize> {
        self.take(1)?.first().map(|b| usize::from(*b))
    }
    fn u16(&mut self) -> Option<usize> {
        let b = self.take(2)?;
        Some(usize::from(*b.first()?) << 8 | usize::from(*b.get(1)?))
    }
}

/// 解析 TLS ClientHello 中的 SNI。
pub fn tls_sni(buf: &[u8]) -> Sni {
    if buf.len() < 5 {
        return if buf.first().is_none_or(|b| *b == 0x16) {
            Sni::Incomplete
        } else {
            Sni::None
        };
    }
    let mut c = Cur(buf);
    // record: type(1)=handshake version(2) length(2)
    if c.u8() != Some(0x16) {
        return Sni::None;
    }
    let _ = c.take(2);
    let Some(rec_len) = c.u16() else {
        return Sni::None;
    };
    let Some(body) = c.take(rec_len.min(c.0.len())) else {
        return Sni::None;
    };
    let truncated = c.0.is_empty() && body.len() < rec_len;
    match parse_hello(body) {
        Some(r) => r,
        None if truncated => Sni::Incomplete,
        None => Sni::None,
    }
}

fn parse_hello(body: &[u8]) -> Option<Sni> {
    let mut c = Cur(body);
    if c.u8()? != 1 {
        return Some(Sni::None); // 不是 ClientHello
    }
    c.take(3)?; // handshake length
    c.take(2 + 32)?; // version + random
    let sid = c.u8()?;
    c.take(sid)?;
    let cs = c.u16()?;
    c.take(cs)?;
    let comp = c.u8()?;
    c.take(comp)?;
    let ext_len = c.u16()?;
    let mut ext = Cur(c.take(ext_len)?);
    while !ext.0.is_empty() {
        let ty = ext.u16()?;
        let len = ext.u16()?;
        let data = ext.take(len)?;
        if ty == 0 {
            let mut d = Cur(data);
            d.u16()?; // list length
            if d.u8()? != 0 {
                return Some(Sni::None);
            }
            let n = d.u16()?;
            let name = std::str::from_utf8(d.take(n)?).ok()?;
            return Some(Sni::Found(name.to_ascii_lowercase()));
        }
    }
    Some(Sni::None)
}

/// 域名匹配：精确匹配，或 `*.example.com` 通配（只匹配其下的子域，不匹配 `example.com` 自身）。
pub fn domain_matches(pattern: &str, host: &str) -> bool {
    let (p, h) = (pattern.to_ascii_lowercase(), host.to_ascii_lowercase());
    match p.strip_prefix("*.") {
        Some(suffix) => {
            h.len() > suffix.len() + 1
                && h.ends_with(suffix)
                && h.as_bytes().get(h.len() - suffix.len() - 1) == Some(&b'.')
        }
        None => p == h,
    }
}

/// 一个 HTTP/HTTPS 代理应当响应的全部主机名：自定义域名 + `子域名.根域名`。
pub fn proxy_hosts(
    custom_domains: &[String],
    subdomain: &str,
    sub_domain_host: &str,
) -> Vec<String> {
    let mut v: Vec<String> = custom_domains
        .iter()
        .map(|d| d.to_ascii_lowercase())
        .collect();
    if !subdomain.is_empty() && !sub_domain_host.is_empty() {
        v.push(format!(
            "{}.{}",
            subdomain.to_ascii_lowercase(),
            sub_domain_host.to_ascii_lowercase()
        ));
    }
    v
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    fn hello(sni: Option<&str>) -> Vec<u8> {
        let mut ext = Vec::new();
        if let Some(name) = sni {
            let n = name.as_bytes();
            let mut e = vec![0u8, 0];
            let list_len = 3 + n.len();
            e.extend_from_slice(&((list_len + 2) as u16).to_be_bytes());
            e.extend_from_slice(&(list_len as u16).to_be_bytes());
            e.push(0);
            e.extend_from_slice(&(n.len() as u16).to_be_bytes());
            e.extend_from_slice(n);
            ext.extend(e);
        }
        // 另一个无关扩展（supported_versions）放在前面，验证会跳过。
        let mut exts = vec![0, 43, 0, 3, 2, 3, 4];
        exts.extend(ext);
        let mut hs = vec![3, 3];
        hs.extend([7u8; 32]);
        hs.push(0);
        hs.extend([0, 2, 0x13, 0x01]);
        hs.extend([1, 0]);
        hs.extend((exts.len() as u16).to_be_bytes());
        hs.extend(exts);
        let mut body = vec![1];
        body.extend([
            (hs.len() >> 16) as u8,
            (hs.len() >> 8) as u8,
            hs.len() as u8,
        ]);
        body.extend(hs);
        let mut rec = vec![0x16, 3, 1];
        rec.extend((body.len() as u16).to_be_bytes());
        rec.extend(body);
        rec
    }

    #[test]
    fn http_host_parsing() {
        let h = b"GET / HTTP/1.1\r\nUser-Agent: x\r\nHOST: Example.COM:8080\r\n\r\nbody";
        assert_eq!(http_host(h).as_deref(), Some("example.com"));
        assert_eq!(
            http_host(b"GET / HTTP/1.1\r\nHost: [::1]:80\r\n\r\n").as_deref(),
            Some("::1")
        );
        assert_eq!(
            http_host(b"GET / HTTP/1.1\r\nHost: a.b\r\n\r\n").as_deref(),
            Some("a.b")
        );
        assert_eq!(
            http_host(b"GET / HTTP/1.1\r\nHost: foo:bar\r\n\r\n").as_deref(),
            Some("foo:bar")
        );
        assert_eq!(http_host(b"GET / HTTP/1.1\r\nAccept: */*\r\n\r\n"), None);
        assert_eq!(http_host(b"GET / HTTP/1.1\r\nHost:   \r\n\r\n"), None);
        assert_eq!(
            http_host(b"GET / HTTP/1.1\r\nbad line\r\nHost: x\r\n\r\n").as_deref(),
            Some("x")
        );
        // 头部之后的二进制正文不影响解析。
        let mut bin = b"POST / HTTP/1.1\r\nHost: up.load\r\n\r\n".to_vec();
        bin.extend([0xff, 0xfe, 0x00]);
        assert_eq!(http_host(&bin).as_deref(), Some("up.load"));
        assert_eq!(http_host(&[0xff, 0xfe]), None);
        assert_eq!(http_host(b""), None);
    }

    #[test]
    fn head_completion() {
        assert!(head_complete(b"GET / HTTP/1.1\r\n\r\n"));
        assert!(!head_complete(b"GET / HTTP/1.1\r\nHost: x\r\n"));
        assert!(!head_complete(b""));
    }

    #[test]
    fn host_rewrite_touches_only_the_head() {
        let head =
            b"GET /a HTTP/1.1\r\nhost: public.example.com\r\nX: 1\r\n\r\nHost: not-a-header\r\n";
        let out = rewrite_host(head, "internal.lan:8080");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "GET /a HTTP/1.1\r\nHost: internal.lan:8080\r\nX: 1\r\n\r\nHost: not-a-header\r\n"
        );
        let nohost = b"GET / HTTP/1.1\r\nX: 1\r\n\r\n";
        assert_eq!(rewrite_host(nohost, "h"), nohost);
        assert_eq!(
            rewrite_host(&[0xff, 0xfe, 0xfd], "h"),
            vec![0xff, 0xfe, 0xfd]
        );
        // 没有空行（头部不完整）也只改 Host 行。
        assert_eq!(
            rewrite_host(b"GET / HTTP/1.1\r\nHost: a\r\n", "b"),
            b"GET / HTTP/1.1\r\nHost: b\r\n"
        );
    }

    #[test]
    fn sni_found_incomplete_and_absent() {
        let full = hello(Some("Secure.Example.com"));
        assert_eq!(tls_sni(&full), Sni::Found("secure.example.com".into()));
        assert_eq!(tls_sni(&hello(None)), Sni::None);
        // 逐字节截断：要么 Incomplete 要么 None，绝不 panic，也不会给出错误的 Found。
        for n in 0..full.len() {
            let r = tls_sni(&full[..n]);
            assert!(matches!(r, Sni::Incomplete | Sni::None), "{n}: {r:?}");
        }
        assert_eq!(tls_sni(&full[..4]), Sni::Incomplete);
        assert_eq!(tls_sni(b""), Sni::Incomplete);
        assert_eq!(tls_sni(b"GET / HTTP/1.1\r\n"), Sni::None);
        assert_eq!(tls_sni(b"G"), Sni::None);
        // 握手类型不是 ClientHello。
        let mut not_hello = full.clone();
        not_hello[5] = 2;
        assert_eq!(tls_sni(&not_hello), Sni::None);
        // 名称类型不是 host_name(0)。
        let mut odd = full.clone();
        let pos = odd.len() - "Secure.Example.com".len() - 3;
        odd[pos] = 9;
        assert_eq!(tls_sni(&odd), Sni::None);
    }

    #[test]
    fn domain_matching() {
        assert!(domain_matches("a.example.com", "A.Example.com"));
        assert!(!domain_matches("a.example.com", "b.example.com"));
        assert!(domain_matches("*.example.com", "x.example.com"));
        assert!(domain_matches("*.example.com", "x.y.example.com"));
        assert!(!domain_matches("*.example.com", "example.com"));
        assert!(!domain_matches("*.example.com", "badexample.com"));
        assert!(!domain_matches("*.example.com", ".example.com"));
    }

    #[test]
    fn hosts_of_a_proxy() {
        let d = vec!["A.com".to_string()];
        assert_eq!(
            proxy_hosts(&d, "web", "Tunnel.example.com"),
            vec!["a.com", "web.tunnel.example.com"]
        );
        assert_eq!(proxy_hosts(&d, "", "t.com"), vec!["a.com"]);
        assert_eq!(proxy_hosts(&d, "web", ""), vec!["a.com"]);
        assert!(proxy_hosts(&[], "", "").is_empty());
    }
}
