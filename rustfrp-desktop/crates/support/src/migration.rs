//! One-time preference migration retains the previous namespace and Agent identity paths.
use crate::{settings, startup::Platform, Error};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

const LIMIT: u64 = 65536;
pub fn candidates(platform: Platform, home: &Path, data: Option<&Path>) -> Vec<PathBuf> {
    match platform {
        Platform::Mac => vec![home.join("Library/Preferences/com.rustfrp.rustfrpApp.plist")],
        Platform::Linux => {
            let root = data
                .map(Path::to_path_buf)
                .unwrap_or_else(|| home.join(".local/share"));
            vec![
                root.join("com.rustfrp.rustfrp_app/shared_preferences.json"),
                root.join("rustfrp_app/shared_preferences.json"),
            ]
        }
        Platform::Windows => data
            .map(|root| vec![root.join("com.rustfrp/rustfrp_app/shared_preferences.json")])
            .unwrap_or_default(),
    }
}
fn read(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::io("无法读取旧桌面偏好")),
    };
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(Error::invalid("旧桌面偏好必须是有界普通文件"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(Error::io_context("旧偏好不可读"))?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::io_context("旧偏好读取失败"))?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::invalid("旧偏好超过 64 KiB"));
    }
    Ok(Some(bytes))
}
fn decode(path: &Path, bytes: &[u8]) -> Result<Value, Error> {
    if path
        .extension()
        .is_some_and(|extension| extension == "plist")
    {
        let plist = plist::Value::from_reader(std::io::Cursor::new(bytes))
            .map_err(Error::invalid_context("旧偏好 plist 格式无效"))?;
        let dictionary = plist
            .as_dictionary()
            .ok_or_else(|| Error::invalid("旧偏好必须是字典"))?;
        let mut result = serde_json::Map::new();
        for (key, value) in dictionary {
            if let Some(value) = value.as_string() {
                result.insert(key.clone(), json!(value));
            } else if let Some(value) = value.as_boolean() {
                result.insert(key.clone(), json!(value));
            }
        }
        Ok(Value::Object(result))
    } else {
        serde_json::from_slice(bytes).map_err(Error::invalid_context("旧偏好 JSON 格式无效"))
    }
}
pub fn convert(legacy: &Value) -> Result<Value, Error> {
    let source = legacy
        .as_object()
        .ok_or_else(|| Error::invalid("旧桌面偏好必须是对象"))?;
    let get = |key: &str| {
        source
            .get(&format!("flutter.{key}"))
            .or_else(|| source.get(key))
    };
    let mut result = serde_json::Map::new();
    for (previous, current) in [
        ("server_url", "controller_url"),
        ("theme_mode", "theme"),
        ("local_mode", "local_mode"),
        ("close_to_tray", "close_to_tray"),
        ("notifications_enabled", "notifications"),
        ("sidebar_collapsed", "sidebar_collapsed"),
        ("rustfrp_update_source", "update_source"),
    ] {
        if let Some(value) = get(previous) {
            result.insert(current.into(), value.clone());
        }
    }
    if let Some(local) = get("local_agent") {
        let local = if let Some(text) = local.as_str() {
            serde_json::from_str(text).map_err(Error::invalid_context(
                "旧本机连接配置无效，请保留原文件后重新填写",
            ))?
        } else {
            local.clone()
        };
        result.insert("local".into(), local);
    }
    let result = Value::Object(result);
    settings::validate(&result)?;
    Ok(result)
}
pub fn migrate(root: &Path, paths: &[PathBuf]) -> Result<Option<Value>, Error> {
    if std::fs::symlink_metadata(root.join("preferences.json")).is_ok() {
        return Ok(None);
    }
    for path in paths {
        let Some(bytes) = read(path)? else { continue };
        let preferences = convert(&decode(path, &bytes)?)?;
        if preferences
            .as_object()
            .is_none_or(|object| object.is_empty())
        {
            continue;
        }
        std::fs::create_dir_all(root).map_err(Error::io_context("无法创建偏好迁移目录"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
                .map_err(Error::io_context("无法保护偏好备份目录"))?;
        }
        let backup = root.join(
            if path
                .extension()
                .is_some_and(|extension| extension == "plist")
            {
                "preferences.legacy-backup.plist"
            } else {
                "preferences.legacy-backup.json"
            },
        );
        if let Some(existing) = read(&backup)? {
            if existing != bytes {
                return Err(Error::invalid("旧偏好备份已存在且内容不同，已保留双方文件"));
            }
        } else {
            let mut stage = tempfile::NamedTempFile::new_in(root)
                .map_err(Error::io_context("无法创建私有偏好备份"))?;
            stage
                .write_all(&bytes)
                .and_then(|()| stage.as_file().sync_all())
                .map_err(Error::io_context("无法保存旧偏好备份"))?;
            stage
                .persist_noclobber(&backup)
                .map_err(Error::io_context("旧偏好备份创建失败"))?;
        }
        settings::save(root, &preferences)?;
        return Ok(Some(preferences));
    }
    Ok(None)
}
pub fn current(root: &Path) -> Result<(), Error> {
    let platform = Platform::current();
    let home = std::env::var_os(if platform == Platform::Windows {
        "USERPROFILE"
    } else {
        "HOME"
    })
    .map(PathBuf::from)
    .ok_or_else(|| Error::io("无法确定旧偏好用户目录"))?;
    let data = std::env::var_os(if platform == Platform::Windows {
        "APPDATA"
    } else {
        "XDG_DATA_HOME"
    })
    .map(PathBuf::from);
    migrate(root, &candidates(platform, &home, data.as_deref()))?;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn migration_preserves_config_identity_and_old_namespace_without_copying_credentials_into_preferences(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("shared_preferences.json");
        let root = directory.path().join("new");
        let configuration = json!({"binary":"/existing/agent","stateDir":"/existing/identity","controller":"host:7000","name":"中文","configFile":"","caFile":"","tlsPin":""});
        let data = json!({"flutter.local_agent":configuration.to_string(),"flutter.server_url":"https://controller.example/","flutter.theme_mode":"dark","flutter.local_mode":true,"flutter.notifications_enabled":false,"flutter.sidebar_collapsed":true,"flutter.rustfrp_update_source":"mainland","flutter.password":"never migrate","flutter.close_to_tray":true});
        std::fs::write(&legacy, data.to_string()).unwrap();
        let preferences = migrate(&root, std::slice::from_ref(&legacy))
            .unwrap()
            .unwrap();
        assert_eq!(preferences["local"], configuration);
        assert_eq!(preferences["notifications"], false);
        assert_eq!(preferences["theme"], "dark");
        assert!(preferences.get("password").is_none());
        assert_eq!(settings::load(&root).unwrap(), preferences);
        assert_eq!(
            std::fs::read(&legacy).unwrap(),
            std::fs::read(root.join("preferences.legacy-backup.json")).unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("preferences.legacy-backup.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(migrate(&root, std::slice::from_ref(&legacy))
            .unwrap()
            .is_none());
        std::fs::remove_file(root.join("preferences.json")).unwrap();
        assert!(migrate(&root, std::slice::from_ref(&legacy))
            .unwrap()
            .is_some());
        std::fs::remove_file(root.join("preferences.json")).unwrap();
        std::fs::write(&legacy, "{\"theme_mode\":\"light\"}").unwrap();
        assert!(migrate(&root, std::slice::from_ref(&legacy)).is_err());
        assert!(convert(&json!({"local_agent":"bad"})).is_err());
        assert!(convert(&json!([])).is_err());
        assert!(convert(&json!({"theme_mode":"unknown"})).is_err());
    }
    #[test]
    fn plist_and_known_platform_locations_are_bounded_and_do_not_follow_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path();
        assert_eq!(candidates(Platform::Linux, home, None).len(), 2);
        assert_eq!(
            candidates(Platform::Windows, home, None),
            Vec::<PathBuf>::new()
        );
        assert_eq!(
            candidates(Platform::Windows, home, Some(home)),
            vec![home.join("com.rustfrp/rustfrp_app/shared_preferences.json")]
        );
        let path = candidates(Platform::Mac, home, None).remove(0);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path,"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>flutter.theme_mode</key><string>light</string><key>flutter.local_mode</key><true/><key>other</key><array/></dict></plist>").unwrap();
        let root = home.join("new");
        let migrated = migrate(&root, std::slice::from_ref(&path))
            .unwrap()
            .unwrap();
        assert_eq!(migrated, json!({"theme":"light","local_mode":true}));
        assert!(decode(&path, b"not plist").is_err());
        assert!(decode(&path, b"<plist version=\"1.0\"><array/></plist>").is_err());
        assert_eq!(read(&home.join("missing")).unwrap(), None);
        assert!(read(home).is_err());
        let large = home.join("large");
        std::fs::write(&large, vec![0; LIMIT as usize + 1]).unwrap();
        assert!(read(&large).is_err());
        let empty = home.join("empty.json");
        std::fs::write(&empty, "{}").unwrap();
        assert!(
            migrate(&home.join("another"), &[home.join("missing"), empty])
                .unwrap()
                .is_none()
        );
        #[cfg(unix)]
        {
            let link = home.join("symlink");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read(&link).is_err());
        }
    }
}
