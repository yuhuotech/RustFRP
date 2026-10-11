//! 配置加载错误。

use thiserror::Error;

/// 配置加载 / 校验错误。消息文案尽量与 frp 保持一致，方便用户对照排查。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// 读文件失败。
    #[error("{0}")]
    Io(String),
    /// 语法错误（TOML / YAML / JSON）。
    #[error("{0}")]
    Parse(String),
    /// 模板渲染失败。
    #[error("template: {0}")]
    Template(String),
    /// 严格模式下出现未知字段。
    #[error("unknown field \"{0}\"")]
    UnknownField(String),
    /// 代理类型缺失或未知。
    #[error("decode proxy at index {index}: {message}")]
    Proxy {
        /// 在 `proxies` 中的下标。
        index: usize,
        /// 原因。
        message: String,
    },
    /// 访问者类型缺失或未知。
    #[error("decode visitor at index {index}: {message}")]
    Visitor {
        /// 在 `visitors` 中的下标。
        index: usize,
        /// 原因。
        message: String,
    },
    /// 旧版 INI 配置，需先用 `convert` 转换。
    #[error(
        "legacy INI configuration is not supported directly; run `rustfrp-agent convert` first"
    )]
    LegacyIni,
    /// 名称重复。
    #[error("{kind} name [{name}] is duplicated")]
    Duplicated {
        /// `proxy` 或 `visitor`。
        kind: &'static str,
        /// 重复的名称。
        name: String,
    },
    /// 校验失败。
    #[error("{0}")]
    Validation(String),
}
