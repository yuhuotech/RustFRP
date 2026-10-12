//! 受 Controller 命令驱动的 Agent 更新器：先验 Ed25519 清单，再校验二进制 SHA-256，最后同目录替换。

use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_MANIFEST: u64 = 1024 * 1024;
const MAX_SIGNATURE: u64 = 1024;
const MAX_VERSION_MARKER: u64 = 128;
const MAX_AGENT: u64 = 128 * 1024 * 1024;

/// Controller 指定版本后下载并应用 Agent。返回 `false` 表示当前版本已经是目标版本。
pub async fn update_agent(version: String) -> Result<bool, String> {
    tokio::task::spawn_blocking(move || update_agent_blocking(&version))
        .await
        .map_err(|e| format!("agent updater worker failed: {e}"))?
}

fn update_agent_blocking(version: &str) -> Result<bool, String> {
    let current = option_env!("RUSTFRP_RELEASE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    update_agent_with_config(
        version,
        current,
        option_env!("RUSTFRP_RELEASE_REPOSITORY"),
        option_env!("RUSTFRP_RELEASE_PUBLIC_KEY_HEX"),
        std::env::current_exe,
        download,
        replace_running_binary,
    )
}

fn update_agent_with_config(
    version: &str,
    current: &str,
    repository: Option<&str>,
    public_key: Option<&str>,
    locate_binary: impl FnOnce() -> std::io::Result<PathBuf>,
    download: impl Fn(&str, &Path, u64) -> Result<(), String>,
    replace: impl Fn(&Path, &Path) -> Result<(), String>,
) -> Result<bool, String> {
    let plan = update_plan(version, current, repository, public_key)?;
    let Some(plan) = plan else {
        return Ok(false);
    };
    let parent = locate_binary().map_err(|e| format!("locate running Agent: {e}"))?;
    perform_update(&parent, &plan, download, replace)?;
    Ok(true)
}

fn perform_update(
    target: &Path,
    plan: &UpdatePlan,
    download: impl Fn(&str, &Path, u64) -> Result<(), String>,
    replace: impl Fn(&Path, &Path) -> Result<(), String>,
) -> Result<(), String> {
    let manifest_path = temp_path(target, "manifest");
    let signature_path = temp_path(target, "signature");
    let staged_path = temp_path(target, "agent");
    let mut cleanup = TempFiles {
        paths: vec![
            manifest_path.clone(),
            signature_path.clone(),
            staged_path.clone(),
        ],
        keep_staged: false,
    };

    download(
        &format!("{}/SHA256SUMS", plan.base),
        &manifest_path,
        MAX_MANIFEST,
    )?;
    download(
        &format!("{}/SHA256SUMS.sig", plan.base),
        &signature_path,
        MAX_SIGNATURE,
    )?;
    let version_path = temp_path(target, "version");
    cleanup.paths.insert(2, version_path.clone());
    let manifest = fs::read(&manifest_path).map_err(|e| format!("read signed manifest: {e}"))?;
    let signature =
        fs::read(&signature_path).map_err(|e| format!("read manifest signature: {e}"))?;
    verify_manifest(&manifest, &signature, &plan.key)?;
    let expected_version_hash = manifest_hash(&manifest, "RELEASE_VERSION")?;
    download(
        &format!("{}/RELEASE_VERSION", plan.base),
        &version_path,
        MAX_VERSION_MARKER,
    )?;
    if sha256_file(&version_path)? != expected_version_hash {
        return Err("signed Release version marker does not match its manifest".into());
    }
    let signed_version = fs::read_to_string(&version_path)
        .map_err(|e| format!("read signed Release version: {e}"))?;
    if signed_version.trim() != plan.version {
        return Err("requested Release tag does not match the signed version marker".into());
    }
    let expected = manifest_hash(&manifest, &plan.asset)?;
    download(
        &format!("{}/{}", plan.base, plan.asset),
        &staged_path,
        MAX_AGENT,
    )?;
    let actual = sha256_file(&staged_path)?;
    if actual != expected {
        return Err("downloaded Agent does not match the signed SHA256 manifest".into());
    }
    replace(target, &staged_path)?;
    cleanup.keep_staged = cfg!(windows);
    Ok(())
}

#[derive(Debug)]
struct UpdatePlan {
    version: String,
    base: String,
    asset: String,
    key: Vec<u8>,
}

fn update_plan(
    version: &str,
    current: &str,
    repository: Option<&str>,
    public_key: Option<&str>,
) -> Result<Option<UpdatePlan>, String> {
    validate_tag(version)?;
    if version.trim_start_matches('v') == current.trim_start_matches('v') {
        return Ok(None);
    }
    let repository = repository.ok_or_else(|| {
        "Agent update is disabled: release repository was not compiled in".to_string()
    })?;
    let public_key = public_key.ok_or_else(|| {
        "Agent update is disabled: trusted release key was not compiled in".to_string()
    })?;
    validate_repository(repository)?;
    let key = parse_hex(public_key, 32)?;
    let asset = agent_asset_name()?;
    Ok(Some(UpdatePlan {
        version: version.to_string(),
        base: format!("https://github.com/{repository}/releases/download/{version}"),
        asset,
        key,
    }))
}

fn validate_tag(tag: &str) -> Result<(), String> {
    if tag.len() < 2
        || tag.len() > 64
        || !tag.starts_with('v')
        || tag
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || ".-_".contains(c)))
    {
        return Err("update version must be a release tag such as v1.2.3".into());
    }
    Ok(())
}

fn validate_repository(repository: &str) -> Result<(), String> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    let valid = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
    };
    if !valid(owner) || !valid(name) || parts.next().is_some() {
        return Err("invalid compiled release repository".into());
    }
    Ok(())
}

fn agent_asset_name() -> Result<String, String> {
    agent_asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn agent_asset_name_for(os: &str, arch: &str) -> Result<String, String> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("rustfrp-agent-linux-x86_64".into()),
        ("macos", "x86_64") => Ok("rustfrp-agent-macos-x86_64".into()),
        ("macos", "aarch64") => Ok("rustfrp-agent-macos-arm64".into()),
        ("windows", "x86_64") => Ok("rustfrp-agent-windows-x86_64.exe".into()),
        _ => Err(format!("Agent updates are not published for {os}/{arch}")),
    }
}

fn download(url: &str, path: &Path, max_bytes: u64) -> Result<(), String> {
    download_with(OsStr::new("curl"), url, path, max_bytes)
}

fn download_with(program: &OsStr, url: &str, path: &Path, max_bytes: u64) -> Result<(), String> {
    let limit = max_bytes.to_string();
    let output = Command::new(program)
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--connect-timeout",
            "15",
            "--max-time",
            "300",
            "--max-filesize",
            &limit,
            "--output",
        ])
        .arg(path)
        .arg(url)
        .output()
        .map_err(|e| format!("start curl for Agent update: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Agent update download failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let size = fs::metadata(path)
        .map_err(|e| format!("inspect downloaded file: {e}"))?
        .len();
    if size == 0 || size > max_bytes {
        return Err("downloaded update file is empty or exceeds its size limit".into());
    }
    Ok(())
}

fn parse_hex(text: &str, expected: usize) -> Result<Vec<u8>, String> {
    if text.len() != expected.saturating_mul(2) {
        return Err("compiled release public key has the wrong size".into());
    }
    let mut bytes = Vec::with_capacity(expected);
    for pair in text.as_bytes().chunks_exact(2) {
        let item =
            std::str::from_utf8(pair).map_err(|_| "compiled release key is invalid".to_string())?;
        bytes.push(
            u8::from_str_radix(item, 16)
                .map_err(|_| "compiled release key is invalid".to_string())?,
        );
    }
    Ok(bytes)
}

fn verify_manifest(manifest: &[u8], signature: &[u8], key: &[u8]) -> Result<(), String> {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(manifest, signature)
        .map_err(|_| "release manifest signature is invalid".to_string())
}

fn manifest_hash(manifest: &[u8], asset: &str) -> Result<String, String> {
    for line in manifest.split(|b| *b == b'\n') {
        let line =
            std::str::from_utf8(line).map_err(|_| "signed manifest is not UTF-8".to_string())?;
        let mut fields = line.split_whitespace();
        let hash = fields.next().unwrap_or_default();
        let file = fields.next().unwrap_or_default();
        if file.strip_prefix("./").unwrap_or(file) == asset {
            if fields.next().is_some()
                || hash.len() != 64
                || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("signed manifest entry is malformed".into());
            }
            return Ok(hash.to_ascii_lowercase());
        }
    }
    Err(format!("signed manifest does not contain {asset}"))
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("read downloaded Agent: {error}"))?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read downloaded Agent: {error}"))?;
        if count == 0 {
            break;
        }
        let bytes = buffer
            .get(..count)
            .ok_or_else(|| "read downloaded Agent: invalid buffer length".to_string())?;
        context.update(bytes);
    }
    Ok(encode_hex(context.finish().as_ref()))
}

fn temp_path(target: &Path, suffix: &str) -> PathBuf {
    let mut path = target.as_os_str().to_os_string();
    path.push(format!(".update-{}-{suffix}", std::process::id()));
    PathBuf::from(path)
}

struct TempFiles {
    paths: Vec<PathBuf>,
    keep_staged: bool,
}
impl Drop for TempFiles {
    fn drop(&mut self) {
        for (index, path) in self.paths.iter().enumerate() {
            if self.keep_staged && index + 1 == self.paths.len() {
                continue;
            }
            let _ = fs::remove_file(path);
        }
    }
}

fn replace_running_binary(target: &Path, staged: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let target_text = powershell_quote(target)?;
        let stage_text = powershell_quote(staged)?;
        let pid = std::process::id();
        let arguments = std::env::args().skip(1).collect::<Vec<_>>();
        let argument_json = serde_json::to_vec(&arguments)
            .map_err(|e| format!("encode Agent restart arguments: {e}"))?;
        let argument_hex = encode_hex(&argument_json);
        let script = format!(
            "$ErrorActionPreference='Stop'; Wait-Process -Id {pid}; $target='{target_text}'; $stage='{stage_text}'; $backup=$target+'.rollback'; $hex='{argument_hex}'; $bytes=New-Object byte[] ($hex.Length/2); for($i=0;$i -lt $bytes.Length;$i++){{$bytes[$i]=[Convert]::ToByte($hex.Substring($i*2,2),16)}}; $agentArgs=ConvertFrom-Json ([Text.Encoding]::UTF8.GetString($bytes)); if(Test-Path -LiteralPath $backup){{Remove-Item -LiteralPath $backup -Force}}; Move-Item -LiteralPath $target -Destination $backup -Force; try{{Move-Item -LiteralPath $stage -Destination $target -Force; Remove-Item -LiteralPath $backup -Force; Start-Process -FilePath $target -ArgumentList $agentArgs}}catch{{Move-Item -LiteralPath $backup -Destination $target -Force; throw}}"
        );
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &script,
            ])
            .spawn()
            .map_err(|e| format!("start Agent replacement helper: {e}"))?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        replace_running_binary_with(
            target,
            staged,
            |from, to| fs::rename(from, to),
            |path| fs::remove_file(path),
        )
    }
}

#[cfg(not(windows))]
fn replace_running_binary_with(
    target: &Path,
    staged: &Path,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
    remove: impl Fn(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let metadata = fs::metadata(target).map_err(|e| format!("inspect current Agent: {e}"))?;
    fs::set_permissions(staged, metadata.permissions())
        .map_err(|e| format!("preserve Agent permissions: {e}"))?;
    let backup = temp_path(target, "rollback");
    rename(target, &backup).map_err(|e| format!("stage current Agent for rollback: {e}"))?;
    if let Err(error) = rename(staged, target) {
        let restore = rename(&backup, target);
        return Err(match restore {
            Ok(()) => format!("replace Agent failed; previous binary restored: {error}"),
            Err(restore_error) => {
                format!("replace Agent failed ({error}); rollback failed ({restore_error})")
            }
        });
    }
    let _ = remove(&backup);
    Ok(())
}

#[cfg(windows)]
fn powershell_quote(path: &Path) -> Result<String, String> {
    let text = path.to_str().ok_or("Agent path is not valid Unicode")?;
    Ok(text.replace('\'', "''"))
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use ring::signature::KeyPair;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_dir() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let path =
            std::env::temp_dir().join(format!("rustfrp-update-{}-{suffix}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn validates_tags_repositories_and_signed_manifest_entries() {
        assert!(validate_tag("v1.2.3").is_ok());
        assert!(validate_tag("../latest").is_err());
        assert!(validate_repository("owner/repo").is_ok());
        assert!(validate_repository("owner/repo/path").is_err());
        let manifest = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  rustfrp-agent-linux-x86_64\n";
        assert!(manifest_hash(manifest, "rustfrp-agent-linux-x86_64").is_ok());
        assert!(manifest_hash(manifest, "missing").is_err());
    }

    #[test]
    fn rejects_bad_release_signatures_and_accepts_ed25519_manifest() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let manifest = b"signed release list";
        let signature = pair.sign(manifest);
        assert!(verify_manifest(manifest, signature.as_ref(), pair.public_key().as_ref()).is_ok());
        assert!(
            verify_manifest(b"tampered", signature.as_ref(), pair.public_key().as_ref()).is_err()
        );
    }

    #[test]
    fn preflight_is_fail_closed_and_builds_platform_release_plans() {
        let key = "12".repeat(32);
        assert!(update_plan("v0.1.0", "0.1.0", None, None)
            .unwrap()
            .is_none());
        assert!(!update_agent_blocking("v0.1.0").unwrap());
        assert!(update_plan("latest", "0.1.0", None, None)
            .unwrap_err()
            .contains("release tag"));
        assert!(update_plan("v1.2.3", "0.1.0", None, Some(&key))
            .unwrap_err()
            .contains("repository was not compiled"));
        assert!(update_plan("v1.2.3", "0.1.0", Some("owner/repo"), None)
            .unwrap_err()
            .contains("trusted release key"));
        assert!(
            update_plan("v1.2.3", "0.1.0", Some("owner/repo/other"), Some(&key))
                .unwrap_err()
                .contains("repository")
        );
        assert!(
            update_plan("v1.2.3", "0.1.0", Some("owner/repo"), Some("not-hex"))
                .unwrap_err()
                .contains("wrong size")
        );
        assert!(update_plan(
            "v1.2.3",
            "0.1.0",
            Some("owner/repo"),
            Some(&"zz".repeat(32))
        )
        .unwrap_err()
        .contains("invalid"));

        let plan = update_plan("v1.2.3", "0.1.0", Some("owner/repo"), Some(&key))
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.base,
            "https://github.com/owner/repo/releases/download/v1.2.3"
        );
        assert_eq!(plan.version, "v1.2.3");
        assert_eq!(plan.key, vec![0x12; 32]);

        assert_eq!(
            agent_asset_name_for("linux", "x86_64").unwrap(),
            "rustfrp-agent-linux-x86_64"
        );
        assert_eq!(
            agent_asset_name_for("macos", "x86_64").unwrap(),
            "rustfrp-agent-macos-x86_64"
        );
        assert_eq!(
            agent_asset_name_for("macos", "aarch64").unwrap(),
            "rustfrp-agent-macos-arm64"
        );
        assert_eq!(
            agent_asset_name_for("windows", "x86_64").unwrap(),
            "rustfrp-agent-windows-x86_64.exe"
        );
        assert!(agent_asset_name_for("linux", "arm").is_err());
    }

    #[test]
    fn manifest_and_key_parsers_reject_malformed_or_ambiguous_entries() {
        assert!(validate_tag("v").is_err());
        assert!(validate_tag(&format!("v{}", "a".repeat(64))).is_err());
        assert!(validate_tag("v1.2.3/evil").is_err());
        for invalid in ["/repo", "owner/", "owner/repo/extra", "owner/repo?query"] {
            assert!(validate_repository(invalid).is_err(), "{invalid}");
        }
        assert_eq!(parse_hex("00ff", 2).unwrap(), vec![0, 255]);
        assert!(parse_hex("0", 1).is_err());
        assert!(parse_hex("gg", 1).is_err());
        assert!(parse_hex("aéa", 2).is_err());

        let hash = "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789";
        let valid = format!("{hash}  asset.bin\n");
        assert_eq!(
            manifest_hash(valid.as_bytes(), "asset.bin").unwrap(),
            hash.to_lowercase()
        );
        assert!(manifest_hash(b"\xff\n", "asset.bin").is_err());
        assert!(manifest_hash(b"short  asset.bin\n", "asset.bin").is_err());
        assert!(manifest_hash(
            b"gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg  asset.bin\n",
            "asset.bin"
        )
        .is_err());
        assert!(manifest_hash(
            format!("{hash}  asset.bin  extra\n").as_bytes(),
            "asset.bin"
        )
        .is_err());
        assert!(manifest_hash(valid.as_bytes(), "other.bin").is_err());
    }

    #[test]
    fn temporary_files_cleanup_and_local_replacement_keep_rollback_safety() {
        let root = temporary_dir();
        let manifest = root.join("manifest");
        let signature = root.join("signature");
        let staged = root.join("agent.staged");
        for path in [&manifest, &signature, &staged] {
            fs::write(path, b"temporary").unwrap();
        }
        {
            let _cleanup = TempFiles {
                paths: vec![manifest.clone(), signature.clone(), staged.clone()],
                keep_staged: true,
            };
        }
        assert!(!manifest.exists());
        assert!(!signature.exists());
        assert!(staged.exists());
        fs::remove_file(&staged).unwrap();

        #[cfg(not(windows))]
        {
            let target = root.join("rustfrp-agent");
            let replacement = root.join("replacement");
            fs::write(&target, b"old binary").unwrap();
            fs::write(&replacement, b"new binary").unwrap();
            replace_running_binary(&target, &replacement).unwrap();
            assert_eq!(fs::read(&target).unwrap(), b"new binary");
            assert!(!replacement.exists());

            let target = root.join("preserved");
            fs::write(&target, b"still running").unwrap();
            let missing = root.join("missing-stage");
            assert!(replace_running_binary(&target, &missing).is_err());
            assert_eq!(fs::read(&target).unwrap(), b"still running");

            let missing_target = root.join("missing-agent");
            let staged = root.join("orphan-stage");
            fs::write(&staged, b"new").unwrap();
            assert!(replace_running_binary(&missing_target, &staged).is_err());
            assert!(staged.exists());
        }

        {
            let _cleanup = TempFiles {
                paths: vec![manifest.clone(), signature.clone(), staged.clone()],
                keep_staged: false,
            };
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn curl_download_wrapper_handles_process_status_and_size_limits() {
        use std::os::unix::fs::PermissionsExt;

        let root = temporary_dir();
        let script = root.join("curl-ok");
        fs::write(
            &script,
            "#!/bin/sh\nwhile [ \"$1\" != \"--output\" ]; do shift; done\nshift\nprintf body > \"$1\"\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let output = root.join("download");
        assert!(download_with(script.as_os_str(), "https://example.invalid/a", &output, 8).is_ok());
        assert_eq!(fs::read(&output).unwrap(), b"body");
        assert!(
            download_with(script.as_os_str(), "https://example.invalid/a", &output, 2)
                .unwrap_err()
                .contains("size limit")
        );

        let failing = root.join("curl-fail");
        fs::write(&failing, "#!/bin/sh\necho network-error >&2\nexit 22\n").unwrap();
        fs::set_permissions(&failing, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            download_with(failing.as_os_str(), "https://example.invalid/a", &output, 8)
                .unwrap_err()
                .contains("network-error")
        );
        assert!(download_with(
            root.join("not-found").as_os_str(),
            "https://example.invalid/a",
            &output,
            8
        )
        .unwrap_err()
        .contains("start curl"));
        let empty = root.join("curl-empty");
        fs::write(
            &empty,
            "#!/bin/sh\nwhile [ \"$1\" != \"--output\" ]; do shift; done\nshift\n: > \"$1\"\n",
        )
        .unwrap();
        fs::set_permissions(&empty, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            download_with(empty.as_os_str(), "https://example.invalid/a", &output, 8)
                .unwrap_err()
                .contains("empty")
        );
        let _ = fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use ring::signature::KeyPair;
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn apply_fixture(
        target: &Path,
        plan: &UpdatePlan,
        files: HashMap<String, Vec<u8>>,
    ) -> Result<(), String> {
        let download = |url: &str, path: &Path, max: u64| {
            let name = url.rsplit('/').next().unwrap_or_default();
            let bytes = files
                .get(name)
                .ok_or_else(|| format!("fixture is missing {name}"))?;
            if bytes.len() as u64 > max {
                return Err("fixture exceeds the configured size limit".into());
            }
            fs::write(path, bytes).map_err(|error| error.to_string())
        };
        #[cfg(not(windows))]
        let replace = replace_running_binary;
        #[cfg(windows)]
        let replace = |target: &Path, staged: &Path| {
            fs::copy(staged, target)
                .map(|_| ())
                .map_err(|error| error.to_string())
        };
        let tag = plan.base.rsplit('/').next().unwrap_or_default();
        let public_key = encode_hex(&plan.key);
        match update_agent_with_config(
            tag,
            "v0.0.0",
            Some("owner/repo"),
            Some(&public_key),
            || Ok(target.to_path_buf()),
            download,
            replace,
        )? {
            true => Ok(()),
            false => Err("fixture tag unexpectedly matched current version".into()),
        }
    }

    #[tokio::test]
    async fn async_entry_uses_the_worker_and_rejects_invalid_tags() {
        let current = option_env!("RUSTFRP_RELEASE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
        assert!(!update_agent(format!("v{current}")).await.unwrap());
        assert!(update_agent("latest".into())
            .await
            .unwrap_err()
            .contains("release tag"));
    }

    #[test]
    fn updater_reports_running_binary_lookup_failures_before_download() {
        let error = update_agent_with_config(
            "v9.8.7",
            "v0.0.0",
            Some("owner/repo"),
            Some(&"12".repeat(32)),
            || Err(std::io::Error::other("executable unavailable")),
            |_, _, _| Ok(()),
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("locate running Agent"));
        assert!(error.contains("executable unavailable"));
    }

    #[test]
    fn signed_update_pipeline_checks_signature_digest_and_rolls_back_before_replace() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let signer = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let key = encode_hex(signer.public_key().as_ref());
        let current = option_env!("RUSTFRP_RELEASE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
        let plan = update_plan("v9.8.7", current, Some("owner/repo"), Some(&key))
            .unwrap()
            .unwrap();
        let binary = b"new signed Agent";
        let version_marker = format!("{}\n", plan.version);
        let hash = ring::digest::digest(&ring::digest::SHA256, binary);
        let version_hash = ring::digest::digest(&ring::digest::SHA256, version_marker.as_bytes());
        let manifest = format!(
            "{}  RELEASE_VERSION\n{}  {}\n",
            encode_hex(version_hash.as_ref()),
            encode_hex(hash.as_ref()),
            plan.asset
        );
        let signature = signer.sign(manifest.as_bytes());
        let base_files = HashMap::from([
            ("SHA256SUMS".to_string(), manifest.as_bytes().to_vec()),
            ("SHA256SUMS.sig".to_string(), signature.as_ref().to_vec()),
            ("RELEASE_VERSION".to_string(), version_marker.into_bytes()),
            (plan.asset.clone(), binary.to_vec()),
        ]);
        let root = std::env::temp_dir().join(format!(
            "rustfrp-update-pipeline-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        let target = root.join("agent");
        fs::write(&target, b"old Agent").unwrap();
        apply_fixture(&target, &plan, base_files.clone()).unwrap();
        assert_eq!(fs::read(&target).unwrap(), binary);

        let tampered_target = root.join("tampered-signature");
        fs::write(&tampered_target, b"old Agent").unwrap();
        let mut bad_signature = base_files.clone();
        bad_signature.insert("SHA256SUMS.sig".into(), vec![0; 64]);
        assert!(apply_fixture(&tampered_target, &plan, bad_signature)
            .unwrap_err()
            .contains("signature"));
        assert_eq!(fs::read(&tampered_target).unwrap(), b"old Agent");

        let manifest_target = root.join("wrong-version-digest");
        fs::write(&manifest_target, b"old Agent").unwrap();
        let bad_version_hash = format!(
            "{}  RELEASE_VERSION\n{}  {}\n",
            "00".repeat(32),
            encode_hex(hash.as_ref()),
            plan.asset
        );
        let bad_version_signature = signer.sign(bad_version_hash.as_bytes());
        let mut wrong_version_digest = HashMap::from([
            ("SHA256SUMS".to_string(), bad_version_hash.into_bytes()),
            (
                "SHA256SUMS.sig".to_string(),
                bad_version_signature.as_ref().to_vec(),
            ),
            (
                "RELEASE_VERSION".to_string(),
                format!("{}\n", plan.version).into_bytes(),
            ),
            (plan.asset.clone(), binary.to_vec()),
        ]);
        assert!(
            apply_fixture(&manifest_target, &plan, wrong_version_digest.clone())
                .unwrap_err()
                .contains("version marker does not match")
        );
        assert_eq!(fs::read(&manifest_target).unwrap(), b"old Agent");

        let tag_target = root.join("wrong-version-tag");
        fs::write(&tag_target, b"old Agent").unwrap();
        let wrong_marker = format!("{}-other\n", plan.version);
        let wrong_marker_hash =
            ring::digest::digest(&ring::digest::SHA256, wrong_marker.as_bytes());
        let wrong_marker_manifest = format!(
            "{}  RELEASE_VERSION\n{}  {}\n",
            encode_hex(wrong_marker_hash.as_ref()),
            encode_hex(hash.as_ref()),
            plan.asset
        );
        let wrong_marker_signature = signer.sign(wrong_marker_manifest.as_bytes());
        wrong_version_digest.insert("SHA256SUMS".into(), wrong_marker_manifest.into_bytes());
        wrong_version_digest.insert(
            "SHA256SUMS.sig".into(),
            wrong_marker_signature.as_ref().to_vec(),
        );
        wrong_version_digest.insert("RELEASE_VERSION".into(), wrong_marker.into_bytes());
        assert!(apply_fixture(&tag_target, &plan, wrong_version_digest)
            .unwrap_err()
            .contains("does not match the signed version marker"));
        assert_eq!(fs::read(&tag_target).unwrap(), b"old Agent");

        let mismatch_target = root.join("bad-digest");
        fs::write(&mismatch_target, b"old Agent").unwrap();
        let mut bad_binary = base_files;
        bad_binary.insert(plan.asset.clone(), b"wrong bytes".to_vec());
        assert!(apply_fixture(&mismatch_target, &plan, bad_binary)
            .unwrap_err()
            .contains("SHA256"));
        assert_eq!(fs::read(&mismatch_target).unwrap(), b"old Agent");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(windows))]
    #[test]
    fn replacement_reports_successful_and_failed_rollback_paths() {
        let root = std::env::temp_dir().join(format!(
            "rustfrp-rollback-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        let target = root.join("agent-restored");
        let staged = root.join("stage-restored");
        fs::write(&target, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();
        let mut rename_count = 0;
        let result = replace_running_binary_with(
            &target,
            &staged,
            |from, to| {
                rename_count += 1;
                if rename_count == 2 {
                    Err(std::io::Error::other("replacement blocked"))
                } else {
                    fs::rename(from, to)
                }
            },
            |path| fs::remove_file(path),
        );
        assert!(result.unwrap_err().contains("previous binary restored"));
        assert_eq!(fs::read(&target).unwrap(), b"old");

        let target = root.join("agent-rollback-failed");
        let staged = root.join("stage-rollback-failed");
        fs::write(&target, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();
        let mut rename_count = 0;
        let result = replace_running_binary_with(
            &target,
            &staged,
            |from, to| {
                rename_count += 1;
                if rename_count >= 2 {
                    Err(std::io::Error::other("replacement blocked"))
                } else {
                    fs::rename(from, to)
                }
            },
            |path| fs::remove_file(path),
        );
        assert!(result.unwrap_err().contains("rollback failed"));
        assert!(temp_path(&target, "rollback").exists());
        let _ = fs::remove_dir_all(root);
    }
}
