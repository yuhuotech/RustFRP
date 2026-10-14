use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(env::var("OUT_DIR")?).join("assets.rs");
    let mut generated = String::from("pub static ASSETS: &[Asset] = &[\n");
    if env::var_os("CARGO_FEATURE_EMBEDDED").is_some() {
        let dist = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?)
            .join("../rustfrp-ui/dist")
            .canonicalize()?;
        let manifest_path = dist.join("ui-manifest.json").canonicalize()?;
        if !manifest_path.starts_with(&dist) {
            return Err("UI manifest escapes the build directory".into());
        }
        println!("cargo:rerun-if-changed={}", manifest_path.display());
        let manifest_bytes = fs::read(&manifest_path)?;
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
        if manifest.get("api").and_then(serde_json::Value::as_str) != Some("v1") {
            return Err("UI manifest does not support API v1".into());
        }
        let entries = manifest
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .ok_or("Missing UI assets")?;
        if entries.is_empty() || entries.len() > 256 {
            return Err("UI asset count exceeds bounds".into());
        }
        let mut names = HashSet::new();
        let mut total = 0u64;
        for entry in entries {
            let name = entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or("Missing asset path")?;
            if name.len() > 2048
                || name.starts_with('/')
                || name == "ui-manifest.json"
                || !names.insert(name)
                || name.split('/').any(|part| {
                    part.is_empty() || matches!(part, "." | "..") || part.contains('\\')
                })
            {
                return Err("Unsafe UI asset path".into());
            }
            let hash = entry
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .ok_or("Missing asset hash")?;
            let raw = dist.join(name).canonicalize()?;
            let gzip = dist.join(format!("{name}.gz")).canonicalize()?;
            let brotli = dist.join(format!("{name}.br")).canonicalize()?;
            for path in [&raw, &gzip, &brotli] {
                if !path.starts_with(&dist) {
                    return Err("UI asset escapes the build directory".into());
                }
                println!("cargo:rerun-if-changed={}", path.display());
            }
            if fs::metadata(&raw)?.len() > 16 * 1024 * 1024 {
                return Err("UI asset exceeds the build budget".into());
            }
            let bytes = fs::read(&raw)?;
            if format!("{:x}", Sha256::digest(&bytes)) != hash {
                return Err("UI asset digest mismatch".into());
            }
            for (path, size) in [
                (&raw, "bytes"),
                (&gzip, "gzip_bytes"),
                (&brotli, "brotli_bytes"),
            ] {
                let expected = entry
                    .get(size)
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("Missing UI asset size")?;
                if fs::metadata(path)?.len() != expected {
                    return Err("UI asset size mismatch".into());
                }
                total = total
                    .checked_add(expected)
                    .ok_or("UI asset size overflow")?;
                if total > 16 * 1024 * 1024 {
                    return Err("Embedded UI exceeds the asset budget".into());
                }
            }
            for (path, field) in [(&gzip, "gzip_sha256"), (&brotli, "brotli_sha256")] {
                let expected = entry
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .ok_or("Missing compressed asset hash")?;
                if format!("{:x}", Sha256::digest(fs::read(path)?)) != expected {
                    return Err("Compressed UI asset digest mismatch".into());
                }
            }
            generated.push_str(&format!("Asset {{ path: {name:?}, bytes: include_bytes!({raw:?}), gzip: include_bytes!({gzip:?}), brotli: include_bytes!({brotli:?}), hash: {hash:?} }},\n"));
        }
        let raw = manifest_path.canonicalize()?;
        let hash = format!("{:x}", Sha256::digest(&manifest_bytes));
        generated.push_str(&format!("Asset {{ path: \"ui-manifest.json\", bytes: include_bytes!({raw:?}), gzip: &[], brotli: &[], hash: {hash:?} }},\n"));
    }
    generated.push_str("];\n");
    fs::write(out, generated)?;
    Ok(())
}
