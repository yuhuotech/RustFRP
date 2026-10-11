//! 密码哈希（Argon2id）、随机令牌与令牌哈希。

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use sha2::{Digest, Sha256};

/// 密码最短长度。
pub const MIN_PASSWORD_LEN: usize = 10;

// 内存纪律：16 MiB / 3 次迭代 / 单线程（OWASP 推荐的最低档附近，且登录是低频操作）。
fn hasher() -> Argon2<'static> {
    let params = Params::new(16 * 1024, 3, 1, None).unwrap_or_default();
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 0x0f), 16).unwrap_or('0'));
    }
    s
}

/// 检查密码强度（长度）。
pub fn check_password(pw: &str) -> Result<(), String> {
    if pw.chars().count() < MIN_PASSWORD_LEN {
        return Err(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        ));
    }
    Ok(())
}

/// 计算密码哈希（PHC 字符串）。
pub fn hash_password(pw: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| e.to_string())?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    hasher()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// 校验密码。哈希格式非法时一律视为不匹配。
pub fn verify_password(pw: &str, phc: &str) -> bool {
    PasswordHash::new(phc)
        .is_ok_and(|parsed| hasher().verify_password(pw.as_bytes(), &parsed).is_ok())
}

/// 生成 256 位随机令牌（十六进制，带 `rfrp_` 前缀便于识别泄露）。
pub fn new_token() -> Result<String, String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    Ok(format!("rfrp_{}", hex(&b)))
}

/// 令牌哈希（SHA-256，十六进制）；库里只存它。
pub fn token_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let h = hash_password("correct horse battery").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "not a phc string"));
        assert_ne!(h, hash_password("correct horse battery").unwrap(), "salted");
    }

    #[test]
    fn password_policy() {
        assert!(check_password("short").is_err());
        assert!(check_password("0123456789").is_ok());
        assert!(check_password("密码密码密码密码密码密码").is_ok());
    }

    #[test]
    fn tokens() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert!(a.starts_with("rfrp_") && a.len() == 5 + 64);
        assert_ne!(a, b);
        assert_eq!(token_hash("x").len(), 64);
        assert_eq!(token_hash("x"), token_hash("x"));
        assert_ne!(token_hash("x"), token_hash("y"));
        assert_eq!(
            token_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
