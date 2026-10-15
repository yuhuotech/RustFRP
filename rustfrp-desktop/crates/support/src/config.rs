//! Complete frpc editing uses the Agent's parser and validation, then a private atomic replacement.
use crate::Error;
use rustfrp_core::frpcfg::{self, ConfigFs};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_CONFIG: usize = 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn file_bytes(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::io("配置文件不可读")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::io_context("读取配置失败"))?;
    if bytes.len() > MAX_CONFIG {
        return Err(Error::invalid("配置文件超过 1 MiB"));
    }
    Ok(Some(bytes))
}
fn ensure_original(path: &Path, original: &str) -> Result<(), Error> {
    if file_bytes(path)?.map_or_else(String::new, |bytes| hash(&bytes)) != original {
        return Err(Error::invalid("配置保存期间文件已变化，请重新读取"));
    }
    Ok(())
}
fn check_path(path: &Path) -> Result<(), Error> {
    if !path.is_absolute() || path.as_os_str().len() > 4096 || path.file_name().is_none() {
        return Err(Error::invalid("配置文件须使用绝对路径"));
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::invalid("请选择普通配置文件，不能覆盖目录或符号链接"));
        }
    }
    Ok(())
}
pub fn read(path: &Path) -> Result<Value, Error> {
    check_path(path)?;
    match file_bytes(path)? {
        None => Ok(
            json!({"exists":false,"text":"# RustFRP 独立模式配置\n# 填写 serverAddr/serverPort 与代理；保存前会验证配置与兼容报告。\n","digest":""}),
        ),
        Some(bytes) => {
            let digest = hash(&bytes);
            let text = String::from_utf8(bytes)
                .map_err(Error::invalid_context("配置文件不是 UTF-8 文本"))?;
            Ok(json!({"exists":true,"text":text,"digest":digest}))
        }
    }
}
struct Overlay<'a> {
    path: &'a str,
    text: &'a str,
    size: RefCell<usize>,
    digest: RefCell<Sha256>,
}
impl ConfigFs for Overlay<'_> {
    fn read_to_string(&self, path: &str) -> std::io::Result<String> {
        let content = if path == self.path {
            self.text.to_owned()
        } else {
            let file = std::fs::File::open(path)?;
            let mut content = String::new();
            file.take(MAX_CONFIG as u64 + 1)
                .read_to_string(&mut content)?;
            content
        };
        if content.len() > MAX_CONFIG {
            return Err(std::io::Error::other(
                "included configuration exceeds 1 MiB",
            ));
        }
        let mut size = self.size.borrow_mut();
        *size += content.len();
        if *size > 4 * MAX_CONFIG {
            return Err(std::io::Error::other(
                "combined configuration exceeds 4 MiB",
            ));
        }
        let mut digest = self.digest.borrow_mut();
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(content.as_bytes());
        digest.update([0]);
        Ok(content)
    }
    fn list_files(&self, dir: &str) -> std::io::Result<Vec<String>> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                files.push(entry.file_name().to_string_lossy().into_owned());
            }
            if files.len() > 1024 {
                return Err(std::io::Error::other(
                    "configuration directory exceeds 1024 files",
                ));
            }
        }
        let target = Path::new(self.path);
        if target.parent() == Some(Path::new(dir)) {
            if let Some(name) = target.file_name().and_then(|name| name.to_str()) {
                if !files.iter().any(|file| file == name) {
                    files.push(name.to_string());
                }
            }
        }
        if files.len() > 1024 {
            return Err(std::io::Error::other(
                "configuration directory exceeds 1024 files",
            ));
        }
        files.sort();
        Ok(files)
    }
    fn dir_exists(&self, dir: &str) -> bool {
        Path::new(dir).is_dir()
    }
    fn write_private(&self, _: &str, _: &str) -> std::io::Result<()> {
        Err(std::io::Error::other("verification is read-only"))
    }
    fn create_dir_all(&self, _: &str) -> std::io::Result<()> {
        Err(std::io::Error::other("verification is read-only"))
    }
}
pub fn verify(path: &Path, text: &str) -> Result<Value, Error> {
    check_path(path)?;
    if text.trim().is_empty() || text.len() > MAX_CONFIG {
        return Err(Error::invalid("配置不能为空且不能超过 1 MiB"));
    }
    let path = path
        .to_str()
        .ok_or_else(|| Error::invalid("配置路径不是 UTF-8"))?;
    let fs = Overlay {
        path,
        text,
        size: RefCell::new(0),
        digest: RefCell::new(Sha256::new()),
    };
    let env = frpcfg::StdEnv;
    let loaded = frpcfg::load_client(
        path,
        &frpcfg::LoadOptions {
            strict: true,
            fs: &fs,
            env: &env,
        },
    )
    .map_err(|error| Error::invalid(format!("配置校验失败：{error}")))?;
    let mut warnings = frpcfg::validate::validate_client_all(
        &loaded.common,
        &loaded.proxies,
        &loaded.visitors,
        &[],
    )
    .map_err(|error| Error::invalid(format!("配置校验失败：{error}")))?;
    warnings.extend(
        loaded
            .report
            .issues
            .iter()
            .map(|issue| format!("{:?} · {}：{}", issue.severity, issue.path, issue.message)),
    );
    if loaded.proxies.len() > 1024 || loaded.visitors.len() > 1024 || warnings.len() > 2048 {
        return Err(Error::invalid("配置资源数量超过限制"));
    }
    let mut digest = fs.digest.into_inner();
    // Template results participate in the preview digest; only the digest crosses IPC.
    let source = frpcfg::export(&loaded.source, frpcfg::ExportFormat::Json)
        .map_err(|error| Error::invalid(format!("配置摘要失败：{error}")))?;
    digest.update(source.as_bytes());
    Ok(
        json!({"digest":format!("{:x}",digest.finalize()),"warnings":warnings,"proxies":loaded.proxies.len(),"visitors":loaded.visitors.len()}),
    )
}
pub trait Replace {
    fn apply(&self, stage: &Path, destination: &Path) -> std::io::Result<()>;
}
pub struct AtomicReplace;
impl Replace for AtomicReplace {
    fn apply(&self, stage: &Path, destination: &Path) -> std::io::Result<()> {
        #[cfg(not(windows))]
        {
            std::fs::rename(stage, destination)
        }
        #[cfg(windows)]
        {
            // Windows cannot replace an open destination via std::fs::rename.
            let backup =
                destination.with_extension(format!("rustfrp-rollback-{}", std::process::id()));
            let existed = destination.exists();
            if existed {
                std::fs::rename(destination, &backup)?;
            }
            match std::fs::rename(stage, destination) {
                Ok(()) => {
                    if existed {
                        std::fs::remove_file(backup)?;
                    }
                    Ok(())
                }
                Err(error) => {
                    if existed {
                        std::fs::rename(backup, destination)?;
                    }
                    Err(error)
                }
            }
        }
    }
}
struct Stage(PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
pub fn save(
    path: &Path,
    text: &str,
    original: &str,
    preview: &str,
    acknowledged: bool,
    replace: &dyn Replace,
) -> Result<Value, Error> {
    let report = verify(path, text)?;
    if report.get("digest").and_then(Value::as_str) != Some(preview) {
        return Err(Error::invalid("配置、包含文件或模板值已变化，请重新验证"));
    }
    if !acknowledged {
        return Err(Error::invalid("请确认兼容报告后保存"));
    }
    let actual = file_bytes(path)?.map_or_else(String::new, |bytes| hash(&bytes));
    if actual != original {
        return Err(Error::invalid("配置文件已被其他程序修改，请重新读取后合并"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| Error::invalid("配置路径缺少父目录"))?;
    std::fs::create_dir_all(parent).map_err(Error::io_context("无法创建配置目录"))?;
    let stage = Stage(parent.join(format!(
        ".rustfrp-config-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&stage.0)
        .map_err(Error::io_context("无法创建私有配置暂存文件"))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(Error::io_context("写入配置暂存失败"))?;
    drop(file);
    // Recheck immediately before replacement, including symlink and concurrent changes.
    check_path(path)?;
    ensure_original(path, original)?;
    replace
        .apply(&stage.0, path)
        .map_err(Error::io_context("原子替换失败，原配置已保留或恢复"))?;
    Ok(json!({"digest":hash(text.as_bytes()),"report":report}))
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    fn config() -> &'static str {
        "serverAddr='127.0.0.1'\nserverPort=7000\n[[proxies]]\nname='test'\ntype='tcp'\nlocalPort=80\nremotePort=0\n"
    }
    #[test]
    fn included_configuration_is_read_only_and_directory_and_total_content_remain_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("new.toml");
        let target = path.to_str().unwrap();
        let overlay = Overlay {
            path: target,
            text: config(),
            size: RefCell::new(0),
            digest: RefCell::new(Sha256::new()),
        };
        let files = overlay.list_files(root.path().to_str().unwrap()).unwrap();
        assert_eq!(files, vec!["new.toml"]);
        assert!(overlay.dir_exists(root.path().to_str().unwrap()));
        assert!(!overlay.dir_exists(root.path().join("missing").to_str().unwrap()));
        assert!(overlay.write_private(target, "should not write").is_err());
        assert!(overlay
            .create_dir_all(root.path().join("never-created").to_str().unwrap())
            .is_err());
        assert!(!path.exists());
        assert!(!root.path().join("never-created").exists());
        std::fs::write(&path, config()).unwrap();
        assert_eq!(
            overlay.list_files(root.path().to_str().unwrap()).unwrap(),
            vec!["new.toml"]
        );
        let included = root.path().join("included.toml");
        std::fs::write(&included, vec![b' '; MAX_CONFIG + 1]).unwrap();
        assert!(overlay.read_to_string(included.to_str().unwrap()).is_err());
        std::fs::write(&included, vec![255]).unwrap();
        assert!(overlay.read_to_string(included.to_str().unwrap()).is_err());
        let content = " ".repeat(MAX_CONFIG);
        std::fs::write(&included, &content).unwrap();
        for _ in 0..4 {
            assert_eq!(
                overlay
                    .read_to_string(included.to_str().unwrap())
                    .unwrap()
                    .len(),
                MAX_CONFIG
            );
        }
        assert!(overlay.read_to_string(included.to_str().unwrap()).is_err());
        assert!(overlay
            .list_files(root.path().join("missing").to_str().unwrap())
            .is_err());
        for index in 0..1023 {
            std::fs::write(root.path().join(format!("item-{index}.toml")), []).unwrap();
        }
        assert!(overlay.list_files(root.path().to_str().unwrap()).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let path = root
                .path()
                .join(std::ffi::OsString::from_vec(vec![255, b'.', b't']));
            assert!(verify(&path, config()).is_err());
        }
        assert!(verify(
            &path,
            "[[proxies]]\nname='missing-domain'\ntype='http'\nlocalPort=80\n"
        )
        .is_err());
    }
    #[test]
    fn full_config_roundtrips_only_after_verification_and_private_atomic_commit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("frpc.toml");
        let initial = read(&path).unwrap();
        assert_eq!(initial["exists"], false);
        let report = verify(&path, config()).unwrap();
        assert_eq!(report["proxies"], 1);
        assert_eq!(report["visitors"], 0);
        assert!(save(&path, config(), "", "wrong", true, &AtomicReplace).is_err());
        assert!(!path.exists());
        assert!(save(
            &path,
            config(),
            "",
            report["digest"].as_str().unwrap(),
            false,
            &AtomicReplace
        )
        .is_err());
        let saved = save(
            &path,
            config(),
            "",
            report["digest"].as_str().unwrap(),
            true,
            &AtomicReplace,
        )
        .unwrap();
        assert_eq!(read(&path).unwrap()["text"], config());
        assert_eq!(saved["digest"], hash(config().as_bytes()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        for source in [
            "serverAddr: 127.0.0.1\nserverPort: 7000",
            "{\"serverAddr\":\"127.0.0.1\",\"serverPort\":7000}",
        ] {
            assert!(verify(&root.path().join("frpc.yaml"), source).is_ok());
        }
    }
    #[test]
    fn includes_templates_and_compatibility_report_share_the_agent_semantics() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("frpc.toml");
        let included = root.path().join("proxy.toml");
        std::fs::write(
            &included,
            "[[proxies]]\nname='included'\ntype='tcp'\nlocalPort=80\n",
        )
        .unwrap();
        let source = format!(
            "serverAddr='127.0.0.1'\nserverPort=7000\ntransport.poolCount=2\nincludes=['{}']\n",
            included.display()
        );
        let report = verify(&path, &source).unwrap();
        assert_eq!(report["proxies"], 1);
        assert!(report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("transport.poolCount")));
        std::fs::write(
            &included,
            "[[proxies]]\nname='changed'\ntype='tcp'\nlocalPort=81\n",
        )
        .unwrap();
        assert!(save(
            &path,
            &source,
            "",
            report["digest"].as_str().unwrap(),
            true,
            &AtomicReplace
        )
        .is_err());
        assert!(!path.exists());
        assert!(verify(&path, "serverAddr='{{ .Envs.HOME }}'\nserverPort=7000\n").is_ok());
    }
    #[test]
    fn invalid_or_concurrently_modified_configs_do_not_overwrite_the_original() {
        struct Failure;
        impl Replace for Failure {
            fn apply(&self, _: &Path, _: &Path) -> std::io::Result<()> {
                Err(std::io::Error::other("injected replace failure"))
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("frpc.toml");
        std::fs::write(&path, config()).unwrap();
        let old = hash(config().as_bytes());
        let changed = config().replace("localPort=80", "localPort=81");
        let report = verify(&path, &changed).unwrap();
        assert!(save(
            &path,
            &changed,
            "stale",
            report["digest"].as_str().unwrap(),
            true,
            &AtomicReplace
        )
        .is_err());
        assert!(save(
            &path,
            &changed,
            &old,
            report["digest"].as_str().unwrap(),
            true,
            &Failure
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), config());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        for source in ["", "transport.protocol='not-a-protocol'", "x ="] {
            assert!(
                verify(&path, source).is_err(),
                "invalid fixture unexpectedly accepted: {source}"
            );
        }
        assert!(verify(&path, &"x".repeat(MAX_CONFIG + 1)).is_err());
        assert!(read(Path::new("relative.toml")).is_err());
        std::fs::write(&path, [255]).unwrap();
        assert!(read(&path).is_err());
        std::fs::write(&path, "x".repeat(MAX_CONFIG + 1)).unwrap();
        assert!(read(&path).is_err());
        #[cfg(unix)]
        {
            let link = root.path().join("link.toml");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read(&link).is_err());
        }
    }
    #[test]
    fn config_read_and_verify_reject_invalid_text_empty_content_and_unbounded_preview() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("invalid.toml");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(read(&path).is_err());
        assert!(verify(&path, " ").is_err());
        assert!(verify(&path, &"x".repeat(MAX_CONFIG + 1)).is_err());
        let target = root.path().join("new.toml");
        let overlay = Overlay {
            path: target.to_str().unwrap(),
            text: config(),
            size: RefCell::new(0),
            digest: RefCell::new(Sha256::new()),
        };
        for index in 0..1024 {
            std::fs::write(root.path().join(format!("item-{index}.toml")), []).unwrap();
        }
        assert!(overlay.list_files(root.path().to_str().unwrap()).is_err());
        assert!(ensure_original(&path, "stale").is_err());
    }

    #[test]
    fn included_file_lists_skip_directories_and_bound_the_added_target_entry() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("not-created.toml");
        std::fs::create_dir(root.path().join("nested")).unwrap();
        let overlay = Overlay {
            path: path.to_str().unwrap(),
            text: config(),
            size: RefCell::new(0),
            digest: RefCell::new(Sha256::new()),
        };
        assert_eq!(
            overlay.list_files(root.path().to_str().unwrap()).unwrap(),
            vec!["not-created.toml"]
        );
        for index in 0..1024 {
            std::fs::write(root.path().join(format!("item-{index}.toml")), []).unwrap();
        }
        assert!(overlay
            .list_files(root.path().to_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("1024"));

        let existing_root = tempfile::tempdir().unwrap();
        let existing = existing_root.path().join("existing.toml");
        std::fs::write(&existing, config()).unwrap();
        let overlay = Overlay {
            path: existing.to_str().unwrap(),
            text: config(),
            size: RefCell::new(0),
            digest: RefCell::new(Sha256::new()),
        };
        let files = overlay
            .list_files(existing_root.path().to_str().unwrap())
            .unwrap();
        assert_eq!(
            files.iter().filter(|name| *name == "existing.toml").count(),
            1
        );
    }

    #[test]
    fn read_rejects_a_non_not_found_open_error_without_following_a_symlink_loop() {
        #[cfg(unix)]
        {
            let root = tempfile::tempdir().unwrap();
            let loop_path = root.path().join("loop");
            std::os::unix::fs::symlink(&loop_path, &loop_path).unwrap();
            assert!(read(&loop_path.join("frpc.toml")).is_err());
        }
    }

    #[test]
    fn configuration_paths_are_absolute_files_and_overlay_directory_checks_are_scoped() {
        let root = tempfile::tempdir().unwrap();
        assert!(check_path(Path::new("relative.toml")).is_err());
        assert!(check_path(Path::new("/")).is_err());
        assert!(check_path(root.path()).is_err());
        assert!(check_path(Path::new(&format!("/{}", "x".repeat(4096)))).is_err());
        #[cfg(unix)]
        {
            let target = root.path().join("config.toml");
            std::fs::write(&target, config()).unwrap();
            let link = root.path().join("linked.toml");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(check_path(&link).is_err());
        }
        let target = root.path().join("new.toml");
        let overlay = Overlay {
            path: target.to_str().unwrap(),
            text: config(),
            size: RefCell::new(0),
            digest: RefCell::new(Sha256::new()),
        };
        let other = root.path().join("other");
        std::fs::create_dir(&other).unwrap();
        assert!(overlay
            .list_files(other.to_str().unwrap())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn verification_rejects_more_than_the_bounded_proxy_count() {
        let root = tempfile::tempdir().unwrap();
        let mut text = String::from("serverAddr='127.0.0.1'\nserverPort=7000\n");
        for index in 0..=1024 {
            text.push_str(&format!(
                "[[proxies]]\nname='tcp-{index}'\ntype='tcp'\nlocalPort=8080\nremotePort={}\n",
                10000 + index
            ));
        }
        assert!(verify(&root.path().join("frpc.toml"), &text)
            .unwrap_err()
            .message
            .contains("资源数量"));

        let mut visitors = String::from("serverAddr='127.0.0.1'\nserverPort=7000\n");
        for index in 0..=1024 {
            visitors.push_str(&format!(
                "[[visitors]]\nname='visitor-{index}'\ntype='stcp'\nserverName='private'\nsecretKey='key'\nbindAddr='127.0.0.1'\nbindPort={}\n",
                50000 + index
            ));
        }
        assert!(verify(&root.path().join("frpc.toml"), &visitors)
            .unwrap_err()
            .message
            .contains("资源数量"));
    }

    #[test]
    fn original_file_digest_accepts_unchanged_bytes_and_rejects_replacements() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("frpc.toml");
        let original = config();
        std::fs::write(&path, original.as_bytes()).unwrap();
        ensure_original(&path, &hash(original.as_bytes())).unwrap();
        assert!(ensure_original(&path, &hash(b"previous config")).is_err());
        std::fs::remove_file(&path).unwrap();
        ensure_original(&path, "").unwrap();
    }
}
