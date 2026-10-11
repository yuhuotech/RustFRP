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

/// frp 的 `plugin` → 插件配置（证书文件在这里读入）。
fn plugin_cfg(
    x: &rustfrp_core::frpcfg::Plugin,
    fs: &dyn ConfigFs,
) -> Result<crate::plugin::PluginCfg, String> {
    let read = |path: &str, what: &str| -> Result<String, String> {
        if path.is_empty() {
            return Ok(String::new());
        }
        fs.read_to_string(path)
            .map_err(|e| format!("read plugin {what} {path}: {e}"))
    };
    Ok(crate::plugin::PluginCfg {
        kind: x.kind.clone(),
        local_addr: x.local_addr.clone(),
        unix_path: x.unix_path.clone(),
        host_header_rewrite: x.host_header_rewrite.clone(),
        request_headers: x
            .request_headers
            .set
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        crt_pem: read(&x.crt_path, "crtPath")?,
        key_pem: read(&x.key_path, "keyPath")?,
        http_user: x.http_user.clone(),
        http_password: x.http_password.clone(),
        username: x.username.clone(),
        password: x.password.clone(),
        local_path: x.local_path.clone(),
        strip_prefix: x.strip_prefix.clone(),
    })
}

/// frp 的 `loadBalancer` / `healthCheck` → 线协议（默认值与 frp 一致：超时 3s、失败 1 次、间隔 10s）。
pub fn lb_opts(p: &rustfrp_core::frpcfg::Proxy) -> rustfrp_core::wire::LbOpts {
    if p.kind == "udp" {
        return Default::default();
    }
    let h = &p.health_check;
    let secs = |v: i64, default: u32| u32::try_from(v).ok().filter(|n| *n > 0).unwrap_or(default);
    rustfrp_core::wire::LbOpts {
        group: p.load_balancer.group.clone(),
        group_key: p.load_balancer.group_key.clone(),
        health: (!h.kind.is_empty()).then(|| rustfrp_core::wire::HealthSpec {
            kind: h.kind.clone(),
            timeout_secs: secs(h.timeout_seconds, 3),
            max_failed: secs(h.max_failed, 1),
            interval_secs: secs(h.interval_seconds, 10),
            path: h.path.clone(),
            headers: h
                .http_headers
                .iter()
                .map(|x| (x.name.clone(), x.value.clone()))
                .collect(),
        }),
    }
}

/// 该代理里配置了、但当前版本尚未实现的选项（不会静默忽略：调用方会把它们打印出来）。
pub(crate) fn ignored_options(p: &rustfrp_core::frpcfg::Proxy) -> Vec<&'static str> {
    let mut v = Vec::new();
    let mut flag = |on: bool, name: &'static str| {
        if on {
            v.push(name);
        }
    };
    flag(p.transport.use_encryption, "transport.useEncryption");
    flag(p.transport.use_compression, "transport.useCompression");
    // frp 本身不支持 udp 限速；tcp / http / https 由 Limiter 实现。
    flag(
        p.kind == "udp" && !p.transport.bandwidth_limit.is_empty(),
        "transport.bandwidthLimit (udp)",
    );
    // udp 上 frp 本身不支持 PROXY protocol。
    flag(
        p.kind == "udp" && !p.transport.proxy_protocol_version.is_empty(),
        "transport.proxyProtocolVersion (udp)",
    );
    // 健康检查与负载均衡组已支持 tcp / http / https；udp 上没有意义。
    flag(
        p.kind == "udp" && !p.health_check.kind.is_empty(),
        "healthCheck (udp)",
    );
    flag(
        p.kind == "udp" && !p.load_balancer.group.is_empty(),
        "loadBalancer (udp)",
    );
    // http 代理已支持这些选项；其他类型上配置它们没有意义，按「未支持」提示。
    let other = p.kind != "http";
    flag(other && !p.locations.is_empty(), "locations");
    flag(
        other && (!p.http_user.is_empty() || !p.http_password.is_empty()),
        "httpUser / httpPassword",
    );
    flag(other && !p.request_headers.set.is_empty(), "requestHeaders");
    flag(
        other && !p.response_headers.set.is_empty(),
        "responseHeaders",
    );
    flag(other && !p.route_by_http_user.is_empty(), "routeByHTTPUser");
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
        if !["tcp", "udp", "http", "https", "stcp", "sudp"].contains(&p.kind.as_str()) {
            notes.push(format!(
                "proxy [{}]: type \"{}\" is not supported yet, skipped",
                p.name, p.kind
            ));
            continue;
        }
        let plugin = match p.plugin.as_ref().filter(|x| !x.kind.is_empty()) {
            Some(x) if !crate::plugin::KINDS.contains(&x.kind.as_str()) => {
                notes.push(format!(
                    "proxy [{}]: plugin \"{}\" is not supported yet, skipped",
                    p.name, x.kind
                ));
                continue;
            }
            Some(x) => Some(plugin_cfg(x, fs).map_err(|e| format!("proxy [{}]: {e}", p.name))?),
            None => None,
        };
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
            bandwidth_bps: if p.kind == "udp" {
                0
            } else {
                u64::try_from(p.transport.bandwidth_limit.bytes()).unwrap_or(0)
            },
            bandwidth_server: p.transport.bandwidth_limit_mode == "server",
            proxy_protocol: if p.kind == "udp" {
                0
            } else {
                rustfrp_core::proxyproto::parse_version(&p.transport.proxy_protocol_version)
                    .map_err(|e| format!("proxy [{}]: {e}", p.name))?
            },
            lb: lb_opts(p),
            secret: rustfrp_core::wire::SecretOpts {
                sk: p.secret_key.clone(),
                allow_users: p.allow_users.clone(),
            },
            plugin,
            http: if p.kind == "http" {
                rustfrp_core::wire::HttpOpts {
                    locations: p.locations.clone(),
                    http_user: p.http_user.clone(),
                    http_password: p.http_password.clone(),
                    route_by_http_user: p.route_by_http_user.clone(),
                    request_headers: p.request_headers.set.clone(),
                    response_headers: p.response_headers.set.clone(),
                }
            } else {
                Default::default()
            },
        });
        for opt in ignored_options(p) {
            notes.push(format!(
                "proxy [{}]: option {opt} is not supported yet and is ignored",
                p.name
            ));
        }
    }
    // 管理接口只提供 JSON / 文本（reload / status / stop），没有网页和 TLS。
    if c.web_server.port > 0 {
        if c.web_server.tls.is_some() {
            notes.push(
                "webServer.tls is not supported yet: the admin interface is plain HTTP".into(),
            );
        }
        if !c.web_server.assets_dir.is_empty() {
            notes
                .push("webServer.assetsDir is not supported yet: there is no web dashboard".into());
        }
    }
    let mut visitors = Vec::new();
    for v in &l.visitors {
        if !["stcp", "sudp"].contains(&v.kind.as_str()) {
            notes.push(format!(
                "visitor [{}]: type \"{}\" is not supported yet, skipped",
                v.name, v.kind
            ));
            continue;
        }
        if v.enabled == Some(false) {
            continue;
        }
        visitors.push(rustfrp_core::wire::VisitorSpec {
            name: v.name.clone(),
            kind: v.kind.clone(),
            server_user: v.server_user.clone(),
            server_name: v.server_name.clone(),
            sk: v.secret_key.clone(),
            bind_addr: v.bind_addr.clone(),
            bind_port: port(v.bind_port, &format!("visitor [{}] bindPort", v.name))?,
        });
        if v.transport.use_encryption || v.transport.use_compression {
            notes.push(format!(
                "visitor [{}]: option transport.useEncryption / useCompression is not supported yet and is ignored",
                v.name
            ));
        }
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
        visitors,
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
    let mut notes = Vec::new();
    if s.web_server.port > 0 {
        notes.push("webServer (dashboard) is not supported yet on the edge and is ignored".into());
    }
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
    fn proxy_protocol_versions_map_and_bad_values_are_rejected() {
        let fs = MemFs::new();
        let base = |extra: &str| {
            format!("serverAddr='x'\n[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\n{extra}\n[[proxies]]\nname='u'\ntype='udp'\nlocalPort=2\nremotePort=3\n")
        };
        let versions = |src: &str| {
            client(src, &fs)
                .unwrap()
                .cfg
                .proxies
                .iter()
                .map(|p| p.proxy_protocol)
                .collect::<Vec<_>>()
        };
        assert_eq!(versions(&base("")), vec![0, 0]);
        assert_eq!(
            versions(&base("transport.proxyProtocolVersion='v1'")),
            vec![1, 0]
        );
        assert_eq!(
            versions(&base("transport.proxyProtocolVersion='v2'")),
            vec![2, 0]
        );
        let err = client(&base("transport.proxyProtocolVersion='v9'"), &fs)
            .err()
            .unwrap();
        assert!(err.contains("proxy [a]") && err.contains("v9"), "{err}");
        // udp 上配置了：frp 本身不支持，给出提示而不是静默忽略。
        let src = "serverAddr='x'\n[[proxies]]\nname='u'\ntype='udp'\nlocalPort=2\nremotePort=3\ntransport.proxyProtocolVersion='v1'\n";
        let plan = client(src, &fs).unwrap();
        assert_eq!(plan.cfg.proxies[0].proxy_protocol, 0);
        assert!(
            plan.notes
                .iter()
                .any(|n| n.contains("proxyProtocolVersion (udp)")),
            "{:?}",
            plan.notes
        );
    }

    #[test]
    fn health_checks_and_groups_map_with_frp_defaults_and_flag_udp() {
        let fs = MemFs::default();
        let src = "serverAddr='x'\n[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\nremotePort=2\nloadBalancer.group='g'\nloadBalancer.groupKey='k'\nhealthCheck.type='tcp'\n\
[[proxies]]\nname='h'\ntype='http'\nlocalPort=3\ncustomDomains=['a.com']\nhealthCheck.type='http'\nhealthCheck.path='/hz'\nhealthCheck.timeoutSeconds=7\nhealthCheck.maxFailed=4\nhealthCheck.intervalSeconds=20\nhealthCheck.httpHeaders=[{name='X-A',value='1'}]\n\
[[proxies]]\nname='u'\ntype='udp'\nlocalPort=5\nremotePort=6\nloadBalancer.group='g'\nhealthCheck.type='tcp'\n";
        let plan = client(src, &fs).unwrap();
        let lb = |i: usize| plan.cfg.proxies[i].lb.clone();
        assert_eq!((lb(0).group.as_str(), lb(0).group_key.as_str()), ("g", "k"));
        let tcp = lb(0).health.unwrap();
        assert_eq!(
            (
                tcp.kind.as_str(),
                tcp.timeout_secs,
                tcp.max_failed,
                tcp.interval_secs
            ),
            ("tcp", 3, 1, 10),
            "frp defaults"
        );
        let http = lb(1).health.unwrap();
        assert_eq!(
            (
                http.path.as_str(),
                http.timeout_secs,
                http.max_failed,
                http.interval_secs
            ),
            ("/hz", 7, 4, 20)
        );
        assert_eq!(http.headers.get("X-A").map(String::as_str), Some("1"));
        assert!(lb(2).is_default(), "udp never joins a group");
        let notes = plan.notes.join("\n");
        assert!(
            notes.contains("healthCheck (udp)") && notes.contains("loadBalancer (udp)"),
            "{notes}"
        );
        assert!(!notes.contains("proxy [a]"), "{notes}");
    }

    #[test]
    fn plugins_map_with_certificates_read_and_unsupported_ones_are_reported() {
        let fs = MemFs::new().with("c.crt", "CERT").with("c.key", "KEY");
        let src = "serverAddr='x'\n\
[[proxies]]\nname='t'\ntype='https'\ncustomDomains=['a.com']\nplugin.type='https2http'\nplugin.localAddr='127.0.0.1:80'\nplugin.crtPath='c.crt'\nplugin.keyPath='c.key'\nplugin.hostHeaderRewrite='in.lan'\nplugin.requestHeaders.set.X-A='1'\n\
[[proxies]]\nname='s'\ntype='tcp'\nremotePort=1\nplugin.type='static_file'\nplugin.localPath='/srv'\nplugin.stripPrefix='st'\nplugin.httpUser='u'\nplugin.httpPassword='p'\n\
[[proxies]]\nname='v'\ntype='tcp'\nremotePort=2\nplugin.type='virtual_net'\n";
        let plan = client(src, &fs).unwrap();
        assert_eq!(plan.cfg.proxies.len(), 2);
        let t = plan.cfg.proxies[0].plugin.clone().unwrap();
        assert_eq!(
            (
                t.crt_pem.as_str(),
                t.key_pem.as_str(),
                t.host_header_rewrite.as_str()
            ),
            ("CERT", "KEY", "in.lan")
        );
        assert_eq!(
            t.request_headers,
            vec![("X-A".to_string(), "1".to_string())]
        );
        let s = plan.cfg.proxies[1].plugin.clone().unwrap();
        assert_eq!(
            (
                s.local_path.as_str(),
                s.strip_prefix.as_str(),
                s.http_user.as_str(),
                s.http_password.as_str()
            ),
            ("/srv", "st", "u", "p")
        );
        assert!(
            plan.notes.iter().any(|n| n.contains("virtual_net")),
            "{:?}",
            plan.notes
        );
        // 证书文件读不到：启动前就报错，而不是静默降级成自签名。
        let err = client(src, &MemFs::new()).err().unwrap();
        assert!(
            err.contains("proxy [t]") && err.contains("crtPath"),
            "{err}"
        );
    }

    #[test]
    fn web_server_options_that_have_no_runtime_are_reported() {
        let fs = MemFs::new();
        let client_src = "serverAddr='x'\nwebServer.port=7400\nwebServer.assetsDir='/srv/ui'\nwebServer.tls.certFile='c'\nwebServer.tls.keyFile='k'\n";
        let plan = client(client_src, &fs);
        let notes = plan.map(|p| p.notes.join("\n")).unwrap_or_default();
        assert!(
            notes.contains("webServer.tls") && notes.contains("webServer.assetsDir"),
            "{notes}"
        );
        let quiet = client("serverAddr='x'\nwebServer.port=7400\n", &fs).unwrap();
        assert!(quiet.notes.is_empty(), "{:?}", quiet.notes);
        let edge = server("bindPort=7000\nwebServer.port=7500\n", &fs).unwrap();
        assert!(
            edge.notes
                .iter()
                .any(|n| n.contains("webServer (dashboard)")),
            "{:?}",
            edge.notes
        );
    }

    #[test]
    fn visitors_are_filtered_and_unsupported_types_and_options_are_reported() {
        let fs = MemFs::new();
        let src = "serverAddr='x'\n\
[[proxies]]\nname='p2p'\ntype='xtcp'\nsecretKey='k'\nlocalPort=1\n\
[[visitors]]\nname='x'\ntype='xtcp'\nserverName='s'\nsecretKey='k'\nbindPort=1\nprotocol='quic'\n\
[[visitors]]\nname='off'\ntype='stcp'\nenabled=false\nserverName='s'\nsecretKey='k'\nbindPort=2\n\
[[visitors]]\nname='enc'\ntype='sudp'\nserverName='s'\nsecretKey='k'\nbindPort=3\nserverUser='bob'\ntransport.useEncryption=true\n";
        let plan = client(src, &fs).unwrap();
        assert!(plan.cfg.proxies.is_empty());
        assert_eq!(plan.cfg.visitors.len(), 1);
        let v = &plan.cfg.visitors[0];
        assert_eq!(
            (
                v.name.as_str(),
                v.kind.as_str(),
                v.server_user.as_str(),
                v.bind_port
            ),
            ("enc", "sudp", "bob", 3)
        );
        let notes = plan.notes.join("\n");
        assert!(
            notes.contains("proxy [p2p]")
                && notes.contains("visitor [x]")
                && notes.contains("visitor [enc]"),
            "{notes}"
        );
        assert!(!notes.contains("visitor [off]"), "{notes}");
        // 端口越界是配置错误。
        assert!(client("serverAddr='x'\n[[visitors]]\nname='v'\ntype='stcp'\nserverName='s'\nsecretKey='k'\nbindPort=70000\n", &fs).is_err());
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
transport.useEncryption = true
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
        assert_eq!(
            p.cfg.proxies[4].plugin.as_ref().map(|x| x.kind.as_str()),
            Some("socks5")
        );
        // sudp 代理现在已支持（没有 secretKey 时由 Edge 在运行期拒绝）；stcp 访问者被接受。
        assert_eq!(p.cfg.proxies.len(), 5);
        assert_eq!(p.cfg.visitors.len(), 1);
        assert_eq!(
            (p.cfg.visitors[0].name.as_str(), p.cfg.visitors[0].bind_port),
            ("v", 1)
        );
        assert_eq!(p.notes.len(), 1, "{:?}", p.notes);
        assert!(p.notes[0].contains("transport.useEncryption"));
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
