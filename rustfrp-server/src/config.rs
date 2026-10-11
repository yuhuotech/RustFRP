//! 运行配置（环境变量 + 默认值）。

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
    /// 内置 Edge 的代理端口绑定地址。
    pub proxy_bind_addr: String,
    /// 允许分配给隧道的远程端口区间；空 = 不限。
    pub allow_ports: Vec<(u16, u16)>,
    /// 对外可访问的主机名（安装命令里使用）。
    pub public_host: String,
    /// 面板对外访问地址（含协议），用于生成安装脚本命令；空则不生成。
    pub public_url: String,
    /// Flutter Web 面板静态目录；空则不托管。
    pub panel_dir: Option<PathBuf>,
    /// Cookie 是否加 Secure（经 HTTPS 访问时必须开启）。
    pub secure_cookie: bool,
    /// 管理会话有效期（毫秒）。
    pub session_ttl_ms: i64,
    /// HTTP 虚拟主机端口（`None` = 不启用）。
    pub vhost_http: Option<u16>,
    /// HTTPS（SNI 透传）虚拟主机端口。
    pub vhost_https: Option<u16>,
    /// 子域名后缀（`*.tunnel.example.com` 的 `tunnel.example.com`）。
    pub subdomain_host: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: PathBuf::from("rustfrp.db"),
            http_addr: "127.0.0.1:8080".into(),
            edge_addr: "0.0.0.0:7000".into(),
            proxy_bind_addr: "0.0.0.0".into(),
            allow_ports: Vec::new(),
            public_host: "127.0.0.1".into(),
            public_url: String::new(),
            panel_dir: None,
            secure_cookie: false,
            session_ttl_ms: 8 * 3600 * 1000,
            vhost_http: None,
            vhost_https: None,
            subdomain_host: String::new(),
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
        let mut c = Config::default();
        if let Some(v) = get("RUSTFRP_DB") {
            c.db_path = v.into();
        }
        if let Some(v) = get("RUSTFRP_HTTP") {
            c.http_addr = v;
        }
        if let Some(v) = get("RUSTFRP_EDGE") {
            c.edge_addr = v;
        }
        if let Some(v) = get("RUSTFRP_PROXY_BIND") {
            c.proxy_bind_addr = v;
        }
        if let Some(v) = get("RUSTFRP_ALLOW_PORTS") {
            c.allow_ports = parse_ports(&v)?;
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
        ] {
            if let Some(v) = get(key).filter(|v| !v.is_empty()) {
                *slot = Some(
                    v.parse::<u16>()
                        .map_err(|_| format!("invalid {key} \"{v}\""))?,
                );
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
        Ok(c)
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

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
            ("RUSTFRP_PROXY_BIND", "10.0.0.1"),
            ("RUSTFRP_ALLOW_PORTS", "6000-6100, 7000"),
            ("RUSTFRP_PUBLIC_HOST", "frp.example.com"),
            ("RUSTFRP_PUBLIC_URL", "https://frp.example.com/"),
            ("RUSTFRP_PANEL_DIR", "/srv/panel"),
            ("RUSTFRP_SECURE_COOKIE", "true"),
            ("RUSTFRP_SESSION_HOURS", "2"),
            ("RUSTFRP_VHOST_HTTP", "8080"),
            ("RUSTFRP_VHOST_HTTPS", "8443"),
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
        assert_eq!(c.allow_ports, vec![(6000, 6100), (7000, 7000)]);
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
}
