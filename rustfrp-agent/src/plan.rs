//! 把 frp 配置（已加载、补全默认值）转换为运行时参数。
//!
//! 这里是「配置层兼容」与「运行时实现」的边界：frp 里有而当前未实现的类型 / 选项，
//! 会被跳过并写入 `notes`，由调用方打印出来，不会静默丢弃。

use std::time::Duration;

use rustfrp_core::frpcfg::{ConfigFs, LoadedClient, LoadedServer, ValueSource};
use rustfrp_core::mux::MuxConfig;
use rustfrp_core::tls::{self, Identity};

use crate::client::{ClientConfig, ProxyCfg};
use crate::edge::EdgeConfig;

/// 客户端运行计划。
pub struct ClientPlan {
    /// 运行参数。
    pub cfg: ClientConfig,
    /// `log.to`。
    pub log_to: String,
    /// `log.level`。
    pub log_level: String,
    /// 被跳过的内容及原因。
    pub notes: Vec<String>,
}

/// 边缘节点运行计划。
pub struct EdgePlan {
    /// 运行参数。
    pub cfg: EdgeConfig,
    /// 服务端证书（未配置则运行时生成自签名）。
    pub identity: Option<Identity>,
    /// `log.to`。
    pub log_to: String,
    /// `log.level`。
    pub log_level: String,
    /// 需要提示的内容。
    pub notes: Vec<String>,
}

fn secs(v: i64, fallback: u64) -> Duration {
    Duration::from_secs(if v > 0 { v as u64 } else { fallback })
}

/// frp 的 0 表示「不启用」。
fn opt_port(v: i64, what: &str) -> Result<Option<u16>, String> {
    if v == 0 {
        return Ok(None);
    }
    port(v, what).map(Some)
}

fn port(v: i64, what: &str) -> Result<u16, String> {
    u16::try_from(v).map_err(|_| format!("{what}: port {v} out of range"))
}

/// 解析令牌：直接值，或 `tokenSource`（`file` 支持；`exec` 暂未实现）。
fn resolve_token(
    token: &str,
    source: &Option<ValueSource>,
    fs: &dyn ConfigFs,
) -> Result<String, String> {
    let Some(src) = source else {
        return Ok(token.to_string());
    };
    match (src.kind.as_str(), &src.file) {
        ("file", Some(f)) => fs
            .read_to_string(&f.path)
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("read token file {}: {e}", f.path)),
        ("exec", _) => Err("auth.tokenSource of type \"exec\" is not implemented yet".into()),
        _ => Err("invalid auth.tokenSource".into()),
    }
}

/// 该代理里配置了、但当前版本尚未实现的选项（不会静默忽略：调用方会把它们打印出来）。
fn ignored_options(p: &rustfrp_core::frpcfg::Proxy) -> Vec<&'static str> {
    let mut v = Vec::new();
    let mut flag = |on: bool, name: &'static str| {
        if on {
            v.push(name);
        }
    };
    flag(p.transport.use_encryption, "transport.useEncryption");
    flag(p.transport.use_compression, "transport.useCompression");
    flag(
        !p.transport.bandwidth_limit.is_empty(),
        "transport.bandwidthLimit",
    );
    flag(
        !p.transport.proxy_protocol_version.is_empty(),
        "transport.proxyProtocolVersion",
    );
    flag(!p.health_check.kind.is_empty(), "healthCheck");
    flag(!p.load_balancer.group.is_empty(), "loadBalancer");
    flag(!p.locations.is_empty(), "locations");
    flag(
        !p.http_user.is_empty() || !p.http_password.is_empty(),
        "httpUser / httpPassword",
    );
    flag(!p.request_headers.set.is_empty(), "requestHeaders");
    flag(!p.response_headers.set.is_empty(), "responseHeaders");
    flag(!p.route_by_http_user.is_empty(), "routeByHTTPUser");
    v
}

/// 由 frpc 配置生成运行计划。
pub fn client_plan(l: &LoadedClient, fs: &dyn ConfigFs) -> Result<ClientPlan, String> {
    let c = &l.common;
    if c.auth.method == "oidc" {
        return Err("auth.method \"oidc\" is not implemented yet".into());
    }
    let mut notes = Vec::new();
    let mut proxies = Vec::new();
    for p in &l.proxies {
        if !["tcp", "udp", "http", "https"].contains(&p.kind.as_str()) {
            notes.push(format!(
                "proxy [{}]: type \"{}\" is not supported yet, skipped",
                p.name, p.kind
            ));
            continue;
        }
        if p.plugin.as_ref().is_some_and(|x| !x.kind.is_empty()) {
            notes.push(format!(
                "proxy [{}]: plugins are not supported yet, skipped",
                p.name
            ));
            continue;
        }
        let name = if c.user.is_empty() {
            p.name.clone()
        } else {
            format!("{}.{}", c.user, p.name)
        };
        proxies.push(ProxyCfg {
            name,
            kind: p.kind.clone(),
            local_ip: p.local_ip.clone(),
            local_port: port(p.local_port, &format!("proxy [{}] localPort", p.name))?,
            remote_port: port(p.remote_port, &format!("proxy [{}] remotePort", p.name))?,
            custom_domains: p.custom_domains.clone(),
            subdomain: p.subdomain.clone(),
            host_header_rewrite: p.host_header_rewrite.clone(),
        });
        for opt in ignored_options(p) {
            notes.push(format!(
                "proxy [{}]: option {opt} is not supported yet and is ignored",
                p.name
            ));
        }
    }
    for v in &l.visitors {
        notes.push(format!(
            "visitor [{}]: visitors are not supported yet, skipped",
            v.name
        ));
    }
    let trusted_ca_pem = if c.transport.tls.trusted_ca_file.is_empty() {
        None
    } else {
        let f = &c.transport.tls.trusted_ca_file;
        Some(
            fs.read_to_string(f)
                .map_err(|e| format!("read trustedCaFile {f}: {e}"))?
                .into_bytes(),
        )
    };
    let hb_interval = if c.transport.heartbeat_interval > 0 {
        c.transport.heartbeat_interval
    } else {
        c.transport.tcp_mux_keepalive_interval
    };
    let cfg = ClientConfig {
        server_addr: c.server_addr.clone(),
        server_port: port(c.server_port, "serverPort")?,
        token: resolve_token(&c.auth.token, &c.auth.token_source, fs)?,
        user: c.user.clone(),
        client_id: c.client_id.clone(),
        trusted_ca_pem,
        login_fail_exit: c.login_fail_exit.unwrap_or(true),
        proxies,
        dial_timeout: secs(c.transport.dial_server_timeout, 10),
        heartbeat_interval: secs(hb_interval, 30),
        heartbeat_timeout: secs(c.transport.heartbeat_timeout, 90),
        mux: MuxConfig::default(),
        udp_packet_size: usize::try_from(c.udp_packet_size).unwrap_or(1500),
        ..Default::default()
    };
    Ok(ClientPlan {
        cfg,
        log_to: c.log.to.clone(),
        log_level: c.log.level.clone(),
        notes,
    })
}

/// 由 frps 配置生成运行计划。
pub fn edge_plan(l: &LoadedServer, fs: &dyn ConfigFs) -> Result<EdgePlan, String> {
    let s = &l.config;
    if s.auth.method == "oidc" {
        return Err("auth.method \"oidc\" is not implemented yet".into());
    }
    let notes = Vec::new();
    let mut allow_ports = Vec::new();
    for r in &s.allow_ports {
        let (lo, hi) = if r.single > 0 {
            (r.single, r.single)
        } else {
            (r.start, r.end)
        };
        allow_ports.push((port(lo, "allowPorts")?, port(hi, "allowPorts")?));
    }
    let identity = if s.transport.tls.cert_file.is_empty() {
        None
    } else {
        let t = &s.transport.tls;
        let cert = fs
            .read_to_string(&t.cert_file)
            .map_err(|e| format!("read certFile {}: {e}", t.cert_file))?;
        let key = fs
            .read_to_string(&t.key_file)
            .map_err(|e| format!("read keyFile {}: {e}", t.key_file))?;
        Some(tls::identity_from_pem(cert.as_bytes(), key.as_bytes()).map_err(|e| e.to_string())?)
    };
    let cfg = EdgeConfig {
        bind_addr: s.bind_addr.clone(),
        bind_port: port(s.bind_port, "bindPort")?,
        proxy_bind_addr: s.proxy_bind_addr.clone(),
        token: resolve_token(&s.auth.token, &s.auth.token_source, fs)?,
        allow_ports,
        max_ports_per_client: usize::try_from(s.max_ports_per_client).unwrap_or(0),
        heartbeat_timeout: secs(s.transport.heartbeat_timeout, 90),
        user_conn_timeout: secs(s.user_conn_timeout, 10),
        vhost_http_port: opt_port(s.vhost_http_port, "vhostHTTPPort")?,
        vhost_https_port: opt_port(s.vhost_https_port, "vhostHTTPSPort")?,
        sub_domain_host: s.sub_domain_host.clone(),
        vhost_head_timeout: secs(s.vhost_http_timeout, 60),
        udp_packet_size: usize::try_from(s.udp_packet_size).unwrap_or(1500),
        ..Default::default()
    };
    Ok(EdgePlan {
        cfg,
        identity,
        log_to: s.log.to.clone(),
        log_level: s.log.level.clone(),
        notes,
    })
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use rustfrp_core::frpcfg::{
        load_client_str, load_server_str, Format, LoadOptions, MapEnv, MemFs,
    };

    fn client(src: &str, fs: &MemFs) -> Result<ClientPlan, String> {
        let env = MapEnv::default();
        let l = load_client_str(
            src,
            Format::Toml,
            &LoadOptions {
                strict: true,
                fs,
                env: &env,
            },
        )
        .unwrap();
        client_plan(&l, fs)
    }

    fn server(src: &str, fs: &MemFs) -> Result<EdgePlan, String> {
        let env = MapEnv::default();
        let l = load_server_str(
            src,
            Format::Toml,
            &LoadOptions {
                strict: true,
                fs,
                env: &env,
            },
        )
        .unwrap();
        edge_plan(&l, fs)
    }

    #[test]
    fn client_basic_mapping() {
        let src = r#"
serverAddr = "10.0.0.1"
serverPort = 7100
user = "alice"
auth.token = "tk"
transport.dialServerTimeout = 3
transport.tcpMuxKeepaliveInterval = 15
log.to = "./frpc.log"
log.level = "debug"
[[proxies]]
name = "ssh"
type = "tcp"
localPort = 22
remotePort = 6000
[[proxies]]
name = "web"
type = "http"
localPort = 80
customDomains = ["a.com"]
hostHeaderRewrite = "internal.lan"
transport.bandwidthLimit = "1MB"
[[proxies]]
name = "dns"
type = "udp"
localPort = 53
remotePort = 5353
[[proxies]]
name = "odd"
type = "sudp"
[[proxies]]
name = "sock"
type = "tcp"
plugin.type = "socks5"
[[visitors]]
name = "v"
type = "stcp"
serverName = "s"
bindPort = 1
"#;
        let p = client(src, &MemFs::new()).unwrap();
        assert_eq!(
            (p.cfg.server_addr.as_str(), p.cfg.server_port),
            ("10.0.0.1", 7100)
        );
        assert_eq!(p.cfg.token, "tk");
        assert_eq!(p.cfg.proxies.len(), 3);
        assert_eq!(
            (p.cfg.proxies[0].name.as_str(), p.cfg.proxies[0].remote_port),
            ("alice.ssh", 6000)
        );
        let web = &p.cfg.proxies[1];
        assert_eq!(
            (
                web.kind.as_str(),
                web.custom_domains.clone(),
                web.host_header_rewrite.as_str()
            ),
            ("http", vec!["a.com".to_string()], "internal.lan")
        );
        assert_eq!(p.cfg.proxies[2].kind, "udp");
        assert_eq!(p.cfg.dial_timeout, Duration::from_secs(3));
        assert_eq!(p.cfg.heartbeat_interval, Duration::from_secs(15));
        assert_eq!(p.cfg.heartbeat_timeout, Duration::from_secs(90));
        assert_eq!(
            (p.log_to.as_str(), p.log_level.as_str()),
            ("./frpc.log", "debug")
        );
        assert_eq!(p.notes.len(), 4);
        assert!(p.notes[0].contains("transport.bandwidthLimit"));
        assert!(p.notes[1].contains("type \"sudp\""));
        assert!(p.notes[2].contains("plugins"));
        assert!(p.notes[3].contains("visitor [v]"));
    }

    #[test]
    fn client_explicit_heartbeat_and_no_user() {
        let p = client(
            "transport.tcpMux = false\n[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\n",
            &MemFs::new(),
        )
        .unwrap();
        assert_eq!(p.cfg.heartbeat_interval, Duration::from_secs(30));
        assert_eq!(p.cfg.proxies[0].name, "a");
        assert!(p.cfg.trusted_ca_pem.is_none());
    }

    #[test]
    fn client_token_sources_and_ca() {
        let fs = MemFs::new()
            .with("/t", " from-file \n")
            .with("/ca.pem", "PEM");
        let p = client("auth.tokenSource.type='file'\nauth.tokenSource.file.path='/t'\ntransport.tls.trustedCaFile='/ca.pem'\n", &fs).unwrap();
        assert_eq!(p.cfg.token, "from-file");
        assert_eq!(p.cfg.trusted_ca_pem.as_deref(), Some(&b"PEM"[..]));

        let e = client(
            "auth.tokenSource.type='file'\nauth.tokenSource.file.path='/missing'\n",
            &fs,
        )
        .err()
        .unwrap();
        assert!(e.contains("read token file /missing"));
        let e = client("transport.tls.trustedCaFile='/missing.pem'\n", &fs)
            .err()
            .unwrap();
        assert!(e.contains("read trustedCaFile"));
        let e = client(
            "auth.tokenSource.type='exec'\nauth.tokenSource.exec.command='x'\n",
            &fs,
        )
        .err()
        .unwrap();
        assert!(e.contains("exec"));
        let e = client("auth.tokenSource.type='http'\n", &fs).err().unwrap();
        assert!(e.contains("invalid auth.tokenSource"));
        let e = client("auth.method='oidc'\n", &fs).err().unwrap();
        assert!(e.contains("oidc"));
    }

    #[test]
    fn client_port_range() {
        let e = client("serverPort = 70000\n", &MemFs::new()).err().unwrap();
        assert!(e.contains("serverPort"));
        let e = client(
            "[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=70000\n",
            &MemFs::new(),
        )
        .err()
        .unwrap();
        assert!(e.contains("localPort"));
        let e = client(
            "[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\nremotePort=-5\n",
            &MemFs::new(),
        )
        .err()
        .unwrap();
        assert!(e.contains("remotePort"));
    }

    #[test]
    fn edge_basic_mapping() {
        let src = r#"
bindAddr = "127.0.0.1"
bindPort = 7100
auth.token = "tk"
maxPortsPerClient = 4
userConnTimeout = 3
transport.heartbeatTimeout = 20
vhostHTTPPort = 80
allowPorts = [ { start = 6000, end = 6100 }, { single = 7000 } ]
"#;
        let p = server(src, &MemFs::new()).unwrap();
        assert_eq!(p.cfg.bind_port, 7100);
        assert_eq!(p.cfg.token, "tk");
        assert_eq!(p.cfg.allow_ports, vec![(6000, 6100), (7000, 7000)]);
        assert_eq!(p.cfg.max_ports_per_client, 4);
        assert_eq!(p.cfg.user_conn_timeout, Duration::from_secs(3));
        assert_eq!(p.cfg.heartbeat_timeout, Duration::from_secs(20));
        assert!(p.identity.is_none());
        assert!(p.notes.is_empty());
        assert_eq!(p.cfg.proxy_bind_addr, "127.0.0.1");
        assert_eq!(p.cfg.vhost_http_port, Some(80));
        assert_eq!(p.cfg.vhost_https_port, None);
        let https = server(
            "vhostHTTPSPort = 443\nsubDomainHost = \"t.example.com\"\nvhostHTTPTimeout = 5\nudpPacketSize = 4000\n",
            &MemFs::new(),
        )
        .unwrap();
        assert_eq!(https.cfg.vhost_https_port, Some(443));
        assert_eq!(https.cfg.sub_domain_host, "t.example.com");
        assert_eq!(https.cfg.vhost_head_timeout, Duration::from_secs(5));
        assert_eq!(https.cfg.udp_packet_size, 4000);
        assert!(server("vhostHTTPPort = 70000\n", &MemFs::new())
            .err()
            .unwrap()
            .contains("vhostHTTPPort"));
        assert!(server("bindPort = 7000\n", &MemFs::new())
            .unwrap()
            .notes
            .is_empty());
    }

    #[test]
    fn edge_tls_and_errors() {
        let ck = rcgen::generate_simple_self_signed(vec!["x".to_string()]).unwrap();
        let fs = MemFs::new()
            .with("c.pem", &ck.cert.pem())
            .with("k.pem", &ck.signing_key.serialize_pem());
        let p = server(
            "transport.tls.certFile='c.pem'\ntransport.tls.keyFile='k.pem'\n",
            &fs,
        )
        .unwrap();
        assert!(p.identity.is_some());
        assert!(server(
            "transport.tls.certFile='nope'\ntransport.tls.keyFile='k.pem'\n",
            &fs
        )
        .err()
        .unwrap()
        .contains("read certFile"));
        assert!(server(
            "transport.tls.certFile='c.pem'\ntransport.tls.keyFile='nope'\n",
            &fs
        )
        .err()
        .unwrap()
        .contains("read keyFile"));
        let bad = MemFs::new().with("c.pem", "junk").with("k.pem", "junk");
        assert!(server(
            "transport.tls.certFile='c.pem'\ntransport.tls.keyFile='k.pem'\n",
            &bad
        )
        .is_err());
        assert!(server("auth.method='oidc'\n", &fs)
            .err()
            .unwrap()
            .contains("oidc"));
        assert!(server("bindPort = 70000\n", &fs)
            .err()
            .unwrap()
            .contains("bindPort"));
        assert!(server("allowPorts = [ { start = 1, end = 70000 } ]\n", &fs)
            .err()
            .unwrap()
            .contains("allowPorts"));
    }
}
