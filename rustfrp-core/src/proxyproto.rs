//! PROXY protocol v1 / v2 头的生成（只写不读）：把访问者的真实地址告诉内网服务。
//!
//! 与 frp 一致，由客户端在连上本地服务后、转发数据前写入；地址取自 Edge 在 `StartWork` 里带来的
//! 访问者地址与它连接的公网地址。无法解析时退化为 v1 `UNKNOWN` / v2 `LOCAL`，不阻断连接。

use std::net::{IpAddr, SocketAddr};

const V2_SIGNATURE: [u8; 12] = *b"\r\n\r\n\0\r\nQUIT\n";

/// v1 文本头。
pub fn v1(src: &str, dst: &str) -> Vec<u8> {
    match (src.parse::<SocketAddr>(), dst.parse::<SocketAddr>()) {
        (Ok(s), Ok(d)) if s.is_ipv4() == d.is_ipv4() => format!(
            "PROXY {} {} {} {} {}\r\n",
            if s.is_ipv4() { "TCP4" } else { "TCP6" },
            s.ip(),
            d.ip(),
            s.port(),
            d.port()
        )
        .into_bytes(),
        _ => b"PROXY UNKNOWN\r\n".to_vec(),
    }
}

/// v2 二进制头。
pub fn v2(src: &str, dst: &str) -> Vec<u8> {
    let mut out = V2_SIGNATURE.to_vec();
    match (src.parse::<SocketAddr>(), dst.parse::<SocketAddr>()) {
        (Ok(s), Ok(d)) if s.is_ipv4() == d.is_ipv4() => {
            out.push(0x21); // 版本 2，PROXY 命令
            match (s.ip(), d.ip()) {
                (IpAddr::V4(a), IpAddr::V4(b)) => {
                    out.push(0x11); // AF_INET + STREAM
                    out.extend_from_slice(&12u16.to_be_bytes());
                    out.extend_from_slice(&a.octets());
                    out.extend_from_slice(&b.octets());
                }
                (IpAddr::V6(a), IpAddr::V6(b)) => {
                    out.push(0x21); // AF_INET6 + STREAM
                    out.extend_from_slice(&36u16.to_be_bytes());
                    out.extend_from_slice(&a.octets());
                    out.extend_from_slice(&b.octets());
                }
                // 前面已保证同族。
                _ => return local_v2(),
            }
            out.extend_from_slice(&s.port().to_be_bytes());
            out.extend_from_slice(&d.port().to_be_bytes());
            out
        }
        _ => local_v2(),
    }
}

fn local_v2() -> Vec<u8> {
    let mut out = V2_SIGNATURE.to_vec();
    out.extend_from_slice(&[0x20, 0x00, 0x00, 0x00]);
    out
}

/// 按版本生成头；`0` 表示不写，其他取值同样视为不写。
pub fn header(version: u8, src: &str, dst: &str) -> Option<Vec<u8>> {
    match version {
        1 => Some(v1(src, dst)),
        2 => Some(v2(src, dst)),
        _ => None,
    }
}

/// frp 配置里的版本字符串 → 内部取值（空串 = 0）；不认识返回 `Err`。
pub fn parse_version(s: &str) -> Result<u8, String> {
    match s {
        "" => Ok(0),
        "v1" => Ok(1),
        "v2" => Ok(2),
        other => Err(format!(
            "unsupported proxyProtocolVersion \"{other}\" (expected v1 or v2)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_text_headers() {
        assert_eq!(
            v1("192.168.0.1:56324", "10.0.0.5:443"),
            b"PROXY TCP4 192.168.0.1 10.0.0.5 56324 443\r\n"
        );
        assert_eq!(
            v1("[2001:db8::1]:5000", "[2001:db8::2]:80"),
            b"PROXY TCP6 2001:db8::1 2001:db8::2 5000 80\r\n"
        );
        for (s, d) in [("junk", "10.0.0.5:443"), ("1.2.3.4:1", "[::1]:2"), ("", "")] {
            assert_eq!(v1(s, d), b"PROXY UNKNOWN\r\n", "{s} {d}");
        }
    }

    #[test]
    fn v2_binary_headers() {
        let h = v2("1.2.3.4:1000", "5.6.7.8:80");
        assert_eq!(&h[..12], &V2_SIGNATURE);
        assert_eq!(&h[12..16], &[0x21, 0x11, 0, 12]);
        assert_eq!(&h[16..20], &[1, 2, 3, 4]);
        assert_eq!(&h[20..24], &[5, 6, 7, 8]);
        assert_eq!(&h[24..28], &[0x03, 0xe8, 0, 80]);
        assert_eq!(h.len(), 28);
        let h6 = v2("[::1]:1", "[::2]:2");
        assert_eq!(&h6[12..16], &[0x21, 0x21, 0, 36]);
        assert_eq!(h6.len(), 16 + 36);
        assert_eq!(&h6[h6.len() - 4..], &[0, 1, 0, 2]);
        for (s, d) in [("junk", "5.6.7.8:80"), ("1.2.3.4:1", "[::1]:2")] {
            let l = v2(s, d);
            assert_eq!(&l[12..], &[0x20, 0, 0, 0], "{s} {d}");
        }
    }

    #[test]
    fn version_selection_and_parsing() {
        assert!(header(0, "a", "b").is_none() && header(3, "a", "b").is_none());
        assert!(header(1, "1.2.3.4:1", "5.6.7.8:2").is_some());
        assert!(header(2, "1.2.3.4:1", "5.6.7.8:2").is_some());
        assert_eq!(parse_version(""), Ok(0));
        assert_eq!(parse_version("v1"), Ok(1));
        assert_eq!(parse_version("v2"), Ok(2));
        assert!(parse_version("v3").unwrap_err().contains("v3"));
    }
}
