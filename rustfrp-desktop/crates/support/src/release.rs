//! The existing Ed25519 SHA256SUMS + signed RELEASE_VERSION format is shared by desktop updates.
use crate::Error;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub notes: String,
    pub page: String,
    pub assets: Vec<Asset>,
}
pub fn repository(value: &str) -> Result<(), Error> {
    let parts = value.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 100
                || matches!(*part, "." | "..")
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
    {
        return Err(Error::invalid("发布仓库未配置或格式无效"));
    }
    Ok(())
}
pub fn version(value: &str) -> Result<semver::Version, Error> {
    let value = value.trim().strip_prefix('v').unwrap_or(value.trim());
    if value.len() > 128 {
        return Err(Error::invalid("发布版本无效"));
    }
    semver::Version::parse(value).map_err(Error::invalid_context("发布版本无效"))
}
pub fn hex(value: &str, bytes: usize) -> Result<Vec<u8>, Error> {
    if value.len() != bytes * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::invalid("发布公钥或摘要格式无效"));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| Error::invalid("发布公钥格式无效"))
        })
        .collect()
}
pub fn manifest_hash(manifest: &[u8], name: &str) -> Result<String, Error> {
    if manifest.len() > 1024 * 1024 {
        return Err(Error::invalid("发布签名清单超过 1 MiB"));
    }
    let text =
        std::str::from_utf8(manifest).map_err(Error::invalid_context("签名清单不是有效文本"))?;
    let mut result = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let hash = parts.next().unwrap_or("");
        let file = parts.next().unwrap_or("").trim_start_matches('*');
        let file = file.strip_prefix("./").unwrap_or(file);
        if file == name {
            if parts.next().is_some() || result.is_some() {
                return Err(Error::invalid("签名清单条目重复或格式无效"));
            }
            hex(hash, 32)?;
            result = Some(hash.to_ascii_lowercase());
        }
    }
    result.ok_or_else(|| Error::invalid(format!("签名清单缺少 {name}")))
}
pub fn authenticate(manifest: &[u8], signature: &[u8], key: &str) -> Result<(), Error> {
    let key = hex(key, 32)?;
    if manifest.is_empty() || manifest.len() > 1024 * 1024 || signature.len() != 64 {
        return Err(Error::invalid("发布清单或签名大小无效"));
    }
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(manifest, signature)
        .map_err(Error::invalid_context("SHA256SUMS 签名无效，已拒绝安装"))
}
pub fn signed_version(manifest: &[u8], marker: &[u8], tag: &str) -> Result<(), Error> {
    if marker.is_empty()
        || marker.len() > 128
        || format!("{:x}", Sha256::digest(marker)) != manifest_hash(manifest, "RELEASE_VERSION")?
        || std::str::from_utf8(marker).ok().map(str::trim) != Some(tag)
    {
        return Err(Error::invalid("Release 标签与签名版本不符，已拒绝安装"));
    }
    Ok(())
}
pub fn newest(bytes: &[u8], repo: &str) -> Result<Option<Release>, Error> {
    repository(repo)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(Error::invalid("发布列表超过 2 MiB"));
    }
    let feed: serde_json::Value =
        serde_json::from_slice(bytes).map_err(Error::invalid_context("发布列表格式无效"))?;
    let releases = feed
        .as_array()
        .ok_or_else(|| Error::invalid("发布列表必须是数组"))?;
    if releases.len() > 100 {
        return Err(Error::invalid("发布列表超过 100 项"));
    }
    let mut newest: Option<Release> = None;
    for release in releases {
        if release.get("draft").and_then(serde_json::Value::as_bool) == Some(true)
            || release
                .get("prerelease")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        {
            continue;
        }
        let Some(tag) = release.get("tag_name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if tag.len() > 128
            || !tag
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        {
            continue;
        }
        let Ok(parsed) = version(tag) else { continue };
        if !parsed.pre.is_empty()
            || newest
                .as_ref()
                .is_some_and(|old| version(&old.version).is_ok_and(|old| parsed <= old))
        {
            continue;
        }
        let assets = release
            .get("assets")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| Error::invalid("发布资源列表缺失"))?;
        if assets.len() > 64 {
            return Err(Error::invalid("发布资源超过 64 项"));
        }
        let mut parsed_assets = Vec::new();
        for asset in assets {
            let name = asset
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| Error::invalid("发布资源名称无效"))?;
            if name.is_empty()
                || matches!(name, "." | "..")
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                || parsed_assets.iter().any(|asset: &Asset| asset.name == name)
            {
                return Err(Error::invalid("发布资源名称重复或不安全"));
            }
            parsed_assets.push(Asset {
                name: name.into(),
                size: asset
                    .get("size")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| Error::invalid("发布资源大小无效"))?,
            });
        }
        newest = Some(Release {
            tag: tag.into(),
            version: parsed.to_string(),
            notes: release
                .get("body")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .chars()
                .take(16384)
                .collect(),
            page: format!("https://github.com/{repo}/releases/tag/{tag}"),
            assets: parsed_assets,
        });
    }
    Ok(newest)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use ring::signature::KeyPair;
    #[test]
    fn manifest_signature_version_and_exact_hash_are_verified_before_installation() {
        let rng = ring::rand::SystemRandom::new();
        let key = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key = ring::signature::Ed25519KeyPair::from_pkcs8(key.as_ref()).unwrap();
        let public = key
            .public_key()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let marker = b"v1.2.3\n";
        let digest = format!("{:x}", Sha256::digest(marker));
        let manifest = format!(
            "{digest}  RELEASE_VERSION\n{} *./app.zip\n",
            "AB".repeat(32)
        );
        let signature = key.sign(manifest.as_bytes());
        authenticate(manifest.as_bytes(), signature.as_ref(), &public).unwrap();
        signed_version(manifest.as_bytes(), marker, "v1.2.3").unwrap();
        assert_eq!(
            manifest_hash(manifest.as_bytes(), "app.zip").unwrap(),
            "ab".repeat(32)
        );
        assert!(authenticate(b"tampered", signature.as_ref(), &public).is_err());
        assert!(authenticate(manifest.as_bytes(), &[0; 64], &public).is_err());
        assert!(authenticate(manifest.as_bytes(), &[], &public).is_err());
        assert!(authenticate(b"", signature.as_ref(), &public).is_err());
        assert!(authenticate(manifest.as_bytes(), signature.as_ref(), "bad key").is_err());
        assert!(signed_version(manifest.as_bytes(), marker, "v1.2.4").is_err());
        assert!(signed_version(manifest.as_bytes(), b"", "v1.2.3").is_err());
        assert!(signed_version(manifest.as_bytes(), &[b'x'; 129], "v1.2.3").is_err());
        assert!(signed_version(manifest.as_bytes(), b"v0.0.0\n", "v1.2.3").is_err());
        for malformed in [
            format!("{} app.zip\n{} app.zip", "ab".repeat(32), "ab".repeat(32)),
            "not-a-hash app.zip".into(),
            format!("{} app.zip extra", "ab".repeat(32)),
        ] {
            assert!(manifest_hash(malformed.as_bytes(), "app.zip").is_err());
        }
        assert!(manifest_hash(b"", "app.zip").is_err());
        assert!(manifest_hash(&[255], "app.zip").is_err());
        assert!(manifest_hash(&vec![0; 1024 * 1024 + 1], "app.zip").is_err());
    }
    #[test]
    fn stable_release_selection_is_numeric_and_untrusted_feed_fields_are_bounded() {
        let item = |tag: &str| serde_json::json!({"tag_name":tag,"body":"版本说明","assets":[{"name":"app.zip","size":42}]});
        let feed = serde_json::json!([item("v1.2.3"),item("v1.10.0"),item("v1.9.0"),item("bad"),item("v2.0.0-beta.1"),{"draft":true},{"prerelease":true},{}]);
        let release = newest(feed.to_string().as_bytes(), "owner/repo")
            .unwrap()
            .unwrap();
        assert_eq!(release.version, "1.10.0");
        assert_eq!(release.assets[0].size, 42);
        assert!(release.page.ends_with("v1.10.0"));
        assert!(newest(b"[]", "owner/repo").unwrap().is_none());
        assert!(newest(b"{}", "owner/repo").is_err());
        assert!(newest(b"bad", "owner/repo").is_err());
        for repo in [
            "",
            "../repo",
            "one",
            "a/b/c",
            "a/with space",
            &format!("{}/repo", "x".repeat(101)),
        ] {
            assert!(repository(repo).is_err());
        }
        assert!(hex(&"g".repeat(64), 32).is_err());
        assert!(hex(&"a".repeat(63), 32).is_err());
        for feed in [
            serde_json::json!([{ "tag_name":"v1.0.0"}]),
            serde_json::json!([{ "tag_name":"v1.0.0","assets":[{"name":"../app.zip","size":42}]}]),
            serde_json::json!([{ "tag_name":"v1.0.0","assets":[{"name":"app.zip","size":-1}]}]),
        ] {
            assert!(newest(feed.to_string().as_bytes(), "a/b").is_err());
        }
        for name in [
            String::new(),
            ".".into(),
            "..".into(),
            "bad/name".into(),
            "x".repeat(129),
        ] {
            let feed =
                serde_json::json!([{ "tag_name":"v1.0.0","assets":[{"name":name,"size":1}] }]);
            assert!(newest(feed.to_string().as_bytes(), "a/b").is_err());
        }
        assert!(version("v1.2.3").unwrap() > version("1.2.3-beta.1").unwrap());
        assert!(version(&"x".repeat(129)).is_err());
        assert!(newest(
            &serde_json::json!([{"tag_name":format!("v{}", "1".repeat(128)),"assets":[]}])
                .to_string()
                .into_bytes(),
            "a/b"
        )
        .unwrap()
        .is_none());
        assert!(newest(&vec![0; 2 * 1024 * 1024 + 1], "a/b").is_err());
        assert!(newest(
            &serde_json::to_vec(&[serde_json::json!({
                "tag_name": "v1_0_0",
                "assets": []
            })])
            .unwrap(),
            "a/b"
        )
        .unwrap()
        .is_none());
    }
    #[test]
    fn release_parser_rejects_bounded_malformed_candidates_before_selecting_a_valid_one() {
        let mut too_many = Vec::new();
        for _ in 0..101 {
            too_many.push(serde_json::json!({
                "tag_name": "v1.0.0",
                "assets": []
            }));
        }
        assert!(newest(serde_json::to_vec(&too_many).unwrap().as_slice(), "a/b").is_err());
        for release in [
            serde_json::json!({"tag_name":"v1.0.0","assets":[{}]}),
            serde_json::json!({"tag_name":"v1.0.0","assets":[{"name":"app.zip"}]}),
            serde_json::json!({"tag_name":"v1.0.0","assets":[{"name":"app.zip","size":1},{"name":"app.zip","size":2}]}),
            serde_json::json!({"tag_name":"v1.0.0","assets":[{"name":"app/zip","size":1}]}),
        ] {
            assert!(newest(serde_json::to_vec(&[release]).unwrap().as_slice(), "a/b").is_err());
        }
        let mut too_many_assets = Vec::new();
        for index in 0..65 {
            too_many_assets.push(serde_json::json!({"name":format!("asset-{index}"),"size":1}));
        }
        assert!(newest(
            serde_json::to_vec(&[
                serde_json::json!({"tag_name":"v1.0.0","assets":too_many_assets})
            ])
            .unwrap()
            .as_slice(),
            "a/b"
        )
        .is_err());
        assert!(newest(b"[]", "invalid/repo/").is_err());
    }
}
