//! 隧道层错误。

use thiserror::Error;

/// 隧道层错误。
#[derive(Debug, Error)]
pub enum TunnelError {
    /// I/O 错误。
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// 线协议错误（帧过大、JSON 非法、意外消息）。
    #[error("wire: {0}")]
    Wire(String),
    /// TLS 配置或握手错误。
    #[error("tls: {0}")]
    Tls(String),
    /// 多路复用错误。
    #[error("mux: {0}")]
    Mux(String),
    /// 鉴权失败。
    #[error("auth: {0}")]
    Auth(String),
    /// 操作超时。
    #[error("timeout: {0}")]
    Timeout(&'static str),
    /// 会话已关闭。
    #[error("session closed")]
    Closed,
}
