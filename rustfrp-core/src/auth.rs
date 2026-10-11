//! 令牌鉴权：`hex(sha256(token || timestamp))`，常数时间比较，限制时间偏移。
//!
//! 令牌只在 TLS 通道内传输；这里的摘要用于避免明文令牌出现在日志与抓包中，
//! 并通过时间戳限制重放窗口（与 frp 的 token 鉴权思路一致）。

use sha2::{Digest, Sha256};

/// 允许的时间偏移（秒）。
pub const MAX_SKEW_SECS: i64 = 15 * 60;

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 0x0f), 16).unwrap_or('0'));
    }
    s
}

/// 计算鉴权摘要。
pub fn auth_key(token: &str, timestamp: i64) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    h.update(timestamp.to_string().as_bytes());
    hex(&h.finalize())
}

fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// 校验：时间戳在窗口内且摘要匹配。
pub fn verify(token: &str, timestamp: i64, presented: &str, now: i64) -> bool {
    if (now - timestamp).abs() > MAX_SKEW_SECS {
        return false;
    }
    ct_eq(&auth_key(token, timestamp), presented)
}

/// 当前 Unix 秒。
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 生成 16 字节随机标识（hex）。仅用作标识，不是密钥；系统随机源不可用时得到全零标识。
pub fn random_id() -> String {
    let mut buf = [0u8; 16];
    let _ = getrandom::fill(&mut buf);
    hex(&buf)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable_hex() {
        let k = auth_key("secret", 1700000000);
        assert_eq!(k.len(), 64);
        assert!(k.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(k, auth_key("secret", 1700000000));
        assert_ne!(k, auth_key("secret", 1700000001));
        assert_ne!(k, auth_key("other", 1700000000));
    }

    #[test]
    fn verify_accepts_and_rejects() {
        let now = 1_700_000_000;
        let k = auth_key("t", now);
        assert!(verify("t", now, &k, now));
        assert!(verify("t", now, &k, now + MAX_SKEW_SECS));
        assert!(!verify("t", now, &k, now + MAX_SKEW_SECS + 1));
        assert!(!verify("t", now, &k, now - MAX_SKEW_SECS - 1));
        assert!(!verify("wrong", now, &k, now));
        assert!(!verify("t", now, "short", now));
        assert!(!verify("t", now, &"0".repeat(64), now));
    }

    #[test]
    fn ids_are_random_hex() {
        let (a, b) = (random_id(), random_id());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(unix_now() > 1_600_000_000);
    }
}
