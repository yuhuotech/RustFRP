//! frp 配置的数据结构（字段名与 frp 主线版本 `pkg/config/v1` 一致）。
//!
//! 约定：Go 里「零值 = 未设置」的字段（`omitempty` + `EmptyOr`）用普通类型承载，默认值在
//! `complete()` 里补齐；Go 里的 `*bool` 用 `Option<bool>`。代理/访问者/插件使用「超集」结构，
//! 每种类型允许哪些字段由 [`super::strict`] 的表决定，从而复刻 frp 的严格模式。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub(crate) fn is_zero(v: &i64) -> bool {
    *v == 0
}
pub(crate) fn is_false(v: &bool) -> bool {
    !*v
}
pub(crate) fn is_empty_str(v: &str) -> bool {
    v.is_empty()
}
fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}
fn is_default_headers(v: &HeaderOperations) -> bool {
    v.set.is_empty()
}

/// 带宽限制，形如 `1MB` / `500KB`。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bandwidth {
    text: String,
    bytes: i64,
}

impl Bandwidth {
    /// 解析 `NMB` / `NKB`（N 可为小数）；空串表示不限速。
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(Self::default());
        }
        let (num, base) = if let Some(n) = s.strip_suffix("MB") {
            (n, 1024.0 * 1024.0)
        } else if let Some(n) = s.strip_suffix("KB") {
            (n, 1024.0)
        } else {
            return Err("unit not support".to_string());
        };
        let f: f64 = num.parse().map_err(|e| format!("{e}"))?;
        Ok(Self {
            text: s.to_string(),
            bytes: (f * base) as i64,
        })
    }

    /// 字节每秒；0 表示不限速。
    pub fn bytes(&self) -> i64 {
        self.bytes
    }

    /// 原始文本。
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// 是否未设置。
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

impl Serialize for Bandwidth {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for Bandwidth {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// `allowPorts` 的一项：`single` 或 `start..=end`。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PortsRange {
    /// 区间起点。
    #[serde(skip_serializing_if = "is_zero")]
    pub start: i64,
    /// 区间终点。
    #[serde(skip_serializing_if = "is_zero")]
    pub end: i64,
    /// 单个端口。
    #[serde(skip_serializing_if = "is_zero")]
    pub single: i64,
}

/// 日志配置。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// `console` 或文件路径。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub to: String,
    /// trace/debug/info/warn/error。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub level: String,
    /// 日志保留天数。
    #[serde(rename = "maxDays", skip_serializing_if = "is_zero")]
    pub max_days: i64,
    /// 控制台输出时关闭颜色。
    #[serde(rename = "disablePrintColor", skip_serializing_if = "is_false")]
    pub disable_print_color: bool,
}

/// 通用 TLS 文件配置。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsConfig {
    /// 证书文件。
    #[serde(rename = "certFile", skip_serializing_if = "is_empty_str")]
    pub cert_file: String,
    /// 私钥文件。
    #[serde(rename = "keyFile", skip_serializing_if = "is_empty_str")]
    pub key_file: String,
    /// 受信 CA 文件。
    #[serde(rename = "trustedCaFile", skip_serializing_if = "is_empty_str")]
    pub trusted_ca_file: String,
    /// 自定义服务名。
    #[serde(rename = "serverName", skip_serializing_if = "is_empty_str")]
    pub server_name: String,
}

/// 管理页面（webServer）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebServerConfig {
    /// 监听地址。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub addr: String,
    /// 监听端口，0 = 不启动。
    #[serde(skip_serializing_if = "is_zero")]
    pub port: i64,
    /// 用户名。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub user: String,
    /// 密码。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub password: String,
    /// 静态资源目录。
    #[serde(rename = "assetsDir", skip_serializing_if = "is_empty_str")]
    pub assets_dir: String,
    /// pprof（RustFRP 不提供，仅保留字段）。
    #[serde(rename = "pprofEnable", skip_serializing_if = "is_false")]
    pub pprof_enable: bool,
    /// 启用 TLS。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,
}

/// QUIC 参数。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QuicOptions {
    /// 心跳周期（秒）。
    #[serde(rename = "keepalivePeriod", skip_serializing_if = "is_zero")]
    pub keepalive_period: i64,
    /// 最大空闲（秒）。
    #[serde(rename = "maxIdleTimeout", skip_serializing_if = "is_zero")]
    pub max_idle_timeout: i64,
    /// 最大并发流。
    #[serde(rename = "maxIncomingStreams", skip_serializing_if = "is_zero")]
    pub max_incoming_streams: i64,
}

/// 动态取值来源（token 等）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ValueSource {
    /// `file` 或 `exec`。
    #[serde(rename = "type")]
    pub kind: String,
    /// 文件来源。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileSource>,
    /// 命令来源。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecSource>,
}

/// 文件来源。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FileSource {
    /// 路径。
    pub path: String,
}

/// 命令来源。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecSource {
    /// 命令。
    pub command: String,
    /// 参数。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// 环境变量。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<ExecEnvVar>,
}

/// 命令的环境变量。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecEnvVar {
    /// 名称。
    pub name: String,
    /// 值。
    pub value: String,
}

/// 客户端 OIDC 鉴权。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthOidcClient {
    /// clientID。
    #[serde(rename = "clientID", skip_serializing_if = "is_empty_str")]
    pub client_id: String,
    /// clientSecret。
    #[serde(rename = "clientSecret", skip_serializing_if = "is_empty_str")]
    pub client_secret: String,
    /// audience。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub audience: String,
    /// scope。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub scope: String,
    /// Token 端点。
    #[serde(rename = "tokenEndpointURL", skip_serializing_if = "is_empty_str")]
    pub token_endpoint_url: String,
    /// 额外端点参数。
    #[serde(
        rename = "additionalEndpointParams",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub additional_endpoint_params: BTreeMap<String, String>,
    /// 端点 CA。
    #[serde(rename = "trustedCaFile", skip_serializing_if = "is_empty_str")]
    pub trusted_ca_file: String,
    /// 跳过证书校验。
    #[serde(rename = "insecureSkipVerify", skip_serializing_if = "is_false")]
    pub insecure_skip_verify: bool,
    /// 访问端点所用代理。
    #[serde(rename = "proxyURL", skip_serializing_if = "is_empty_str")]
    pub proxy_url: String,
    /// 动态 token 来源。
    #[serde(rename = "tokenSource", skip_serializing_if = "Option::is_none")]
    pub token_source: Option<ValueSource>,
}

/// 服务端 OIDC 鉴权。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthOidcServer {
    /// 签发者。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub issuer: String,
    /// 受众。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub audience: String,
    /// 跳过过期检查。
    #[serde(rename = "skipExpiryCheck", skip_serializing_if = "is_false")]
    pub skip_expiry_check: bool,
    /// 跳过签发者检查。
    #[serde(rename = "skipIssuerCheck", skip_serializing_if = "is_false")]
    pub skip_issuer_check: bool,
}

/// 客户端鉴权。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthClient {
    /// `token` / `oidc`。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub method: String,
    /// `HeartBeats` / `NewWorkConns`。
    #[serde(rename = "additionalScopes", skip_serializing_if = "Vec::is_empty")]
    pub additional_scopes: Vec<String>,
    /// 令牌。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub token: String,
    /// 令牌来源。
    #[serde(rename = "tokenSource", skip_serializing_if = "Option::is_none")]
    pub token_source: Option<ValueSource>,
    /// OIDC 设置。
    #[serde(skip_serializing_if = "is_default")]
    pub oidc: AuthOidcClient,
}

/// 服务端鉴权。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthServer {
    /// `token` / `oidc`。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub method: String,
    /// 额外作用域。
    #[serde(rename = "additionalScopes", skip_serializing_if = "Vec::is_empty")]
    pub additional_scopes: Vec<String>,
    /// 令牌。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub token: String,
    /// 令牌来源。
    #[serde(rename = "tokenSource", skip_serializing_if = "Option::is_none")]
    pub token_source: Option<ValueSource>,
    /// OIDC 设置。
    #[serde(skip_serializing_if = "is_default")]
    pub oidc: AuthOidcServer,
}

/// 客户端 TLS。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsClient {
    /// 是否启用（默认 true）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable: Option<bool>,
    /// 关闭自定义首字节（默认 true）。
    #[serde(
        rename = "disableCustomTLSFirstByte",
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_custom_tls_first_byte: Option<bool>,
    /// 证书文件。
    #[serde(rename = "certFile", skip_serializing_if = "is_empty_str")]
    pub cert_file: String,
    /// 私钥文件。
    #[serde(rename = "keyFile", skip_serializing_if = "is_empty_str")]
    pub key_file: String,
    /// 受信 CA。
    #[serde(rename = "trustedCaFile", skip_serializing_if = "is_empty_str")]
    pub trusted_ca_file: String,
    /// 服务名。
    #[serde(rename = "serverName", skip_serializing_if = "is_empty_str")]
    pub server_name: String,
}

/// 服务端 TLS。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsServer {
    /// 仅接受 TLS。
    #[serde(skip_serializing_if = "is_false")]
    pub force: bool,
    /// 证书文件。
    #[serde(rename = "certFile", skip_serializing_if = "is_empty_str")]
    pub cert_file: String,
    /// 私钥文件。
    #[serde(rename = "keyFile", skip_serializing_if = "is_empty_str")]
    pub key_file: String,
    /// 受信 CA。
    #[serde(rename = "trustedCaFile", skip_serializing_if = "is_empty_str")]
    pub trusted_ca_file: String,
    /// 服务名。
    #[serde(rename = "serverName", skip_serializing_if = "is_empty_str")]
    pub server_name: String,
}

/// 客户端传输层。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientTransport {
    /// tcp/kcp/quic/websocket/wss。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub protocol: String,
    /// v1 / v2。
    #[serde(rename = "wireProtocol", skip_serializing_if = "is_empty_str")]
    pub wire_protocol: String,
    /// 拨号超时（秒）。
    #[serde(rename = "dialServerTimeout", skip_serializing_if = "is_zero")]
    pub dial_server_timeout: i64,
    /// TCP keepalive（秒，负数关闭）。
    #[serde(rename = "dialServerKeepalive", skip_serializing_if = "is_zero")]
    pub dial_server_keepalive: i64,
    /// 本地绑定 IP。
    #[serde(rename = "connectServerLocalIP", skip_serializing_if = "is_empty_str")]
    pub connect_server_local_ip: String,
    /// 代理 URL。
    #[serde(rename = "proxyURL", skip_serializing_if = "is_empty_str")]
    pub proxy_url: String,
    /// 预建连接数。
    #[serde(rename = "poolCount", skip_serializing_if = "is_zero")]
    pub pool_count: i64,
    /// 多路复用（默认 true）。
    #[serde(rename = "tcpMux", skip_serializing_if = "Option::is_none")]
    pub tcp_mux: Option<bool>,
    /// 多路复用保活间隔（秒）。
    #[serde(rename = "tcpMuxKeepaliveInterval", skip_serializing_if = "is_zero")]
    pub tcp_mux_keepalive_interval: i64,
    /// QUIC 参数。
    #[serde(skip_serializing_if = "is_default")]
    pub quic: QuicOptions,
    /// 心跳间隔（秒）。
    #[serde(rename = "heartbeatInterval", skip_serializing_if = "is_zero")]
    pub heartbeat_interval: i64,
    /// 心跳超时（秒）。
    #[serde(rename = "heartbeatTimeout", skip_serializing_if = "is_zero")]
    pub heartbeat_timeout: i64,
    /// TLS。
    #[serde(skip_serializing_if = "is_default")]
    pub tls: TlsClient,
}

/// 服务端传输层。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerTransport {
    /// 多路复用（默认 true）。
    #[serde(rename = "tcpMux", skip_serializing_if = "Option::is_none")]
    pub tcp_mux: Option<bool>,
    /// 多路复用保活间隔（秒）。
    #[serde(rename = "tcpMuxKeepaliveInterval", skip_serializing_if = "is_zero")]
    pub tcp_mux_keepalive_interval: i64,
    /// TCP keepalive（秒）。
    #[serde(rename = "tcpKeepalive", skip_serializing_if = "is_zero")]
    pub tcp_keepalive: i64,
    /// 每代理最大连接池。
    #[serde(rename = "maxPoolCount", skip_serializing_if = "is_zero")]
    pub max_pool_count: i64,
    /// 心跳超时（秒）。
    #[serde(rename = "heartbeatTimeout", skip_serializing_if = "is_zero")]
    pub heartbeat_timeout: i64,
    /// QUIC 参数。
    #[serde(skip_serializing_if = "is_default")]
    pub quic: QuicOptions,
    /// TLS。
    #[serde(skip_serializing_if = "is_default")]
    pub tls: TlsServer,
}

/// 虚拟网络（frp alpha 特性）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VirtualNet {
    /// 地址。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub address: String,
}

/// 内置 store。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreConfig {
    /// 存储路径。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub path: String,
}

/// 头部操作。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HeaderOperations {
    /// 要设置的头。
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
}

/// 健康检查的 HTTP 头。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpHeader {
    /// 名称。
    pub name: String,
    /// 值。
    pub value: String,
}

/// 代理传输选项。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyTransport {
    /// 加密。
    #[serde(rename = "useEncryption", skip_serializing_if = "is_false")]
    pub use_encryption: bool,
    /// 压缩。
    #[serde(rename = "useCompression", skip_serializing_if = "is_false")]
    pub use_compression: bool,
    /// 带宽限制。
    #[serde(rename = "bandwidthLimit", skip_serializing_if = "Bandwidth::is_empty")]
    pub bandwidth_limit: Bandwidth,
    /// `client` / `server`。
    #[serde(rename = "bandwidthLimitMode", skip_serializing_if = "is_empty_str")]
    pub bandwidth_limit_mode: String,
    /// `v1` / `v2` / 空。
    #[serde(rename = "proxyProtocolVersion", skip_serializing_if = "is_empty_str")]
    pub proxy_protocol_version: String,
}

/// 负载均衡。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LoadBalancer {
    /// 分组。
    pub group: String,
    /// 分组密钥。
    #[serde(rename = "groupKey", skip_serializing_if = "is_empty_str")]
    pub group_key: String,
}

/// 健康检查。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthCheck {
    /// `tcp` / `http` / 空。
    #[serde(rename = "type")]
    pub kind: String,
    /// 超时（秒，默认 3）。
    #[serde(rename = "timeoutSeconds", skip_serializing_if = "is_zero")]
    pub timeout_seconds: i64,
    /// 最大失败次数（默认 1）。
    #[serde(rename = "maxFailed", skip_serializing_if = "is_zero")]
    pub max_failed: i64,
    /// 间隔（秒，默认 10）。
    #[serde(rename = "intervalSeconds")]
    pub interval_seconds: i64,
    /// HTTP 路径。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub path: String,
    /// HTTP 头。
    #[serde(rename = "httpHeaders", skip_serializing_if = "Vec::is_empty")]
    pub http_headers: Vec<HttpHeader>,
}

/// 插件（代理 / 访问者的 `plugin`）。字段为各插件类型的并集。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Plugin {
    /// 插件类型。
    #[serde(rename = "type")]
    pub kind: String,
    /// 本地地址。
    #[serde(rename = "localAddr", skip_serializing_if = "is_empty_str")]
    pub local_addr: String,
    /// Host 改写。
    #[serde(rename = "hostHeaderRewrite", skip_serializing_if = "is_empty_str")]
    pub host_header_rewrite: String,
    /// 请求头。
    #[serde(rename = "requestHeaders", skip_serializing_if = "is_default_headers")]
    pub request_headers: HeaderOperations,
    /// 启用 HTTP/2（默认 true）。
    #[serde(rename = "enableHTTP2", skip_serializing_if = "Option::is_none")]
    pub enable_http2: Option<bool>,
    /// 证书路径。
    #[serde(rename = "crtPath", skip_serializing_if = "is_empty_str")]
    pub crt_path: String,
    /// 私钥路径。
    #[serde(rename = "keyPath", skip_serializing_if = "is_empty_str")]
    pub key_path: String,
    /// HTTP Basic 用户（http_proxy / static_file）。
    #[serde(rename = "httpUser", skip_serializing_if = "is_empty_str")]
    pub http_user: String,
    /// HTTP Basic 密码。
    #[serde(rename = "httpPassword", skip_serializing_if = "is_empty_str")]
    pub http_password: String,
    /// socks5 用户名。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub username: String,
    /// socks5 密码。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub password: String,
    /// static_file 本地路径。
    #[serde(rename = "localPath", skip_serializing_if = "is_empty_str")]
    pub local_path: String,
    /// static_file 去除前缀。
    #[serde(rename = "stripPrefix", skip_serializing_if = "is_empty_str")]
    pub strip_prefix: String,
    /// unix socket 路径。
    #[serde(rename = "unixPath", skip_serializing_if = "is_empty_str")]
    pub unix_path: String,
    /// virtual_net 访问者插件的目的 IP。
    #[serde(rename = "destinationIP", skip_serializing_if = "is_empty_str")]
    pub destination_ip: String,
}

/// NAT 穿透选项。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NatTraversal {
    /// 仅用 STUN 公网地址。
    #[serde(rename = "disableAssistedAddrs", skip_serializing_if = "is_false")]
    pub disable_assisted_addrs: bool,
}

/// 代理（`[[proxies]]`）。所有类型字段的超集，类型相关的字段合法性见 [`super::strict`]。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Proxy {
    /// 名称。
    pub name: String,
    /// tcp/udp/http/https/tcpmux/stcp/xtcp/sudp。
    #[serde(rename = "type")]
    pub kind: String,
    /// 是否启用（缺省 = 启用）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 注解。
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
    /// 传输选项。
    #[serde(skip_serializing_if = "is_default")]
    pub transport: ProxyTransport,
    /// 元数据。
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub metadatas: BTreeMap<String, String>,
    /// 负载均衡。
    #[serde(rename = "loadBalancer", skip_serializing_if = "is_default")]
    pub load_balancer: LoadBalancer,
    /// 健康检查。
    #[serde(rename = "healthCheck", skip_serializing_if = "is_default")]
    pub health_check: HealthCheck,
    /// 本地地址（默认 127.0.0.1）。
    #[serde(rename = "localIP", skip_serializing_if = "is_empty_str")]
    pub local_ip: String,
    /// 本地端口。
    #[serde(rename = "localPort", skip_serializing_if = "is_zero")]
    pub local_port: i64,
    /// 插件。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<Plugin>,
    /// 远程端口（tcp/udp）。
    #[serde(rename = "remotePort", skip_serializing_if = "is_zero")]
    pub remote_port: i64,
    /// 自定义域名。
    #[serde(rename = "customDomains", skip_serializing_if = "Vec::is_empty")]
    pub custom_domains: Vec<String>,
    /// 子域名。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub subdomain: String,
    /// 路径前缀。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<String>,
    /// Basic Auth 用户。
    #[serde(rename = "httpUser", skip_serializing_if = "is_empty_str")]
    pub http_user: String,
    /// Basic Auth 密码。
    #[serde(rename = "httpPassword", skip_serializing_if = "is_empty_str")]
    pub http_password: String,
    /// Host 改写。
    #[serde(rename = "hostHeaderRewrite", skip_serializing_if = "is_empty_str")]
    pub host_header_rewrite: String,
    /// 请求头。
    #[serde(rename = "requestHeaders", skip_serializing_if = "is_default_headers")]
    pub request_headers: HeaderOperations,
    /// 响应头。
    #[serde(rename = "responseHeaders", skip_serializing_if = "is_default_headers")]
    pub response_headers: HeaderOperations,
    /// 按 HTTP 用户路由。
    #[serde(rename = "routeByHTTPUser", skip_serializing_if = "is_empty_str")]
    pub route_by_http_user: String,
    /// tcpmux 复用器。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub multiplexer: String,
    /// RustFRP 扩展：流隧道最大并发连接或 UDP 最大活跃源地址数，0 = 不限。
    #[serde(rename = "maxConnections", skip_serializing_if = "is_zero")]
    pub max_connections: i64,
    /// stcp/xtcp/sudp 密钥。
    #[serde(rename = "secretKey", skip_serializing_if = "is_empty_str")]
    pub secret_key: String,
    /// 允许访问的用户。
    #[serde(rename = "allowUsers", skip_serializing_if = "Vec::is_empty")]
    pub allow_users: Vec<String>,
    /// xtcp NAT 穿透。
    #[serde(rename = "natTraversal", skip_serializing_if = "Option::is_none")]
    pub nat_traversal: Option<NatTraversal>,
}

/// 访问者传输选项。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VisitorTransport {
    /// 加密。
    #[serde(rename = "useEncryption", skip_serializing_if = "is_false")]
    pub use_encryption: bool,
    /// 压缩。
    #[serde(rename = "useCompression", skip_serializing_if = "is_false")]
    pub use_compression: bool,
}

/// 访问者（`[[visitors]]`）：stcp / xtcp / sudp 字段的超集。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Visitor {
    /// 名称。
    pub name: String,
    /// stcp/xtcp/sudp。
    #[serde(rename = "type")]
    pub kind: String,
    /// 是否启用。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 传输选项。
    #[serde(skip_serializing_if = "is_default")]
    pub transport: VisitorTransport,
    /// 密钥。
    #[serde(rename = "secretKey", skip_serializing_if = "is_empty_str")]
    pub secret_key: String,
    /// 服务端用户。
    #[serde(rename = "serverUser", skip_serializing_if = "is_empty_str")]
    pub server_user: String,
    /// 服务端代理名。
    #[serde(rename = "serverName", skip_serializing_if = "is_empty_str")]
    pub server_name: String,
    /// 绑定地址。
    #[serde(rename = "bindAddr", skip_serializing_if = "is_empty_str")]
    pub bind_addr: String,
    /// 绑定端口（可为负）。
    #[serde(rename = "bindPort", skip_serializing_if = "is_zero")]
    pub bind_port: i64,
    /// 插件。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<Plugin>,
    /// xtcp：kcp / quic。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub protocol: String,
    /// xtcp：保持隧道。
    #[serde(rename = "keepTunnelOpen", skip_serializing_if = "is_false")]
    pub keep_tunnel_open: bool,
    /// xtcp：每小时最大重试。
    #[serde(rename = "maxRetriesAnHour", skip_serializing_if = "is_zero")]
    pub max_retries_an_hour: i64,
    /// xtcp：最小重试间隔。
    #[serde(rename = "minRetryInterval", skip_serializing_if = "is_zero")]
    pub min_retry_interval: i64,
    /// xtcp：回落到的访问者。
    #[serde(rename = "fallbackTo", skip_serializing_if = "is_empty_str")]
    pub fallback_to: String,
    /// xtcp：回落超时。
    #[serde(rename = "fallbackTimeoutMs", skip_serializing_if = "is_zero")]
    pub fallback_timeout_ms: i64,
    /// xtcp：NAT 穿透。
    #[serde(rename = "natTraversal", skip_serializing_if = "Option::is_none")]
    pub nat_traversal: Option<NatTraversal>,
}

/// 客户端配置（frpc）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    /// API 版本标记（`version`）。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub version: String,
    /// 鉴权。
    #[serde(skip_serializing_if = "is_default")]
    pub auth: AuthClient,
    /// 用户前缀。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub user: String,
    /// 客户端 ID。
    #[serde(rename = "clientID", skip_serializing_if = "is_empty_str")]
    pub client_id: String,
    /// 服务端地址。
    #[serde(rename = "serverAddr", skip_serializing_if = "is_empty_str")]
    pub server_addr: String,
    /// 服务端端口。
    #[serde(rename = "serverPort", skip_serializing_if = "is_zero")]
    pub server_port: i64,
    /// STUN 服务器。
    #[serde(rename = "natHoleStunServer", skip_serializing_if = "is_empty_str")]
    pub nat_hole_stun_server: String,
    /// DNS 服务器。
    #[serde(rename = "dnsServer", skip_serializing_if = "is_empty_str")]
    pub dns_server: String,
    /// 首次登录失败是否退出（默认 true）。
    #[serde(rename = "loginFailExit", skip_serializing_if = "Option::is_none")]
    pub login_fail_exit: Option<bool>,
    /// 仅启动这些名字的代理。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub start: Vec<String>,
    /// 日志。
    #[serde(skip_serializing_if = "is_default")]
    pub log: LogConfig,
    /// 管理页面。
    #[serde(rename = "webServer", skip_serializing_if = "is_default")]
    pub web_server: WebServerConfig,
    /// 传输层。
    #[serde(skip_serializing_if = "is_default")]
    pub transport: ClientTransport,
    /// 虚拟网络。
    #[serde(rename = "virtualNet", skip_serializing_if = "is_default")]
    pub virtual_net: VirtualNet,
    /// 特性开关。
    #[serde(rename = "featureGates", skip_serializing_if = "BTreeMap::is_empty")]
    pub feature_gates: BTreeMap<String, bool>,
    /// UDP 包大小（默认 1500）。
    #[serde(rename = "udpPacketSize", skip_serializing_if = "is_zero")]
    pub udp_packet_size: i64,
    /// 元数据。
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub metadatas: BTreeMap<String, String>,
    /// 引入的其它配置文件。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<String>,
    /// 内置 store。
    #[serde(skip_serializing_if = "is_default")]
    pub store: StoreConfig,
    /// 代理。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proxies: Vec<Proxy>,
    /// 访问者。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub visitors: Vec<Visitor>,
}

/// SSH 隧道网关。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SshTunnelGateway {
    /// 绑定端口。
    #[serde(rename = "bindPort", skip_serializing_if = "is_zero")]
    pub bind_port: i64,
    /// 私钥文件。
    #[serde(rename = "privateKeyFile", skip_serializing_if = "is_empty_str")]
    pub private_key_file: String,
    /// 自动生成私钥路径。
    #[serde(rename = "autoGenPrivateKeyPath", skip_serializing_if = "is_empty_str")]
    pub auto_gen_private_key_path: String,
    /// authorized_keys 文件。
    #[serde(rename = "authorizedKeysFile", skip_serializing_if = "is_empty_str")]
    pub authorized_keys_file: String,
}

/// 服务端 HTTP 插件。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpPluginOptions {
    /// 名称。
    pub name: String,
    /// 地址。
    pub addr: String,
    /// 路径。
    pub path: String,
    /// 操作列表。
    pub ops: Vec<String>,
    /// 校验 TLS。
    #[serde(rename = "tlsVerify", skip_serializing_if = "is_false")]
    pub tls_verify: bool,
}

/// 服务端配置（frps）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// API 版本标记。
    #[serde(skip_serializing_if = "is_empty_str")]
    pub version: String,
    /// 鉴权。
    #[serde(skip_serializing_if = "is_default")]
    pub auth: AuthServer,
    /// 绑定地址。
    #[serde(rename = "bindAddr", skip_serializing_if = "is_empty_str")]
    pub bind_addr: String,
    /// 绑定端口。
    #[serde(rename = "bindPort", skip_serializing_if = "is_zero")]
    pub bind_port: i64,
    /// KCP 端口。
    #[serde(rename = "kcpBindPort", skip_serializing_if = "is_zero")]
    pub kcp_bind_port: i64,
    /// QUIC 端口。
    #[serde(rename = "quicBindPort", skip_serializing_if = "is_zero")]
    pub quic_bind_port: i64,
    /// 代理绑定地址。
    #[serde(rename = "proxyBindAddr", skip_serializing_if = "is_empty_str")]
    pub proxy_bind_addr: String,
    /// vhost HTTP 端口。
    #[serde(rename = "vhostHTTPPort", skip_serializing_if = "is_zero")]
    pub vhost_http_port: i64,
    /// vhost HTTP 超时（秒，默认 60）。
    #[serde(rename = "vhostHTTPTimeout", skip_serializing_if = "is_zero")]
    pub vhost_http_timeout: i64,
    /// vhost HTTPS 端口。
    #[serde(rename = "vhostHTTPSPort", skip_serializing_if = "is_zero")]
    pub vhost_https_port: i64,
    /// tcpmux HTTP CONNECT 端口。
    #[serde(rename = "tcpmuxHTTPConnectPort", skip_serializing_if = "is_zero")]
    pub tcpmux_http_connect_port: i64,
    /// tcpmux 透传。
    #[serde(rename = "tcpmuxPassthrough", skip_serializing_if = "is_false")]
    pub tcpmux_passthrough: bool,
    /// RustFRP 扩展：单设备的并发 TCP 连接上限，0 = 不限。
    #[serde(rename = "maxConnectionsPerClient", skip_serializing_if = "is_zero")]
    pub max_connections_per_client: i64,
    /// 子域名后缀。
    #[serde(rename = "subDomainHost", skip_serializing_if = "is_empty_str")]
    pub sub_domain_host: String,
    /// 自定义 404 页面。
    #[serde(rename = "custom404Page", skip_serializing_if = "is_empty_str")]
    pub custom_404_page: String,
    /// SSH 隧道网关。
    #[serde(rename = "sshTunnelGateway", skip_serializing_if = "is_default")]
    pub ssh_tunnel_gateway: SshTunnelGateway,
    /// 管理页面。
    #[serde(rename = "webServer", skip_serializing_if = "is_default")]
    pub web_server: WebServerConfig,
    /// Prometheus。
    #[serde(rename = "enablePrometheus", skip_serializing_if = "is_false")]
    pub enable_prometheus: bool,
    /// 日志。
    #[serde(skip_serializing_if = "is_default")]
    pub log: LogConfig,
    /// 传输层。
    #[serde(skip_serializing_if = "is_default")]
    pub transport: ServerTransport,
    /// 向客户端返回详细错误（默认 true）。
    #[serde(
        rename = "detailedErrorsToClient",
        skip_serializing_if = "Option::is_none"
    )]
    pub detailed_errors_to_client: Option<bool>,
    /// 单客户端最大端口数。
    #[serde(rename = "maxPortsPerClient", skip_serializing_if = "is_zero")]
    pub max_ports_per_client: i64,
    /// 用户连接超时（秒，默认 10）。
    #[serde(rename = "userConnTimeout", skip_serializing_if = "is_zero")]
    pub user_conn_timeout: i64,
    /// UDP 包大小。
    #[serde(rename = "udpPacketSize", skip_serializing_if = "is_zero")]
    pub udp_packet_size: i64,
    /// NAT 分析数据保留小时数。
    #[serde(
        rename = "natholeAnalysisDataReserveHours",
        skip_serializing_if = "is_zero"
    )]
    pub nathole_analysis_data_reserve_hours: i64,
    /// 允许的端口。
    #[serde(rename = "allowPorts", skip_serializing_if = "Vec::is_empty")]
    pub allow_ports: Vec<PortsRange>,
    /// HTTP 插件。
    #[serde(rename = "httpPlugins", skip_serializing_if = "Vec::is_empty")]
    pub http_plugins: Vec<HttpPluginOptions>,
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn bandwidth_parse() {
        assert!(Bandwidth::parse("").unwrap().is_empty());
        assert!(Bandwidth::parse("  ").unwrap().is_empty());
        let mb = Bandwidth::parse("2MB").unwrap();
        assert_eq!((mb.bytes(), mb.as_str()), (2 * 1024 * 1024, "2MB"));
        assert_eq!(Bandwidth::parse("1.5KB").unwrap().bytes(), 1536);
        assert_eq!(Bandwidth::parse("10GB").unwrap_err(), "unit not support");
        assert!(Bandwidth::parse("xMB").is_err());
    }

    #[test]
    fn bandwidth_serde_roundtrip() {
        let t = ProxyTransport {
            bandwidth_limit: Bandwidth::parse("1MB").unwrap(),
            ..Default::default()
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains(r#""bandwidthLimit":"1MB""#));
        assert_eq!(serde_json::from_str::<ProxyTransport>(&json).unwrap(), t);
        assert!(serde_json::from_str::<ProxyTransport>(r#"{"bandwidthLimit":"5GB"}"#).is_err());
    }

    #[test]
    fn defaults_serialize_to_empty_object() {
        assert_eq!(
            serde_json::to_string(&ClientConfig::default()).unwrap(),
            "{}"
        );
        assert_eq!(
            serde_json::to_string(&ServerConfig::default()).unwrap(),
            "{}"
        );
    }

    #[test]
    fn helper_predicates() {
        assert!(is_zero(&0) && !is_zero(&1));
        assert!(is_false(&false) && !is_false(&true));
        assert!(is_empty_str("") && !is_empty_str("x"));
        assert!(is_default_headers(&HeaderOperations::default()));
        let mut h = HeaderOperations::default();
        h.set.insert("a".into(), "b".into());
        assert!(!is_default_headers(&h));
    }
}
