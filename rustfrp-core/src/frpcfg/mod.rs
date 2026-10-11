//! frp 配置兼容层：读取、校验、补全默认值、导出 frp 原生配置（`frps.toml` / `frpc.toml`）。
//!
//! 这是 RustFRP「配置层完全兼容 frp」原则的实现。字段、默认值、校验规则与错误文案对齐
//! frp 主线版本（见 `rustfrp-protocol/frp-compat/upstream`）。

pub mod compat;
pub mod complete;
pub mod env;
pub mod error;
pub mod export;
pub mod ini;
pub mod load;
pub mod strict;
pub mod template;
pub mod types;
pub mod validate;

pub use compat::{CompatReport, Issue, Severity};
pub use env::{ConfigFs, Env, MapEnv, MemFs, StdEnv, StdFs};
pub use error::ConfigError;
pub use export::{export, ExportFormat};
pub use load::{
    load_client, load_client_str, load_server, load_server_str, Format, LoadOptions, LoadedClient,
    LoadedServer,
};
pub use types::*;
