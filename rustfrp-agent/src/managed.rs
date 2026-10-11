//! 受管模式的本地支持：设备身份文件、接入参数、生成运行配置。

use std::sync::Arc;

use rustfrp_core::frpcfg::{ConfigFs, Env};
use rustfrp_core::identity::DeviceKey;
use serde::{Deserialize, Serialize};

use crate::client::{ClientConfig, EnrollInfo, ManagedCfg};

/// 受管模式命令行参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedOpts {
    /// Controller 的 Edge 地址，`host` 或 `host:port`（默认端口 7000）。
    pub controller: String,
    /// 一次性接入令牌（首次接入必填）。
    pub enroll_token: Option<String>,
    /// 设备名（默认取主机名）。
    pub name: Option<String>,
    /// 身份文件所在目录（默认 `~/.rustfrp`）。
    pub state_dir: Option<String>,
    /// 受信 CA 文件（PEM）；缺省不校验证书（与 frp 一致）。
    pub ca_file: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    /// 十六进制 PKCS#8 私钥。
    key: String,
    /// Controller 签发的设备 ID。
    #[serde(default)]
    client_id: Option<String>,
}

/// 已加载的身份。
#[derive(Debug)]
pub struct LoadedIdentity {
    /// 设备密钥。
    pub key: Arc<DeviceKey>,
    /// 设备 ID（尚未接入则为空）。
    pub client_id: Option<String>,
}

/// 状态目录：显式指定优先，否则 `$HOME/.rustfrp`（Windows 用 `%APPDATA%\rustfrp`）。
pub fn state_dir(opts: &ManagedOpts, env: &dyn Env) -> Result<String, String> {
    if let Some(d) = &opts.state_dir {
        return Ok(d.clone());
    }
    if let Some(h) = env.get("HOME") {
        return Ok(format!("{h}/.rustfrp"));
    }
    if let Some(a) = env.get("APPDATA") {
        return Ok(format!("{a}\\rustfrp"));
    }
    Err("cannot determine a state directory; pass --state-dir".into())
}

fn identity_path(dir: &str) -> String {
    format!("{dir}/identity.json")
}

/// 读取身份文件；不存在则生成新密钥并保存。
pub fn load_or_create(fs: &dyn ConfigFs, dir: &str) -> Result<LoadedIdentity, String> {
    let path = identity_path(dir);
    if let Ok(text) = fs.read_to_string(&path) {
        let f: IdentityFile = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        let key = DeviceKey::from_hex(&f.key).map_err(|e| format!("{path}: {e}"))?;
        return Ok(LoadedIdentity {
            key: Arc::new(key),
            client_id: f.client_id,
        });
    }
    let key = DeviceKey::generate().map_err(|e| e.to_string())?;
    fs.create_dir_all(dir)
        .map_err(|e| format!("create {dir}: {e}"))?;
    save(fs, dir, &key, None)?;
    Ok(LoadedIdentity {
        key: Arc::new(key),
        client_id: None,
    })
}

/// 保存身份文件（权限仅当前用户）。
pub fn save(
    fs: &dyn ConfigFs,
    dir: &str,
    key: &DeviceKey,
    client_id: Option<&str>,
) -> Result<(), String> {
    let f = IdentityFile {
        key: key.to_hex(),
        client_id: client_id.map(str::to_string),
    };
    let text = serde_json::to_string_pretty(&f).map_err(|e| e.to_string())?;
    let path = identity_path(dir);
    fs.write_private(&path, &text)
        .map_err(|e| format!("write {path}: {e}"))
}

/// 拆分 `host[:port]`。
pub fn split_controller(s: &str) -> Result<(String, u16), String> {
    match s.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => {
            let port = p
                .parse::<u16>()
                .map_err(|_| format!("invalid port in --controller \"{s}\""))?;
            Ok((h.to_string(), port))
        }
        _ if s.is_empty() || s.contains(':') => Err(format!("invalid --controller \"{s}\"")),
        _ => Ok((s.to_string(), 7000)),
    }
}

fn device_name(opts: &ManagedOpts, env: &dyn Env) -> String {
    opts.name
        .clone()
        .or_else(|| env.get("HOSTNAME"))
        .or_else(|| env.get("COMPUTERNAME"))
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

/// 由命令行参数与身份生成运行配置。
pub fn client_config(
    opts: &ManagedOpts,
    id: &LoadedIdentity,
    fs: &dyn ConfigFs,
    env: &dyn Env,
) -> Result<ClientConfig, String> {
    let (host, port) = split_controller(&opts.controller)?;
    if id.client_id.is_none() && opts.enroll_token.is_none() {
        return Err(
            "this device is not enrolled yet; pass --enroll-token <token> from the panel".into(),
        );
    }
    let trusted_ca_pem = match &opts.ca_file {
        Some(f) => Some(
            fs.read_to_string(f)
                .map_err(|e| format!("read --ca-file {f}: {e}"))?
                .into_bytes(),
        ),
        None => None,
    };
    let enroll = opts.enroll_token.as_ref().map(|t| EnrollInfo {
        token: t.clone(),
        name: device_name(opts, env),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        version: env!("CARGO_PKG_VERSION").into(),
    });
    Ok(ClientConfig {
        server_addr: host,
        server_port: port,
        trusted_ca_pem,
        // 首次接入失败（令牌无效 / 过期）应直接报错退出；已接入的设备则一直重试。
        login_fail_exit: id.client_id.is_none(),
        managed: Some(ManagedCfg {
            key: id.key.clone(),
            client_id: id.client_id.clone(),
            enroll,
        }),
        ..Default::default()
    })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use rustfrp_core::frpcfg::{MapEnv, MemFs};

    fn opts() -> ManagedOpts {
        ManagedOpts {
            controller: "frp.example.com".into(),
            enroll_token: Some("t".into()),
            name: None,
            state_dir: None,
            ca_file: None,
        }
    }

    fn env(pairs: &[(&str, &str)]) -> MapEnv {
        MapEnv(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn state_dir_resolution() {
        let mut o = opts();
        assert_eq!(
            state_dir(&o, &env(&[("HOME", "/home/u")])).unwrap(),
            "/home/u/.rustfrp"
        );
        assert_eq!(
            state_dir(&o, &env(&[("APPDATA", "C:\\Users\\u\\AppData")])).unwrap(),
            "C:\\Users\\u\\AppData\\rustfrp"
        );
        assert!(state_dir(&o, &env(&[])).is_err());
        o.state_dir = Some("/var/lib/rustfrp".into());
        assert_eq!(
            state_dir(&o, &env(&[("HOME", "/h")])).unwrap(),
            "/var/lib/rustfrp"
        );
    }

    #[test]
    fn identity_is_created_once_and_reloaded() {
        let fs = MemFs::new();
        let first = load_or_create(&fs, "/s").unwrap();
        assert!(first.client_id.is_none());
        let again = load_or_create(&fs, "/s").unwrap();
        assert_eq!(
            first.key.public_hex(),
            again.key.public_hex(),
            "the key is stable across restarts"
        );

        save(&fs, "/s", &first.key, Some("dev-1")).unwrap();
        let enrolled = load_or_create(&fs, "/s").unwrap();
        assert_eq!(enrolled.client_id.as_deref(), Some("dev-1"));
        assert_eq!(enrolled.key.public_hex(), first.key.public_hex());
    }

    #[test]
    fn corrupt_identity_files_are_reported_not_overwritten() {
        let fs = MemFs::new().with("/s/identity.json", "not json");
        assert!(load_or_create(&fs, "/s")
            .unwrap_err()
            .contains("identity.json"));
        let fs = MemFs::new().with("/s/identity.json", r#"{"key":"zz"}"#);
        assert!(load_or_create(&fs, "/s").is_err());
        assert_eq!(
            fs.read_to_string("/s/identity.json").unwrap(),
            r#"{"key":"zz"}"#
        );
    }

    #[test]
    fn controller_parsing() {
        assert_eq!(split_controller("h").unwrap(), ("h".into(), 7000));
        assert_eq!(split_controller("h:7100").unwrap(), ("h".into(), 7100));
        for bad in ["", ":7000", "h:abc", "h:99999", "a:b:c"] {
            assert!(split_controller(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn config_generation() {
        let fs = MemFs::new().with("/ca.pem", "PEM");
        let id = LoadedIdentity {
            key: Arc::new(DeviceKey::generate().unwrap()),
            client_id: None,
        };
        let mut o = opts();
        o.controller = "h:7100".into();
        o.name = Some("my-box".into());
        o.ca_file = Some("/ca.pem".into());
        let c = client_config(&o, &id, &fs, &env(&[])).unwrap();
        assert_eq!((c.server_addr.as_str(), c.server_port), ("h", 7100));
        assert_eq!(c.trusted_ca_pem.as_deref(), Some(&b"PEM"[..]));
        assert!(
            c.login_fail_exit,
            "a failed first enrollment must be reported"
        );
        let m = c.managed.unwrap();
        assert!(m.client_id.is_none());
        let e = m.enroll.unwrap();
        assert_eq!((e.token.as_str(), e.name.as_str()), ("t", "my-box"));

        // 已接入后不需要令牌。
        let enrolled = LoadedIdentity {
            client_id: Some("dev-1".into()),
            ..id
        };
        o.enroll_token = None;
        let c = client_config(&o, &enrolled, &fs, &env(&[])).unwrap();
        assert!(!c.login_fail_exit, "an enrolled device keeps retrying");
        assert_eq!(c.managed.unwrap().client_id.as_deref(), Some("dev-1"));
        // 未接入又没有令牌 → 明确的错误。
        let fresh = LoadedIdentity {
            key: Arc::new(DeviceKey::generate().unwrap()),
            client_id: None,
        };
        assert!(client_config(&o, &fresh, &fs, &env(&[]))
            .unwrap_err()
            .contains("--enroll-token"));
        o.enroll_token = Some("t".into());
        o.ca_file = Some("/missing.pem".into());
        assert!(client_config(&o, &fresh, &fs, &env(&[]))
            .unwrap_err()
            .contains("--ca-file"));
        o.ca_file = None;
        o.controller = ":".into();
        assert!(client_config(&o, &fresh, &fs, &env(&[])).is_err());
    }

    #[test]
    fn device_name_fallbacks() {
        let mut o = opts();
        assert_eq!(
            device_name(&o, &env(&[("HOSTNAME", "h1"), ("COMPUTERNAME", "c1")])),
            "h1"
        );
        assert_eq!(device_name(&o, &env(&[("COMPUTERNAME", "c1")])), "c1");
        assert_eq!(device_name(&o, &env(&[])), std::env::consts::OS);
        o.name = Some("explicit".into());
        assert_eq!(device_name(&o, &env(&[("HOSTNAME", "h1")])), "explicit");
    }
}
