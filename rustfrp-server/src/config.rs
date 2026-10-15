//! 运行配置（环境变量 + 默认值）。

use std::net::SocketAddr;
use std::path::PathBuf;

/// 服务配置。
#[derive(Debug, Clone)]
pub struct Config {
    /// 数据库文件。
    pub db_path: PathBuf,
    /// 管理 API / 面板监听地址（默认仅本机）。
    pub http_addr: String,
    /// 内置 Edge 监听地址（Client 连接）。
    pub edge_addr: String,
    /// 是否启动内置 Edge（默认开启）。关闭后 Controller 只做管理面，设备必须接入独立节点。
    pub builtin_edge: bool,
    /// Prometheus `/metrics` 的 Bearer 令牌；空 = 不提供该端点。
    pub metrics_token: String,
    /// 节点端口（外置 Edge 接入 Controller 的监听地址）；空 = 不启用。
    pub node_addr: String,
    /// 内置 Edge 的代理端口绑定地址。
    pub proxy_bind_addr: String,
    /// 允许分配给隧道的远程端口区间；空 = 不限。
    pub allow_ports: Vec<(u16, u16)>,
    /// 禁止分配给隧道的端口区间（在 `allow_ports` 之上再剔除；Controller 自身监听的端口总是保留）。
    pub reserved_ports: Vec<(u16, u16)>,
    /// 单个客户端最多的隧道数；0 = 不限。
    pub max_tunnels_per_client: i64,
    /// 单客户端共享并发流 / UDP 活跃源地址上限；0 = 不限。
    pub max_connections_per_client: usize,
    /// 对外可访问的主机名（安装命令里使用）。
    pub public_host: String,
    /// 面板对外访问地址（含协议），用于生成安装脚本命令；空则不生成。
    pub public_url: String,
    /// 共享 Web UI 静态目录；空则不托管。
    pub panel_dir: Option<PathBuf>,
    /// Cookie 是否加 Secure（经 HTTPS 访问时必须开启）。
    pub secure_cookie: bool,
    /// 管理会话有效期（毫秒）。
    pub session_ttl_ms: i64,
    /// HTTP 虚拟主机端口（`None` = 不启用）。
    pub vhost_http: Option<u16>,
    /// HTTPS（SNI 透传）虚拟主机端口。
    pub vhost_https: Option<u16>,
    /// TCPMUX HTTP CONNECT 共享端口。
    pub tcpmux_http_connect_port: Option<u16>,
    /// Whether to forward CONNECT requests unchanged to the selected Client.
    pub tcpmux_passthrough: bool,
    /// 子域名后缀（`*.tunnel.example.com` 的 `tunnel.example.com`）。
    pub subdomain_host: String,
    /// 审计保留天数；0 保留全部。
    pub audit_retention_days: i64,
    /// 流量分钟桶保留天数；0 保留全部。
    pub traffic_retention_days: i64,
    /// 每轮、每表清理上限。
    pub cleanup_batch: i64,
    /// 内置 Edge 的 PEM 证书链与私钥，必须成对配置。
    pub edge_tls_cert: Option<PathBuf>,
    pub edge_tls_key: Option<PathBuf>,
    /// 启动时由实际 Edge 证书生成，不从环境变量接受。
    pub edge_tls_fingerprint: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: PathBuf::from("rustfrp.db"),
            http_addr: "127.0.0.1:8080".into(),
            edge_addr: "0.0.0.0:7000".into(),
            builtin_edge: true,
            metrics_token: String::new(),
            node_addr: String::new(),
            proxy_bind_addr: "0.0.0.0".into(),
            allow_ports: Vec::new(),
            reserved_ports: Vec::new(),
            max_tunnels_per_client: 0,
            max_connections_per_client: 0,
            public_host: "127.0.0.1".into(),
            public_url: String::new(),
            panel_dir: None,
            secure_cookie: false,
            session_ttl_ms: 8 * 3600 * 1000,
            vhost_http: None,
            vhost_https: None,
            tcpmux_http_connect_port: None,
            tcpmux_passthrough: false,
            subdomain_host: String::new(),
            audit_retention_days: 90,
            traffic_retention_days: 7,
            cleanup_batch: 500,
            edge_tls_cert: None,
            edge_tls_key: None,
            edge_tls_fingerprint: String::new(),
        }
    }
}

/// 解析 `6000-6100,7000` 形式的端口区间。
pub fn parse_ports(s: &str) -> Result<Vec<(u16, u16)>, String> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let bad = |_| format!("invalid port range \"{part}\"");
        let (lo, hi) = match part.split_once('-') {
            Some((a, b)) => (
                a.trim().parse::<u16>().map_err(bad)?,
                b.trim().parse::<u16>().map_err(bad)?,
            ),
            None => {
                let p = part.parse::<u16>().map_err(bad)?;
                (p, p)
            }
        };
        if lo > hi {
            return Err(format!("invalid port range \"{part}\""));
        }
        out.push((lo, hi));
    }
    Ok(out)
}

impl Config {
    /// 从「键 → 值」查询函数构造（便于测试，不直接读进程环境）。
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let mut c = Config {
            edge_tls_cert: get("RUSTFRP_EDGE_TLS_CERT")
                .filter(|v| !v.trim().is_empty())
                .map(PathBuf::from),
            edge_tls_key: get("RUSTFRP_EDGE_TLS_KEY")
                .filter(|v| !v.trim().is_empty())
                .map(PathBuf::from),
            ..Default::default()
        };
        if c.edge_tls_cert.is_some() != c.edge_tls_key.is_some() {
            return Err(
                "RUSTFRP_EDGE_TLS_CERT and RUSTFRP_EDGE_TLS_KEY must be configured together".into(),
            );
        }
        if let Some(v) = get("RUSTFRP_DB") {
            c.db_path = v.into();
        }
        if let Some(v) = get("RUSTFRP_HTTP") {
            c.http_addr = v;
        }
        if let Some(v) = get("RUSTFRP_EDGE") {
            c.edge_addr = v;
        }
        if let Some(v) = get("RUSTFRP_BUILTIN_EDGE") {
            c.builtin_edge = !matches!(v.trim(), "0" | "false" | "no" | "off");
        }
        if let Some(v) = get("RUSTFRP_METRICS_TOKEN") {
            let t = v.trim();
            if !t.is_empty() && (t.len() < 16 || t.chars().any(char::is_whitespace)) {
                return Err(
                    "RUSTFRP_METRICS_TOKEN must be at least 16 characters without spaces".into(),
                );
            }
            c.metrics_token = t.to_string();
        }
        if let Some(v) = get("RUSTFRP_NODE_ADDR") {
            c.node_addr = v.trim().to_string();
        }
        if let Some(v) = get("RUSTFRP_PROXY_BIND") {
            c.proxy_bind_addr = v;
        }
        if let Some(v) = get("RUSTFRP_ALLOW_PORTS") {
            c.allow_ports = parse_ports(&v)?;
        }
        if let Some(v) = get("RUSTFRP_RESERVED_PORTS") {
            c.reserved_ports = parse_ports(&v)?;
        }
        if let Some(v) = get("RUSTFRP_PUBLIC_HOST") {
            c.public_host = v;
        }
        if let Some(v) = get("RUSTFRP_PUBLIC_URL") {
            c.public_url = v.trim_end_matches('/').to_string();
        }
        if let Some(v) = get("RUSTFRP_PANEL_DIR").filter(|v| !v.is_empty()) {
            c.panel_dir = Some(v.into());
        }
        if let Some(v) = get("RUSTFRP_SECURE_COOKIE") {
            c.secure_cookie = matches!(v.as_str(), "1" | "true" | "yes");
        }
        for (key, slot) in [
            ("RUSTFRP_VHOST_HTTP", &mut c.vhost_http),
            ("RUSTFRP_VHOST_HTTPS", &mut c.vhost_https),
            (
                "RUSTFRP_TCPMUX_HTTP_CONNECT_PORT",
                &mut c.tcpmux_http_connect_port,
            ),
        ] {
            if let Some(v) = get(key).filter(|v| !v.is_empty()) {
                *slot = Some(
                    v.parse::<u16>()
                        .map_err(|_| format!("invalid {key} \"{v}\""))?,
                );
            }
        }
        if let Some(v) = get("RUSTFRP_TCPMUX_PASSTHROUGH") {
            c.tcpmux_passthrough = match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => return Err("invalid RUSTFRP_TCPMUX_PASSTHROUGH".into()),
            };
        }
        if let Some(v) = get("RUSTFRP_MAX_CONNECTIONS_PER_CLIENT") {
            c.max_connections_per_client = v
                .parse::<usize>()
                .map_err(|_| "invalid RUSTFRP_MAX_CONNECTIONS_PER_CLIENT".to_string())?;
            if c.max_connections_per_client > 1_000_000 {
                return Err("RUSTFRP_MAX_CONNECTIONS_PER_CLIENT must be 0..1000000".into());
            }
        }
        if let Some(v) = get("RUSTFRP_SUBDOMAIN_HOST") {
            c.subdomain_host = v.trim().trim_matches('.').to_ascii_lowercase();
        }
        if let Some(v) = get("RUSTFRP_SESSION_HOURS") {
            let h: i64 = v
                .parse()
                .map_err(|_| format!("invalid RUSTFRP_SESSION_HOURS \"{v}\""))?;
            if !(1..=720).contains(&h) {
                return Err("RUSTFRP_SESSION_HOURS must be 1..720".into());
            }
            c.session_ttl_ms = h * 3600 * 1000;
        }
        for (key, slot, min, max) in [
            (
                "RUSTFRP_AUDIT_RETENTION_DAYS",
                &mut c.audit_retention_days,
                0,
                3650,
            ),
            (
                "RUSTFRP_TRAFFIC_RETENTION_DAYS",
                &mut c.traffic_retention_days,
                0,
                3650,
            ),
            (
                "RUSTFRP_MAX_TUNNELS_PER_CLIENT",
                &mut c.max_tunnels_per_client,
                0,
                10_000,
            ),
            (
                "RUSTFRP_CLEANUP_BATCH",
                &mut c.cleanup_batch,
                1,
                crate::maintenance::MAX_BATCH,
            ),
        ] {
            if let Some(value) = get(key) {
                let n = value.parse::<i64>().map_err(|_| format!("invalid {key}"))?;
                if !(min..=max).contains(&n) {
                    return Err(format!("{key} must be {min}..{max}"));
                }
                *slot = n;
            }
        }
        if !loopback_http(&c.http_addr)
            && (!c.secure_cookie || !c.public_url.starts_with("https://"))
        {
            return Err(
                "non-loopback RUSTFRP_HTTP requires RUSTFRP_SECURE_COOKIE=true and an https:// RUSTFRP_PUBLIC_URL behind a TLS reverse proxy".into(),
            );
        }
        Ok(c)
    }

    /// 远程端口是否可分配：`0`（由服务器分配）总是通过；否则必须落在 `allow_ports` 内（若配置），
    /// 且不得命中显式保留区间或 Controller 自身的监听端口（Edge / 管理 HTTP / vhost）。
    pub fn check_remote_port(&self, port: i64) -> Result<(), String> {
        if port == 0 {
            return Ok(());
        }
        let within = |ranges: &[(u16, u16)]| {
            ranges
                .iter()
                .any(|(lo, hi)| (i64::from(*lo)..=i64::from(*hi)).contains(&port))
        };
        if !self.allow_ports.is_empty() && !within(&self.allow_ports) {
            return Err(format!(
                "remote port {port} is outside the ports this server allows"
            ));
        }
        let listening = [
            Some(self.edge_port()),
            self.vhost_http,
            self.vhost_https,
            self.tcpmux_http_connect_port,
            self.http_addr
                .rsplit(':')
                .next()
                .and_then(|p| p.parse().ok()),
        ];
        if within(&self.reserved_ports) || listening.iter().flatten().any(|p| i64::from(*p) == port)
        {
            return Err(format!("remote port {port} is reserved by the server"));
        }
        Ok(())
    }

    /// 内置 Edge 的监听端口（用于生成安装命令）。
    pub fn edge_port(&self) -> u16 {
        self.edge_addr
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(7000)
    }
}

fn loopback_http(addr: &str) -> bool {
    addr.parse::<SocketAddr>()
        .map(|s| s.ip().is_loopback())
        .unwrap_or_else(|_| {
            addr.strip_prefix("localhost:").is_some()
                || addr.starts_with("127.")
                || addr.starts_with("[::1]:")
        })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn retention_config_is_bounded() {
        let defaults = cfg(&[]).unwrap();
        assert_eq!(defaults.audit_retention_days, 90);
        assert_eq!(defaults.traffic_retention_days, 7);
        assert_eq!(defaults.cleanup_batch, 500);
        assert_eq!(
            cfg(&[
                ("RUSTFRP_AUDIT_RETENTION_DAYS", "0"),
                ("RUSTFRP_CLEANUP_BATCH", "1")
            ])
            .unwrap()
            .audit_retention_days,
            0
        );
        assert_eq!(
            cfg(&[
                ("RUSTFRP_AUDIT_RETENTION_DAYS", "3650"),
                ("RUSTFRP_TRAFFIC_RETENTION_DAYS", "0"),
                ("RUSTFRP_CLEANUP_BATCH", "1000")
            ])
            .unwrap()
            .cleanup_batch,
            1000
        );
        assert_eq!(
            cfg(&[("RUSTFRP_TRAFFIC_RETENTION_DAYS", "3650")])
                .unwrap()
                .traffic_retention_days,
            3650
        );
        for (key, value) in [
            ("RUSTFRP_AUDIT_RETENTION_DAYS", "bad"),
            ("RUSTFRP_AUDIT_RETENTION_DAYS", "-1"),
            ("RUSTFRP_AUDIT_RETENTION_DAYS", "3651"),
            ("RUSTFRP_CLEANUP_BATCH", "0"),
            ("RUSTFRP_METRICS_TOKEN", "short"),
            ("RUSTFRP_METRICS_TOKEN", "has space in it xxxxxxxxx"),
            ("RUSTFRP_MAX_TUNNELS_PER_CLIENT", "-1"),
            ("RUSTFRP_MAX_TUNNELS_PER_CLIENT", "10001"),
            ("RUSTFRP_MAX_CONNECTIONS_PER_CLIENT", "1000001"),
            ("RUSTFRP_RESERVED_PORTS", "x"),
            ("RUSTFRP_CLEANUP_BATCH", "1001"),
            ("RUSTFRP_TRAFFIC_RETENTION_DAYS", "bad"),
            ("RUSTFRP_TRAFFIC_RETENTION_DAYS", "-1"),
            ("RUSTFRP_TRAFFIC_RETENTION_DAYS", "3651"),
            ("RUSTFRP_TCPMUX_PASSTHROUGH", "maybe"),
        ] {
            assert!(cfg(&[(key, value)]).is_err());
        }
    }

    #[test]
    fn public_management_listener_requires_secure_public_url() {
        assert!(cfg(&[("RUSTFRP_HTTP", "0.0.0.0:8080")]).is_err());
        assert!(cfg(&[
            ("RUSTFRP_HTTP", "0.0.0.0:8080"),
            ("RUSTFRP_SECURE_COOKIE", "true"),
            ("RUSTFRP_PUBLIC_URL", "https://panel.example.test"),
        ])
        .is_ok());
        assert!(cfg(&[("RUSTFRP_HTTP", "127.0.0.1:8080")]).is_ok());
    }

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_are_safe() {
        let c = cfg(&[]).unwrap();
        assert_eq!(
            c.http_addr, "127.0.0.1:8080",
            "the management API listens on loopback by default"
        );
        assert!(c.panel_dir.is_none() && !c.secure_cookie && c.allow_ports.is_empty());
        assert_eq!(c.edge_port(), 7000);
        assert_eq!(c.session_ttl_ms, 8 * 3600 * 1000);
    }

    #[test]
    fn overrides() {
        let c = cfg(&[
            ("RUSTFRP_DB", "/data/x.db"),
            ("RUSTFRP_HTTP", "0.0.0.0:9000"),
            ("RUSTFRP_EDGE", "0.0.0.0:7100"),
            ("RUSTFRP_NODE_ADDR", " 0.0.0.0:7200 "),
            ("RUSTFRP_BUILTIN_EDGE", "off"),
            ("RUSTFRP_METRICS_TOKEN", " 0123456789abcdef0123 "),
            ("RUSTFRP_PROXY_BIND", "10.0.0.1"),
            ("RUSTFRP_ALLOW_PORTS", "6000-6100, 7000"),
            ("RUSTFRP_RESERVED_PORTS", "6050"),
            ("RUSTFRP_MAX_TUNNELS_PER_CLIENT", "20"),
            ("RUSTFRP_PUBLIC_HOST", "frp.example.com"),
            ("RUSTFRP_PUBLIC_URL", "https://frp.example.com/"),
            ("RUSTFRP_PANEL_DIR", "/srv/panel"),
            ("RUSTFRP_SECURE_COOKIE", "true"),
            ("RUSTFRP_SESSION_HOURS", "2"),
            ("RUSTFRP_VHOST_HTTP", "8080"),
            ("RUSTFRP_VHOST_HTTPS", "8443"),
            ("RUSTFRP_TCPMUX_HTTP_CONNECT_PORT", "7443"),
            ("RUSTFRP_TCPMUX_PASSTHROUGH", "true"),
            ("RUSTFRP_MAX_CONNECTIONS_PER_CLIENT", "1500"),
            ("RUSTFRP_SUBDOMAIN_HOST", ".Tunnel.Example.com."),
        ])
        .unwrap();
        assert_eq!(c.db_path.to_str(), Some("/data/x.db"));
        assert_eq!(
            (
                c.http_addr.as_str(),
                c.edge_port(),
                c.proxy_bind_addr.as_str()
            ),
            ("0.0.0.0:9000", 7100, "10.0.0.1")
        );
        assert_eq!(c.node_addr, "0.0.0.0:7200");
        assert_eq!(c.tcpmux_http_connect_port, Some(7443));
        assert!(c.tcpmux_passthrough);
        assert_eq!(c.max_connections_per_client, 1500);
        assert!(!c.builtin_edge);
        assert_eq!(c.metrics_token, "0123456789abcdef0123");
        assert_eq!(c.allow_ports, vec![(6000, 6100), (7000, 7000)]);
        assert_eq!(
            (c.reserved_ports.clone(), c.max_tunnels_per_client),
            (vec![(6050, 6050)], 20)
        );
        assert_eq!(
            (c.public_host.as_str(), c.public_url.as_str()),
            ("frp.example.com", "https://frp.example.com")
        );
        assert!(c.panel_dir.is_some() && c.secure_cookie);
        assert_eq!(c.session_ttl_ms, 2 * 3600 * 1000);
        assert_eq!((c.vhost_http, c.vhost_https), (Some(8080), Some(8443)));
        assert_eq!(c.subdomain_host, "tunnel.example.com");
        assert!(cfg(&[("RUSTFRP_PANEL_DIR", "")])
            .unwrap()
            .panel_dir
            .is_none());
        assert!(
            !cfg(&[("RUSTFRP_SECURE_COOKIE", "no")])
                .unwrap()
                .secure_cookie
        );
        assert_eq!(
            Config {
                edge_addr: "bad".into(),
                ..Config::default()
            }
            .edge_port(),
            7000
        );
    }

    #[test]
    fn errors() {
        assert!(cfg(&[("RUSTFRP_ALLOW_PORTS", "abc")]).is_err());
        assert!(cfg(&[("RUSTFRP_ALLOW_PORTS", "5-1")]).is_err());
        assert!(cfg(&[("RUSTFRP_ALLOW_PORTS", "1-99999")]).is_err());
        assert!(cfg(&[("RUSTFRP_VHOST_HTTP", "x")]).is_err());
        assert!(cfg(&[("RUSTFRP_VHOST_HTTPS", "99999")]).is_err());
        assert!(cfg(&[("RUSTFRP_VHOST_HTTP", "")])
            .unwrap()
            .vhost_http
            .is_none());
        assert!(cfg(&[("RUSTFRP_SESSION_HOURS", "x")]).is_err());
        assert!(cfg(&[("RUSTFRP_SESSION_HOURS", "0")]).is_err());
        assert!(cfg(&[("RUSTFRP_SESSION_HOURS", "721")]).is_err());
        assert_eq!(parse_ports("").unwrap(), vec![]);
    }
    #[test]
    fn edge_certificate_configuration_requires_pair() {
        assert!(cfg(&[("RUSTFRP_EDGE_TLS_CERT", "cert.pem")]).is_err());
        assert!(cfg(&[("RUSTFRP_EDGE_TLS_KEY", "key.pem")]).is_err());
        let c = cfg(&[
            ("RUSTFRP_EDGE_TLS_CERT", "cert.pem"),
            ("RUSTFRP_EDGE_TLS_KEY", "key.pem"),
        ])
        .unwrap();
        assert_eq!(c.edge_tls_cert, Some(PathBuf::from("cert.pem")));
        assert_eq!(c.edge_tls_key, Some(PathBuf::from("key.pem")));
        let c = cfg(&[("RUSTFRP_EDGE_TLS_CERT", ""), ("RUSTFRP_EDGE_TLS_KEY", " ")]).unwrap();
        assert!(c.edge_tls_cert.is_none());
    }
}
