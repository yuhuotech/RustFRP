//! 客户端本地插件（frp 的 `plugin`）：代替「连接本地端口」，直接在隧道流上提供服务。
//!
//! 支持：`socks5` / `http_proxy` / `static_file` / `unix_domain_socket` /
//! `http2https` / `https2http` / `https2https` / `tls2raw`。其余类型（如 `virtual_net`）在计划阶段被报告并跳过。
//! 每条连接只占一个异步任务；不缓冲整包。

mod http_proxy;
mod socks5;
mod static_file;
mod tls_http;

use std::sync::Arc;
use std::time::Duration;

use rustfrp_core::error::TunnelError;
use rustfrp_core::pipe::{self, Traffic};
use rustfrp_core::tls;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// 可读写的字节流（隧道流或 TLS 流）。
pub trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

/// 已支持的插件类型。
pub const KINDS: [&str; 8] = [
    "socks5",
    "http_proxy",
    "static_file",
    "unix_domain_socket",
    "http2https",
    "https2http",
    "https2https",
    "tls2raw",
];

/// 插件配置（已解析：证书文件内容已读入）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginCfg {
    /// 类型。
    pub kind: String,
    /// 本地服务地址（`host:port`）。
    pub local_addr: String,
    /// unix socket 路径。
    pub unix_path: String,
    /// Host 改写（`*2*` 类）。
    pub host_header_rewrite: String,
    /// 追加 / 覆盖的请求头（`*2*` 类）。
    pub request_headers: Vec<(String, String)>,
    /// 证书与私钥 PEM（`https2*` / `tls2raw`；都为空则生成自签名证书）。
    pub crt_pem: String,
    /// 私钥 PEM。
    pub key_pem: String,
    /// HTTP Basic 用户 / 密码（`http_proxy` / `static_file`）。
    pub http_user: String,
    /// HTTP Basic 密码。
    pub http_password: String,
    /// socks5 用户名 / 密码。
    pub username: String,
    /// socks5 密码。
    pub password: String,
    /// static_file 根目录。
    pub local_path: String,
    /// static_file 去除的 URL 前缀。
    pub strip_prefix: String,
}

/// 运行期插件：配置加 TLS 上下文（只构建一次）。
pub struct Plugin {
    cfg: PluginCfg,
    acceptor: Option<TlsAcceptor>,
    connector: Option<TlsConnector>,
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Plugin({})", self.cfg.kind)
    }
}

impl Plugin {
    /// 校验配置并准备 TLS 上下文。
    pub fn new(cfg: PluginCfg) -> Result<Plugin, String> {
        if !KINDS.contains(&cfg.kind.as_str()) {
            return Err(format!("plugin \"{}\" is not supported", cfg.kind));
        }
        let need = |v: &str, what: &str| {
            if v.is_empty() {
                Err(format!("plugin {}: {what} is required", cfg.kind))
            } else {
                Ok(())
            }
        };
        match cfg.kind.as_str() {
            "http2https" | "https2http" | "https2https" | "tls2raw" => {
                need(&cfg.local_addr, "localAddr")?
            }
            "static_file" => need(&cfg.local_path, "localPath")?,
            "unix_domain_socket" => need(&cfg.unix_path, "unixPath")?,
            _ => {}
        }
        if cfg.kind == "unix_domain_socket" && !cfg!(unix) {
            return Err("plugin unix_domain_socket is only supported on unix".into());
        }
        let inbound_tls = ["https2http", "https2https", "tls2raw"].contains(&cfg.kind.as_str());
        let outbound_tls = ["http2https", "https2https"].contains(&cfg.kind.as_str());
        let acceptor = if inbound_tls {
            let id = if cfg.crt_pem.is_empty() && cfg.key_pem.is_empty() {
                tls::self_signed(vec!["localhost".into()])
            } else {
                tls::identity_from_pem(cfg.crt_pem.as_bytes(), cfg.key_pem.as_bytes())
            }
            .map_err(|e| format!("plugin {}: {e}", cfg.kind))?;
            Some(TlsAcceptor::from(
                tls::app_server_config(id).map_err(|e| format!("plugin {}: {e}", cfg.kind))?,
            ))
        } else {
            None
        };
        let connector = if outbound_tls {
            Some(TlsConnector::from(
                tls::app_client_config().map_err(|e| format!("plugin {}: {e}", cfg.kind))?,
            ))
        } else {
            None
        };
        Ok(Plugin {
            cfg,
            acceptor,
            connector,
        })
    }

    /// 在一条隧道流上提供服务，直到连接结束。
    pub async fn serve<S: Duplex + 'static>(
        &self,
        stream: S,
        traffic: &Arc<Traffic>,
        io_timeout: Duration,
    ) -> Result<(), TunnelError> {
        let c = &self.cfg;
        match c.kind.as_str() {
            "socks5" => socks5::serve(stream, c, traffic, io_timeout).await,
            "http_proxy" => http_proxy::serve(stream, c, traffic, io_timeout).await,
            "static_file" => static_file::serve(stream, c, traffic, io_timeout).await,
            "unix_domain_socket" => unix(stream, c, traffic).await,
            _ => {
                tls_http::serve(
                    stream,
                    c,
                    self.acceptor.as_ref(),
                    self.connector.as_ref(),
                    traffic,
                    io_timeout,
                )
                .await
            }
        }
    }
}

/// 拨号并带超时。
async fn dial(addr: &str, limit: Duration) -> Result<TcpStream, TunnelError> {
    let s = tokio::time::timeout(limit, TcpStream::connect(addr))
        .await
        .map_err(|_| TunnelError::Timeout("dial plugin target"))??;
    let _ = s.set_nodelay(true);
    Ok(s)
}

#[cfg(unix)]
async fn unix<S: Duplex>(
    mut stream: S,
    c: &PluginCfg,
    traffic: &Arc<Traffic>,
) -> Result<(), TunnelError> {
    let mut local = tokio::net::UnixStream::connect(&c.unix_path).await?;
    pipe::relay_counted(&mut stream, &mut local, traffic).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn unix<S: Duplex>(
    _stream: S,
    _c: &PluginCfg,
    _traffic: &Arc<Traffic>,
) -> Result<(), TunnelError> {
    Err(TunnelError::Wire(
        "unix_domain_socket is only supported on unix".into(),
    ))
}

/// 读取直到出现完整的 HTTP 头（`\r\n\r\n`），返回（头长度, 已读字节）；超过上限或超时报错。
async fn read_head<S: Duplex>(
    s: &mut S,
    limit: usize,
    io_timeout: Duration,
) -> Result<(usize, Vec<u8>), TunnelError> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 2048];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            return Ok((i + 4, buf));
        }
        if buf.len() >= limit {
            return Err(TunnelError::Wire("request head too large".into()));
        }
        let n = tokio::time::timeout(io_timeout, s.read(&mut tmp))
            .await
            .map_err(|_| TunnelError::Timeout("read request head"))??;
        if n == 0 {
            return Err(TunnelError::Closed);
        }
        buf.extend_from_slice(tmp.get(..n).unwrap_or(&[]));
    }
}
