//! 导出为标准 frp 配置（TOML / YAML / JSON），保证用户随时可迁出。

use serde::Serialize;

use super::error::ConfigError;

/// 导出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// TOML（frp 官方推荐）。
    Toml,
    /// YAML。
    Yaml,
    /// JSON。
    Json,
}

/// 把客户端 / 服务端配置序列化为指定格式。
pub fn export<T: Serialize>(cfg: &T, format: ExportFormat) -> Result<String, ConfigError> {
    let err = |e: String| ConfigError::Parse(format!("export: {e}"));
    match format {
        ExportFormat::Toml => toml::to_string_pretty(cfg).map_err(|e| err(e.to_string())),
        ExportFormat::Yaml => serde_yaml_ng::to_string(cfg).map_err(|e| err(e.to_string())),
        ExportFormat::Json => serde_json::to_string_pretty(cfg).map_err(|e| err(e.to_string())),
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use crate::frpcfg::types::{ClientConfig, Proxy};

    fn sample() -> ClientConfig {
        ClientConfig {
            server_addr: "1.2.3.4".into(),
            server_port: 7000,
            proxies: vec![Proxy {
                name: "ssh".into(),
                kind: "tcp".into(),
                local_port: 22,
                remote_port: 6000,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn exports_all_formats() {
        let toml = export(&sample(), ExportFormat::Toml).unwrap();
        assert!(toml.contains("serverAddr = \"1.2.3.4\"") && toml.contains("[[proxies]]"));
        let yaml = export(&sample(), ExportFormat::Yaml).unwrap();
        assert!(yaml.contains("serverAddr: 1.2.3.4"));
        let json = export(&sample(), ExportFormat::Json).unwrap();
        assert!(json.contains("\"serverPort\": 7000"));
    }

    #[test]
    fn roundtrip_is_lossless() {
        let c = sample();
        for f in [ExportFormat::Toml, ExportFormat::Yaml, ExportFormat::Json] {
            let text = export(&c, f).unwrap();
            let v: serde_json::Value = match f {
                ExportFormat::Toml => toml::from_str(&text).unwrap(),
                ExportFormat::Yaml => serde_yaml_ng::from_str(&text).unwrap(),
                ExportFormat::Json => serde_json::from_str(&text).unwrap(),
            };
            assert_eq!(serde_json::from_value::<ClientConfig>(v).unwrap(), c);
        }
    }

    #[test]
    fn toml_cannot_serialize_non_table_root() {
        let err = export(&1_i64, ExportFormat::Toml).unwrap_err();
        assert!(err.to_string().starts_with("export:"));
    }
}
