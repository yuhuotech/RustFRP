//! Bounded private preference files; secrets are rejected instead of persisted.

use crate::config::{AtomicReplace, Replace};
use crate::Error;
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;

pub fn load(root: &Path) -> Result<Value, Error> {
    let path = root.join("preferences.json");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(Error::invalid("桌面偏好必须是普通文件"))
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(_) => return Err(Error::io("无法读取桌面偏好")),
    }
    let file = std::fs::File::open(path).map_err(Error::io_context("无法读取桌面偏好"))?;
    let mut bytes = Vec::new();
    file.take(32769)
        .read_to_end(&mut bytes)
        .map_err(Error::io_context("读取桌面偏好失败"))?;
    if bytes.len() > 32768 {
        return Err(Error::invalid("桌面偏好文件过大"));
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(Error::invalid_context("桌面偏好格式无效"))?;
    validate(&value)?;
    Ok(value)
}

pub fn validate(value: &Value) -> Result<(), Error> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::invalid("桌面偏好必须是对象"))?;
    for (key, value) in object {
        let valid = match key.as_str() {
            "controller_url" => value.as_str().is_some_and(|value| {
                value.is_empty() || crate::controller::controller_url(value).is_ok()
            }),
            "theme" => value
                .as_str()
                .is_some_and(|value| matches!(value, "light" | "dark" | "system")),
            "update_source" => value
                .as_str()
                .is_some_and(|value| matches!(value, "github" | "mainland")),
            "close_to_tray" | "notifications" | "local_mode" | "lightweight"
            | "sidebar_collapsed" => value.is_boolean(),
            "local" => value.as_object().is_some_and(|value| {
                value.iter().all(|(key, value)| {
                    matches!(
                        key.as_str(),
                        "binary"
                            | "controller"
                            | "name"
                            | "stateDir"
                            | "configFile"
                            | "caFile"
                            | "tlsPin"
                    ) && value.as_str().is_some_and(|value| value.len() <= 2048)
                })
            }),
            _ => false,
        };
        if !valid {
            return Err(Error::invalid("桌面偏好包含无效字段或不允许持久化的凭据"));
        }
    }
    Ok(())
}

pub fn save(root: &Path, value: &Value) -> Result<(), Error> {
    save_with(root, value, &AtomicReplace)
}

fn save_with(root: &Path, value: &Value, replace: &dyn Replace) -> Result<(), Error> {
    validate(value)?;
    let data = value.to_string();
    if data.len() > 32768 {
        return Err(Error::invalid("桌面偏好超过大小限制"));
    }
    std::fs::create_dir_all(root).map_err(Error::io_context("无法创建桌面状态目录"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .map_err(Error::io_context("无法保护桌面状态目录"))?;
    }
    let destination = root.join("preferences.json");
    if let Ok(metadata) = std::fs::symlink_metadata(&destination) {
        if !metadata.is_file() {
            return Err(Error::invalid("不能覆盖目录或符号链接中的桌面偏好"));
        }
    }
    let mut file =
        tempfile::NamedTempFile::new_in(root).map_err(Error::io_context("无法创建偏好暂存文件"))?;
    use std::io::Write;
    file.write_all(data.as_bytes())
        .and_then(|()| file.as_file().sync_all())
        .map_err(Error::io_context("保存桌面偏好失败"))?;
    let staging = file.into_temp_path();
    replace
        .apply(&staging, &destination)
        .map_err(Error::io_context("应用桌面偏好失败"))?;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn preferences_are_private_atomic_and_never_accept_tokens_or_passwords() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("desktop");
        assert_eq!(load(&root).unwrap(), json!({}));
        let prefs = json!({"controller_url":"https://example.com/","theme":"dark","local":{"controller":"host:7000","stateDir":"/private/state"}});
        save(&root, &prefs).unwrap();
        assert_eq!(load(&root).unwrap(), prefs);
        assert!(!root.join("preferences.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("preferences.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        for value in [
            json!({"password":"secret"}),
            json!({"local":{"enrollToken":"secret"}}),
            json!({"theme":"invalid"}),
            json!([]),
        ] {
            assert!(save(&root, &value).is_err());
        }
        std::fs::write(root.join("preferences.json"), "x".repeat(32769)).unwrap();
        assert!(load(&root).is_err());
    }

    #[test]
    fn failed_replacement_preserves_original_and_removes_private_staging() {
        struct Fail;
        impl Replace for Fail {
            fn apply(&self, _: &Path, _: &Path) -> std::io::Result<()> {
                Err(std::io::Error::other("injected replacement failure"))
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        save(root, &json!({"theme":"light"})).unwrap();
        assert!(save_with(root, &json!({"theme":"dark"}), &Fail).is_err());
        assert_eq!(load(root).unwrap(), json!({"theme":"light"}));
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 1);
        std::fs::write(root.join("preferences.json"), "not JSON").unwrap();
        assert!(load(root).is_err());
        std::fs::remove_file(root.join("preferences.json")).unwrap();
        std::fs::create_dir(root.join("preferences.json")).unwrap();
        assert!(load(root).is_err());
        assert!(save(root, &json!({})).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn preference_writes_never_follow_an_existing_staging_or_destination_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let external = directory.path().join("external");
        std::fs::write(&external, "preserve").unwrap();
        let root = directory.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&external, root.join("preferences.tmp")).unwrap();
        save(&root, &json!({"theme":"dark"})).unwrap();
        assert_eq!(std::fs::read_to_string(&external).unwrap(), "preserve");
        std::fs::remove_file(root.join("preferences.json")).unwrap();
        std::os::unix::fs::symlink(&external, root.join("preferences.json")).unwrap();
        assert!(load(&root).is_err());
        assert!(save(&root, &json!({})).is_err());
        assert_eq!(std::fs::read_to_string(&external).unwrap(), "preserve");
    }
}
