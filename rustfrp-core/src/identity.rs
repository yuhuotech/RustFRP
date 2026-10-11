//! 设备身份：Ed25519 密钥对。私钥只留在本机，公钥在接入时交给 Controller；
//! 之后每次登录用私钥对「设备 ID + 时间戳」签名，服务端用公钥验签（不需要在服务端保存任何共享密钥）。

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};

use crate::error::TunnelError;

/// 允许的时间偏移（秒）。
pub use crate::auth::MAX_SKEW_SECS;

/// 设备密钥对。
pub struct DeviceKey {
    pair: Ed25519KeyPair,
    pkcs8: Vec<u8>,
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 0x0f), 16).unwrap_or('0'));
    }
    s
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let digits: Option<Vec<u32>> = s.chars().map(|c| c.to_digit(16)).collect();
    let digits = digits?;
    digits
        .chunks_exact(2)
        .map(|p| match p {
            [hi, lo] => Some(((hi << 4) | lo) as u8),
            _ => None,
        })
        .collect()
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 绝不输出私钥。
        write!(f, "DeviceKey({})", self.public_hex())
    }
}

impl DeviceKey {
    /// 生成新密钥。
    pub fn generate() -> Result<Self, TunnelError> {
        let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|e| TunnelError::Auth(e.to_string()))?;
        Self::from_pkcs8(doc.as_ref())
    }

    /// 从 PKCS#8 字节还原。
    pub fn from_pkcs8(bytes: &[u8]) -> Result<Self, TunnelError> {
        let pair =
            Ed25519KeyPair::from_pkcs8(bytes).map_err(|e| TunnelError::Auth(e.to_string()))?;
        Ok(Self {
            pair,
            pkcs8: bytes.to_vec(),
        })
    }

    /// 从十六进制的 PKCS#8 还原（持久化格式）。
    pub fn from_hex(hex: &str) -> Result<Self, TunnelError> {
        let bytes =
            from_hex(hex.trim()).ok_or_else(|| TunnelError::Auth("invalid key encoding".into()))?;
        Self::from_pkcs8(&bytes)
    }

    /// 十六进制 PKCS#8（持久化格式，注意保密）。
    pub fn to_hex(&self) -> String {
        to_hex(&self.pkcs8)
    }

    /// 十六进制公钥（交给 Controller）。
    pub fn public_hex(&self) -> String {
        to_hex(self.pair.public_key().as_ref())
    }

    /// 对登录载荷签名，返回十六进制签名。
    pub fn sign_login(&self, client_id: &str, timestamp: i64) -> String {
        to_hex(
            self.pair
                .sign(login_payload(client_id, timestamp).as_bytes())
                .as_ref(),
        )
    }
}

impl DeviceKey {
    /// 对「挑战 + 身份」签名（节点登录用：挑战由 Controller 现场生成，签名无法被重放）。
    pub fn sign_challenge(&self, id: &str, challenge: &str) -> String {
        to_hex(
            self.pair
                .sign(challenge_payload(id, challenge).as_bytes())
                .as_ref(),
        )
    }
}

fn challenge_payload(id: &str, challenge: &str) -> String {
    format!("rustfrp-node:{id}:{challenge}")
}

/// 验证挑战签名。
pub fn verify_challenge(public_hex: &str, id: &str, challenge: &str, signature_hex: &str) -> bool {
    let (Some(public), Some(sig)) = (from_hex(public_hex), from_hex(signature_hex)) else {
        return false;
    };
    UnparsedPublicKey::new(&ED25519, public)
        .verify(challenge_payload(id, challenge).as_bytes(), &sig)
        .is_ok()
}

fn login_payload(client_id: &str, timestamp: i64) -> String {
    format!("rustfrp-login:{client_id}:{timestamp}")
}

/// 用十六进制公钥验证登录签名，并限制时间偏移。
pub fn verify_login(
    public_hex: &str,
    client_id: &str,
    timestamp: i64,
    signature_hex: &str,
    now: i64,
) -> bool {
    if (now - timestamp).abs() > MAX_SKEW_SECS {
        return false;
    }
    let (Some(public), Some(sig)) = (from_hex(public_hex), from_hex(signature_hex)) else {
        return false;
    };
    UnparsedPublicKey::new(&ED25519, public)
        .verify(login_payload(client_id, timestamp).as_bytes(), &sig)
        .is_ok()
}

/// 公钥格式是否合法（32 字节十六进制）。
pub fn is_valid_public_hex(public_hex: &str) -> bool {
    from_hex(public_hex).is_some_and(|b| b.len() == 32)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let k = DeviceKey::generate().unwrap();
        let now = 1_700_000_000;
        let sig = k.sign_login("cid", now);
        assert!(verify_login(&k.public_hex(), "cid", now, &sig, now));
        assert!(verify_login(
            &k.public_hex(),
            "cid",
            now,
            &sig,
            now + MAX_SKEW_SECS
        ));
        assert!(!verify_login(
            &k.public_hex(),
            "cid",
            now,
            &sig,
            now + MAX_SKEW_SECS + 1
        ));
        assert!(!verify_login(&k.public_hex(), "other", now, &sig, now));
        assert!(!verify_login(&k.public_hex(), "cid", now + 1, &sig, now));
        let other = DeviceKey::generate().unwrap();
        assert!(!verify_login(&other.public_hex(), "cid", now, &sig, now));
    }

    #[test]
    fn challenge_signatures_bind_identity_and_nonce() {
        let k = DeviceKey::generate().unwrap();
        let sig = k.sign_challenge("node-1", "nonce-a");
        assert!(verify_challenge(&k.public_hex(), "node-1", "nonce-a", &sig));
        assert!(!verify_challenge(
            &k.public_hex(),
            "node-2",
            "nonce-a",
            &sig
        ));
        assert!(!verify_challenge(
            &k.public_hex(),
            "node-1",
            "nonce-b",
            &sig
        ));
        assert!(!verify_challenge(
            &DeviceKey::generate().unwrap().public_hex(),
            "node-1",
            "nonce-a",
            &sig
        ));
        assert!(!verify_challenge("zz", "node-1", "nonce-a", &sig));
        assert!(!verify_challenge(
            &k.public_hex(),
            "node-1",
            "nonce-a",
            "xyz"
        ));
    }

    #[test]
    fn malformed_inputs_are_rejected_not_panicking() {
        let k = DeviceKey::generate().unwrap();
        let sig = k.sign_login("c", 1);
        assert!(!verify_login("zz", "c", 1, &sig, 1));
        assert!(!verify_login(&k.public_hex(), "c", 1, "abc", 1));
        assert!(!verify_login(&k.public_hex(), "c", 1, "", 1));
        assert!(!verify_login("", "c", 1, &sig, 1));
        assert!(!is_valid_public_hex("abc"));
        assert!(!is_valid_public_hex("zz"));
        assert!(!is_valid_public_hex(&"00".repeat(31)));
        assert!(is_valid_public_hex(&k.public_hex()));
    }

    #[test]
    fn persistence_roundtrip() {
        let k = DeviceKey::generate().unwrap();
        let again = DeviceKey::from_hex(&format!("  {}\n", k.to_hex())).unwrap();
        assert_eq!(again.public_hex(), k.public_hex());
        assert!(DeviceKey::from_hex("nothex").is_err());
        assert!(DeviceKey::from_hex("abc").is_err());
        assert!(DeviceKey::from_hex("00ff").is_err());
        assert!(DeviceKey::from_pkcs8(&[1, 2, 3]).is_err());
        let shown = format!("{k:?}");
        assert!(shown.contains(&k.public_hex()) && !shown.contains(&k.to_hex()));
    }
}
