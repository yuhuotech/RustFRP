//! 配置导入预览和事务应用；公共身份不会从上传配置替换。
use crate::{auth::token_hash, config::Config, db::StoreError, store};
use rusqlite::{params, Connection, OptionalExtension};
use rustfrp_core::frpcfg::{self, ExportFormat, LoadOptions, MapEnv, MemFs, Proxy};
use serde::Serialize;

/// 可复核的导入计划。
#[derive(Serialize)]
pub struct Preview {
    pub digest: String,
    pub changes: Vec<Change>,
    pub warnings: Vec<String>,
}
/// 一个隧道的增改预览。
#[derive(Serialize)]
pub struct Change {
    pub name: String,
    pub action: String,
    pub kind: String,
    pub local_ip: String,
    pub local_port: i64,
    pub remote_port: i64,
    pub enabled: bool,
}
fn invalid(e: impl std::fmt::Display) -> StoreError {
    StoreError::Invalid(e.to_string())
}
fn load(text: &str) -> Result<frpcfg::LoadedClient, StoreError> {
    if text.len() > 1024 * 1024 || text.contains("{{") {
        return Err(invalid(
            "import maximum 1 MiB; environment templates are not allowed",
        ));
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("includes") || lower.contains("tokensource") {
        return Err(invalid(
            "includes and tokenSource are not allowed in managed import",
        ));
    }
    let (fs, env) = (MemFs::new(), MapEnv::default());
    let loaded = frpcfg::load_client_str(
        text,
        frpcfg::Format::Auto,
        &LoadOptions {
            strict: true,
            fs: &fs,
            env: &env,
        },
    )
    .map_err(invalid)?;
    if loaded.source.proxies.len() > 100 {
        return Err(invalid("import maximum 100 proxies"));
    }
    frpcfg::validate::validate_client_all(&loaded.common, &loaded.proxies, &loaded.visitors, &[])
        .map_err(invalid)?;
    Ok(loaded)
}
fn supported(p: &Proxy) -> bool {
    ["tcp", "udp", "http", "https", "tcpmux"].contains(&p.kind.as_str())
        && p.plugin.as_ref().is_none_or(plugin_supported)
}
fn plugin_supported(p: &rustfrp_core::frpcfg::Plugin) -> bool {
    rustfrp_agent::plugin::KINDS.contains(&p.kind.as_str())
}
fn plugin_opts(p: &Proxy) -> Option<rustfrp_core::wire::PluginOpts> {
    let x = p.plugin.as_ref().filter(|x| !x.kind.is_empty())?;
    if !plugin_supported(x) {
        return None;
    }
    Some(rustfrp_core::wire::PluginOpts {
        kind: x.kind.clone(),
        local_addr: x.local_addr.clone(),
        unix_path: x.unix_path.clone(),
        host_header_rewrite: x.host_header_rewrite.clone(),
        request_headers: x.request_headers.set.clone(),
        crt_path: x.crt_path.clone(),
        key_path: x.key_path.clone(),
        http_user: x.http_user.clone(),
        http_password: x.http_password.clone(),
        username: x.username.clone(),
        password: x.password.clone(),
        local_path: x.local_path.clone(),
        strip_prefix: x.strip_prefix.clone(),
        enable_http2: x.enable_http2,
    })
}
fn spec(p: &Proxy, client: &str, enabled: bool) -> store::NewTunnel {
    store::NewTunnel {
        client_id: client.into(),
        name: p.name.clone(),
        kind: p.kind.clone(),
        local_ip: p.local_ip.clone(),
        local_port: p.local_port,
        remote_port: p.remote_port,
        enabled,
        custom_domains: p.custom_domains.clone(),
        subdomain: p.subdomain.clone(),
        host_header_rewrite: p.host_header_rewrite.clone(),
        http: http_opts(p),
        proxy_protocol: proxy_protocol(p),
        bandwidth_bps: bandwidth(p),
        bandwidth_server: p.transport.bandwidth_limit_mode == "server",
        max_connections: usize::try_from(p.max_connections).unwrap_or(0),
        multiplexer: p.multiplexer.clone(),
        use_encryption: p.transport.use_encryption,
        use_compression: p.transport.use_compression,
        lb: rustfrp_agent::plan::lb_opts(p),
        plugin: plugin_opts(p),
    }
}
/// frp 的 `transport.bandwidthLimit`（字节 / 秒）；udp 上 frp 不支持。
fn bandwidth(p: &Proxy) -> u64 {
    if p.kind == "udp" {
        return 0;
    }
    u64::try_from(p.transport.bandwidth_limit.bytes()).unwrap_or(0)
}
/// 字节 / 秒 → frp 的 `1MB` / `500KB` 写法（KB 向上取整，保证不超限）。
fn bandwidth_text(bps: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bps == 0 {
        String::new()
    } else if bps % MB == 0 {
        format!("{}MB", bps / MB)
    } else {
        format!("{}KB", bps.div_ceil(1024))
    }
}
/// frp 的 `transport.proxyProtocolVersion`：v1 / v2 → 1 / 2；其他值在导入前已被校验拒绝。
fn proxy_protocol(p: &Proxy) -> u8 {
    if p.kind == "udp" {
        return 0;
    }
    rustfrp_core::proxyproto::parse_version(&p.transport.proxy_protocol_version).unwrap_or(0)
}
/// frp 的 http 选项只对 http 代理生效。
fn http_opts(p: &Proxy) -> rustfrp_core::wire::HttpOpts {
    if p.kind != "http" {
        return Default::default();
    }
    rustfrp_core::wire::HttpOpts {
        locations: p.locations.clone(),
        http_user: p.http_user.clone(),
        http_password: p.http_password.clone(),
        route_by_http_user: p.route_by_http_user.clone(),
        request_headers: p.request_headers.set.clone(),
        response_headers: p.response_headers.set.clone(),
    }
}
fn apply(
    c: &Connection,
    client: &str,
    loaded: &frpcfg::LoadedClient,
    now: i64,
) -> Result<Vec<Change>, StoreError> {
    store::get_client(c, client)?;
    let existing = store::list_tunnels(c, Some(client))?;
    let mut changes = vec![];
    for source in &loaded.source.proxies {
        if !supported(source) {
            continue;
        }
        let mut p = source.clone();
        p.complete();
        let enabled = source.is_enabled()
            && (loaded.common.start.is_empty() || loaded.common.start.contains(&p.name));
        let current = existing.iter().find(|t| t.name == p.name);
        let action = if current.is_some() {
            "update"
        } else {
            "create"
        };
        if let Some(t) = current {
            if t.kind != p.kind {
                return Err(invalid(format!("tunnel {} type cannot be changed", p.name)));
            }
            let lb = rustfrp_agent::plan::lb_opts(&p);
            store::update_tunnel(
                c,
                &t.id,
                &store::TunnelPatch {
                    name: None,
                    local_ip: Some(p.local_ip.clone()),
                    local_port: Some(p.local_port),
                    remote_port: Some(p.remote_port),
                    enabled: Some(enabled),
                    custom_domains: Some(p.custom_domains.clone()),
                    subdomain: Some(p.subdomain.clone()),
                    host_header_rewrite: Some(p.host_header_rewrite.clone()),
                    locations: Some(http_opts(&p).locations),
                    http_user: Some(p.http_user.clone()).filter(|_| p.kind == "http"),
                    http_password: Some(p.http_password.clone()).filter(|_| p.kind == "http"),
                    route_by_http_user: Some(http_opts(&p).route_by_http_user),
                    request_headers: Some(http_opts(&p).request_headers),
                    response_headers: Some(http_opts(&p).response_headers),
                    proxy_protocol: Some(proxy_protocol(&p)),
                    bandwidth_bps: Some(bandwidth(&p)),
                    bandwidth_server: Some(p.transport.bandwidth_limit_mode == "server"),
                    max_connections: usize::try_from(p.max_connections).ok(),
                    multiplexer: Some(p.multiplexer.clone()),
                    use_encryption: Some(p.transport.use_encryption),
                    use_compression: Some(p.transport.use_compression),
                    lb_group: Some(lb.group.clone()),
                    lb_group_key: Some(lb.group_key.clone()),
                    health_check: Some(lb.health.clone().unwrap_or_default()),
                    plugin: Some(plugin_opts(&p).unwrap_or_default()),
                },
                now,
            )?;
        } else {
            store::create_tunnel(c, &spec(&p, client, enabled), now)?;
        }
        changes.push(Change {
            name: p.name,
            action: action.into(),
            kind: p.kind,
            local_ip: p.local_ip,
            local_port: p.local_port,
            remote_port: p.remote_port,
            enabled,
        });
    }
    Ok(changes)
}
fn policy(
    c: &Connection,
    cfg: &Config,
    client: &str,
    loaded: &frpcfg::LoadedClient,
) -> Result<(), StoreError> {
    let device = store::get_client(c, client)?;
    let (vhost_http, vhost_https, tcpmux_port, subdomain_host) = if device.node_id.is_empty() {
        (
            cfg.vhost_http,
            cfg.vhost_https,
            cfg.tcpmux_http_connect_port,
            cfg.subdomain_host.clone(),
        )
    } else {
        let node = store::get_node(c, &device.node_id)?;
        (
            node.vhost_http,
            node.vhost_https,
            node.tcpmux_http_connect_port,
            node.subdomain_host,
        )
    };
    for p in &loaded.source.proxies {
        if !supported(p) {
            continue;
        }
        if (p.kind == "http" && vhost_http.is_none())
            || (p.kind == "https" && vhost_https.is_none())
            || (p.kind == "tcpmux" && tcpmux_port.is_none())
            || (!p.subdomain.is_empty() && subdomain_host.is_empty())
        {
            return Err(invalid("server vhost port or subdomain suffix is disabled"));
        }
        if let Err(m) = cfg.check_remote_port(p.remote_port) {
            return Err(invalid(&m));
        }
        if let Err(m) = rustfrp_core::proxyproto::parse_version(&p.transport.proxy_protocol_version)
        {
            return Err(invalid(&m));
        }
    }
    Ok(())
}
fn warnings(loaded: &frpcfg::LoadedClient) -> Vec<String> {
    let mut warnings = loaded
        .report
        .issues
        .iter()
        .map(|i| format!("{}: {}", i.path, i.message))
        .collect::<Vec<_>>();
    warnings.push("auth/serverAddr/serverPort/user/log/transport/webServer/metadatas use managed identity and node settings; preserved for export".into());
    for p in &loaded.source.proxies {
        if !supported(p) {
            warnings.push(format!(
                "proxy [{}] type or plugin is not supported; preserved but not activated",
                p.name
            ));
        }
        for (on, field) in [
            (
                p.kind == "udp" && !p.transport.bandwidth_limit.is_empty(),
                "bandwidthLimit on udp",
            ),
            // udp 隧道上 frp 本身不支持 PROXY protocol。
            (
                p.kind == "udp" && !p.transport.proxy_protocol_version.is_empty(),
                "PROXY protocol on udp",
            ),
            (
                p.kind == "udp" && !p.health_check.kind.is_empty(),
                "healthCheck on udp",
            ),
            (
                p.kind == "udp" && !p.load_balancer.group.is_empty(),
                "loadBalancer on udp",
            ),
            (p.kind != "http" && !p.locations.is_empty(), "locations"),
            (
                p.kind != "http" && (!p.http_user.is_empty() || !p.http_password.is_empty()),
                "Basic Auth",
            ),
            (
                p.kind != "http" && !p.request_headers.set.is_empty(),
                "requestHeaders",
            ),
            (
                p.kind != "http" && !p.response_headers.set.is_empty(),
                "responseHeaders",
            ),
            (
                p.kind != "http" && !p.route_by_http_user.is_empty(),
                "routeByHTTPUser",
            ),
        ] {
            if on {
                warnings.push(format!(
                    "proxy [{}] {field} preserved; runtime not implemented",
                    p.name
                ));
            }
        }
        if p.plugin
            .as_ref()
            .is_some_and(|plugin| plugin.enable_http2 == Some(true))
        {
            warnings.push(format!(
                "proxy [{}] plugin.enableHTTP2 preserved; runtime supports HTTP/1.1 only",
                p.name
            ));
        }
    }
    for v in &loaded.source.visitors {
        warnings.push(format!(
            "visitor [{}] preserved; runtime not implemented",
            v.name
        ));
    }
    warnings
}
/// 预览使用回滚事务执行完整约束校验，不创建资源。
pub fn preview(
    c: &mut Connection,
    cfg: &Config,
    client: &str,
    text: &str,
) -> Result<Preview, StoreError> {
    let loaded = load(text)?;
    policy(c, cfg, client, &loaded)?;
    let tx = c.transaction()?;
    let changes = apply(&tx, client, &loaded, store::now_ms())?;
    store::enforce_tunnel_quota(&tx, client, cfg.max_tunnels_per_client)?;
    tx.rollback()?;
    Ok(Preview {
        digest: token_hash(text),
        changes,
        warnings: warnings(&loaded),
    })
}
/// 确认后重新预检，业务与审计共事务；保存原配置以便迁出。
pub fn import(
    c: &mut Connection,
    cfg: &Config,
    client: &str,
    text: &str,
    audit: (&str, &str, &str),
) -> Result<usize, StoreError> {
    let (actor, reason, ip) = audit;
    let loaded = load(text)?;
    policy(c, cfg, client, &loaded)?;
    let now = store::now_ms();
    let tx = c.transaction()?;
    let changes = apply(&tx, client, &loaded, now)?;
    store::enforce_tunnel_quota(&tx, client, cfg.max_tunnels_per_client)?;
    tx.execute("INSERT INTO client_config_sources VALUES(?1,?2,?3) ON CONFLICT(client_id) DO UPDATE SET source_json=excluded.source_json,updated_at=excluded.updated_at",params![client,serde_json::to_string(&loaded.source).map_err(invalid)?,now])?;
    store::audit(
        &tx,
        actor,
        "config.import",
        "client",
        client,
        reason,
        "ok",
        ip,
        now,
    )?;
    tx.commit()?;
    Ok(changes.len())
}
/// 导出原字段并用当前受管隧道覆盖可运行字段，不输出设备身份秘密。
pub fn export(c: &Connection, cfg: &Config, client: &str) -> Result<String, StoreError> {
    store::get_client(c, client)?;
    let raw: Option<String> = c
        .query_row(
            "SELECT source_json FROM client_config_sources WHERE client_id=?1",
            [client],
            |r| r.get(0),
        )
        .optional()?;
    let mut source: frpcfg::ClientConfig = match raw {
        Some(raw) => serde_json::from_str(&raw).map_err(invalid)?,
        None => Default::default(),
    };
    source.auth = Default::default();
    source.server_addr = cfg.public_host.clone();
    source.server_port = i64::from(cfg.edge_port());
    source.user.clear();
    source.start.clear();
    source.includes.clear();
    let tunnels = store::list_tunnels(c, Some(client))?;
    source
        .proxies
        .retain(|p| !supported(p) || tunnels.iter().any(|t| t.name == p.name));
    for t in tunnels {
        let p = if let Some(index) = source.proxies.iter().position(|p| p.name == t.name) {
            source.proxies.get_mut(index)
        } else {
            source.proxies.push(Proxy::default());
            source.proxies.last_mut()
        };
        if let Some(p) = p {
            p.name = t.name;
            p.kind = t.kind;
            p.enabled = Some(t.enabled);
            p.local_ip = t.local_ip;
            p.local_port = i64::from(t.local_port);
            p.remote_port = i64::from(t.remote_port);
            p.custom_domains = t.custom_domains;
            p.subdomain = t.subdomain;
            p.host_header_rewrite = t.host_header_rewrite;
            let opts = store::tunnel_http_opts(c, &t.id)?;
            p.locations = opts.locations;
            p.http_user = opts.http_user;
            p.http_password = opts.http_password;
            p.route_by_http_user = opts.route_by_http_user;
            p.request_headers.set = opts.request_headers;
            p.response_headers.set = opts.response_headers;
            p.transport.bandwidth_limit =
                rustfrp_core::frpcfg::Bandwidth::parse(&bandwidth_text(t.bandwidth_bps))
                    .map_err(invalid)?;
            p.transport.bandwidth_limit_mode = if t.bandwidth_server {
                "server".into()
            } else {
                String::new()
            };
            let lb = store::tunnel_lb(c, &t.id)?;
            p.load_balancer.group = lb.group;
            p.load_balancer.group_key = lb.group_key;
            if let Some(h) = lb.health {
                p.health_check.kind = h.kind;
                p.health_check.timeout_seconds = i64::from(h.timeout_secs);
                p.health_check.max_failed = i64::from(h.max_failed);
                p.health_check.interval_seconds = i64::from(h.interval_secs);
                p.health_check.path = h.path;
                p.health_check.http_headers = h
                    .headers
                    .into_iter()
                    .map(|(name, value)| rustfrp_core::frpcfg::HttpHeader { name, value })
                    .collect();
            } else {
                p.health_check = Default::default();
            }
            p.transport.proxy_protocol_version = match t.proxy_protocol {
                1 => "v1".into(),
                2 => "v2".into(),
                _ => String::new(),
            };
            p.max_connections = i64::try_from(t.max_connections).unwrap_or(0);
            p.multiplexer = t.multiplexer;
            p.transport.use_encryption = t.use_encryption;
            p.transport.use_compression = t.use_compression;
            let plugin = store::tunnel_plugin_opts(c, &t.id)?;
            if let Some(plugin) = plugin {
                p.plugin = Some(rustfrp_core::frpcfg::Plugin {
                    kind: plugin.kind,
                    local_addr: plugin.local_addr,
                    unix_path: plugin.unix_path,
                    host_header_rewrite: plugin.host_header_rewrite,
                    request_headers: rustfrp_core::frpcfg::HeaderOperations {
                        set: plugin.request_headers,
                    },
                    crt_path: plugin.crt_path,
                    key_path: plugin.key_path,
                    http_user: plugin.http_user,
                    http_password: plugin.http_password,
                    username: plugin.username,
                    password: plugin.password,
                    local_path: plugin.local_path,
                    strip_prefix: plugin.strip_prefix,
                    enable_http2: plugin.enable_http2,
                    destination_ip: String::new(),
                });
            } else if p.plugin.as_ref().is_none_or(plugin_supported) {
                p.plugin = None;
            }
        }
    }
    frpcfg::export(&source, ExportFormat::Toml).map_err(invalid)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    fn cfg() -> Config {
        Config {
            vhost_http: Some(80),
            vhost_https: Some(443),
            subdomain_host: "example.com".into(),
            allow_ports: vec![(1, 65535)],
            ..Default::default()
        }
    }
    fn db() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut c).unwrap();
        store::create_client(
            &c,
            "c",
            "client",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "linux",
            "x",
            "v",
            0,
        )
        .unwrap();
        c
    }
    fn text(extra: &str) -> String {
        format!("serverAddr='ignored'\nserverPort=7000\n[[proxies]]\nname='ssh'\ntype='tcp'\nlocalIP='127.0.0.1'\nlocalPort=22\nremotePort=6000\n{extra}")
    }
    #[test]
    fn preview_and_import_cover_noop_web_and_unsupported() {
        let mut c = db();
        let p = preview(&mut c, &cfg(), "c", &text(""));
        assert!(p.is_ok());
        let p = p.unwrap();
        assert_eq!(p.changes[0].action, "create");
        assert!(p.warnings.iter().any(|w| w.contains("managed")));
        let count = import(&mut c, &cfg(), "c", &text(""), ("root", "reason", "")).unwrap();
        assert_eq!(count, 1);
        let out = export(&c, &cfg(), "c").unwrap();
        assert!(out.contains("[[proxies]]"));
        let web = text(
            "[[proxies]]\nname='web'\ntype='http'\nlocalPort=80\ncustomDomains=['a.example.com']\n",
        );
        assert!(preview(&mut c, &cfg(), "c", &web).is_ok());
    }
    #[test]
    fn rejects_limits_policy_types_and_bad_digest_inputs() {
        let mut c = db();
        for raw in [
            "{{ env }}",
            "{{ env }}",
            "[common]\nincludes=['x']",
            "auth.tokenSource.type='file'",
            "[common]",
            "x",
        ]
        .iter()
        {
            assert!(preview(&mut c, &cfg(), "c", raw).is_err());
        }
        let mut c = db();
        let mut p = text("");
        p.push_str(&"x".repeat(1024 * 1024));
        assert!(preview(&mut c, &cfg(), "c", &p).is_err());
        let visitor = "serverAddr='x'\n[[visitors]]\nname='v'\ntype='stcp'\n";
        assert!(preview(&mut c, &cfg(), "c", visitor).is_err());
        let unsupported = "serverAddr='x'\n[[proxies]]\nname='p'\ntype='xtcp'\nlocalPort=1\n";
        let p = preview(&mut c, &cfg(), "c", unsupported).unwrap();
        assert!(p.warnings.iter().any(|w| w.contains("not supported")));
    }
    #[test]
    fn policy_rejects_vhost_subdomain_and_ports() {
        let mut c = db();
        let mut no = cfg();
        no.vhost_http = None;
        assert!(preview(&mut c, &no, "c", &text("")).is_ok());
        let web = text(
            "[[proxies]]\nname='web'\ntype='http'\nlocalPort=80\ncustomDomains=['a.example.com']\n",
        );
        assert!(preview(&mut c, &no, "c", &web).is_err());
        let mut no = cfg();
        no.subdomain_host.clear();
        let sub = text("[[proxies]]\nname='web'\ntype='http'\nlocalPort=80\nsubdomain='x'\n");
        assert!(preview(&mut c, &no, "c", &sub).is_err());
        let mut no = cfg();
        no.allow_ports = vec![(100, 200)];
        let port = text("[[proxies]]\nname='p'\ntype='tcp'\nlocalPort=1\nremotePort=6000\n");
        assert!(preview(&mut c, &no, "c", &port).is_err());
    }
    #[test]
    fn update_type_conflict_and_export_fallback() {
        let mut c = db();
        import(
            &mut c,
            &cfg(),
            "c",
            &text("").replace("ssh", "one"),
            ("root", "r", ""),
        )
        .unwrap();
        let conflict =
            "serverAddr='x'\n[[proxies]]\nname='one'\ntype='udp'\nlocalPort=1\nremotePort=2\n";
        assert!(import(&mut c, &cfg(), "c", conflict, ("root", "r", "")).is_err());
        let mut c = db();
        let source =
            "serverAddr='x'\n[[proxies]]\nname='one'\ntype='tcp'\nlocalPort=22\nremotePort=6000\n";
        assert_eq!(
            import(&mut c, &cfg(), "c", source, ("root", "r", "")).unwrap(),
            1
        );
        assert!(export(&c, &cfg(), "c").unwrap().contains("one"));
        assert!(export(&c, &cfg(), "missing").is_err());
    }
    #[cfg(test)]
    mod coverage_more {
        use super::*;
        fn c() -> Connection {
            let mut c = Connection::open_in_memory().unwrap();
            crate::db::migrate(&mut c).unwrap();
            store::create_client(
                &c,
                "c",
                "n",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "linux",
                "x",
                "v",
                0,
            )
            .unwrap();
            c
        }
        fn run(extra: &str) -> Preview {
            let mut c = c();
            let cfg = Config {
                vhost_http: Some(80),
                vhost_https: Some(443),
                subdomain_host: "example.com".into(),
                allow_ports: vec![(1, 65535)],
                ..Default::default()
            };
            let text=format!("serverAddr='x'\n[[proxies]]\nname='p'\ntype='http'\nlocalIP='127.0.0.1'\nlocalPort=80\ncustomDomains=['a.example.com']\n{extra}");
            preview(&mut c, &cfg, "c", &text).unwrap()
        }
        #[test]
        fn warning_matrix_and_supported_kinds() {
            let p=run("transport.useEncryption=true\ntransport.useCompression=true\ntransport.bandwidthLimit='1MB'\ntransport.bandwidthLimitMode='client'\ntransport.proxyProtocolVersion='v2'\nhealthCheck={type='tcp'}\nloadBalancer={group='g'}\nlocations=['/']\nhttpUser='u'\nhttpPassword='p'\nrequestHeaders={set={X='y'}}\nresponseHeaders={set={Y='z'}}\nrouteByHTTPUser='u'");
            // http 代理已支持路径 / 认证 / 请求头 / 按用户选路：不再提示；其余仍逐项列出。
            let all = p.warnings.join("\n");
            for ok in [
                "locations",
                "Basic Auth",
                "requestHeaders",
                "routeByHTTPUser",
                "bandwidthLimit",
                "responseHeaders",
                "healthCheck",
                "loadBalancer",
            ] {
                assert!(!all.contains(ok), "{ok}: {all}");
            }
            assert!(!all.contains("encryption"), "{all}");
            assert!(!all.contains("compression"), "{all}");
            for kind in ["tcp", "udp", "https"] {
                let mut c = c();
                let cfg = Config {
                    vhost_https: Some(443),
                    ..Config::default()
                };
                let text=format!("serverAddr='x'\n[[proxies]]\nname='p'\ntype='{kind}'\nlocalPort=80\nremotePort=6000\ncustomDomains=['a.example.com']");
                let _ = preview(&mut c, &cfg, "c", &text);
            }
            let visitors = load(
                "serverAddr='x'\n[[visitors]]\nname='private-ssh'\ntype='stcp'\nserverName='ssh'\nsecretKey='secret'\nbindAddr='127.0.0.1'\nbindPort=6100\n",
            )
            .unwrap();
            assert!(warnings(&visitors)
                .join("\n")
                .contains("visitor [private-ssh] preserved"));
        }
        #[test]
        fn encryption_compression_and_connection_limits_roundtrip() {
            let mut c = c();
            let cfg = Config {
                allow_ports: vec![(1, 65535)],
                ..Default::default()
            };
            let text = "serverAddr='x'\n[[proxies]]\nname='secure'\ntype='tcp'\nlocalPort=22\nremotePort=6000\nmaxConnections=17\ntransport.useEncryption=true\ntransport.useCompression=true\n";
            import(&mut c, &cfg, "c", text, ("a", "r", "")).unwrap();
            let tunnel = store::list_tunnels(&c, Some("c")).unwrap().remove(0);
            assert_eq!(tunnel.max_connections, 17);
            assert!(tunnel.use_encryption && tunnel.use_compression);
            let exported = export(&c, &cfg, "c").unwrap();
            assert!(exported.contains("maxConnections = 17"), "{exported}");
            assert!(exported.contains("useEncryption = true"), "{exported}");
            assert!(exported.contains("useCompression = true"), "{exported}");
        }
        #[test]
        fn tcpmux_import_uses_the_selected_node_shared_listener() {
            let mut c = c();
            let cfg = Config {
                tcpmux_http_connect_port: Some(7443),
                ..Default::default()
            };
            let text = "serverAddr='x'\n[[proxies]]\nname='connect'\ntype='tcpmux'\nlocalPort=22\ncustomDomains=['ssh.example.test']\nmultiplexer='httpconnect'\n";
            let preview = preview(&mut c, &cfg, "c", text).unwrap();
            assert_eq!(preview.changes.len(), 1);
            assert!(
                !preview.warnings.join("\n").contains("tcpmux"),
                "{:?}",
                preview.warnings
            );
            import(&mut c, &cfg, "c", text, ("a", "r", "")).unwrap();
            let tunnel = store::list_tunnels(&c, Some("c")).unwrap().remove(0);
            assert_eq!(
                (tunnel.kind.as_str(), tunnel.multiplexer.as_str()),
                ("tcpmux", "httpconnect")
            );
            assert!(export(&c, &cfg, "c").unwrap().contains("type = \"tcpmux\""));
        }
        #[test]
        fn http_options_roundtrip_through_import_and_export() {
            let mut c = c();
            let cfg = Config {
                vhost_http: Some(80),
                subdomain_host: "example.com".into(),
                ..Default::default()
            };
            let text = "serverAddr='x'\n[[proxies]]\nname='web'\ntype='http'\nlocalPort=80\ncustomDomains=['a.example.com']\nlocations=['/api','/rpc']\nhttpUser='alice'\nhttpPassword='s3cret'\nrouteByHTTPUser='alice'\nrequestHeaders.set.X-Env='prod'\nresponseHeaders.set.X-Frame-Options='deny'\n";
            let p = preview(&mut c, &cfg, "c", text).unwrap();
            assert!(!p.warnings.join("\n").contains("Basic Auth"));
            assert_eq!(import(&mut c, &cfg, "c", text, ("a", "r", "")).unwrap(), 1);
            let t = store::list_tunnels(&c, Some("c")).unwrap().remove(0);
            assert_eq!(
                (
                    t.locations.clone(),
                    t.http_user.as_str(),
                    t.http_password_set
                ),
                (vec!["/api".to_string(), "/rpc".to_string()], "alice", true)
            );
            let out = export(&c, &cfg, "c").unwrap();
            for want in [
                "httpUser = \"alice\"",
                "httpPassword = \"s3cret\"",
                "routeByHTTPUser = \"alice\"",
                "/rpc",
                "X-Env",
                "X-Frame-Options",
            ] {
                assert!(out.contains(want), "{want}\n{out}");
            }
            // 再次导入（更新路径）并清掉选项。
            let plain = "serverAddr='x'\n[[proxies]]\nname='web'\ntype='http'\nlocalPort=80\ncustomDomains=['a.example.com']\n";
            assert_eq!(import(&mut c, &cfg, "c", plain, ("a", "r", "")).unwrap(), 1);
            let t = store::list_tunnels(&c, Some("c")).unwrap().remove(0);
            assert!(
                t.locations.is_empty()
                    && !t.http_password_set
                    && t.request_headers.is_empty()
                    && t.response_headers.is_empty()
            );
            // 其他代理类型带这些字段会在严格解析阶段被拒绝（frp 的字段只属于 http 代理）。
            let tcp = "serverAddr='x'\n[[proxies]]\nname='t'\ntype='tcp'\nlocalPort=22\nremotePort=6000\nhttpUser='u'\n";
            assert!(preview(&mut c, &cfg, "c", tcp).is_err());
        }
        #[test]
        fn load_balancer_and_health_check_roundtrip_and_udp_is_flagged() {
            let mut c = c();
            let cfg = Config {
                allow_ports: vec![(1, 65535)],
                ..Default::default()
            };
            let text = "serverAddr='x'\n[[proxies]]\nname='t'\ntype='tcp'\nlocalPort=22\nremotePort=6000\nloadBalancer.group='g'\nloadBalancer.groupKey='k'\nhealthCheck.type='http'\nhealthCheck.path='/healthz'\nhealthCheck.timeoutSeconds=2\nhealthCheck.maxFailed=3\nhealthCheck.intervalSeconds=5\nhealthCheck.httpHeaders=[{name='X-A',value='1'}]\n";
            let p = preview(&mut c, &cfg, "c", text).unwrap();
            assert!(
                !p.warnings.join("\n").contains("loadBalancer"),
                "{:?}",
                p.warnings
            );
            import(&mut c, &cfg, "c", text, ("a", "r", "")).unwrap();
            let t = store::list_tunnels(&c, Some("c")).unwrap().remove(0);
            assert_eq!(t.lb_group, "g");
            let lb = store::tunnel_lb(&c, &t.id).unwrap();
            let h = lb.health.unwrap();
            assert_eq!(
                (
                    lb.group_key.as_str(),
                    h.kind.as_str(),
                    h.path.as_str(),
                    h.timeout_secs,
                    h.max_failed,
                    h.interval_secs,
                    h.headers.get("X-A").map(String::as_str)
                ),
                ("k", "http", "/healthz", 2, 3, 5, Some("1"))
            );
            let out = export(&c, &cfg, "c").unwrap();
            for want in [
                "group = \"g\"",
                "groupKey = \"k\"",
                "/healthz",
                "maxFailed = 3",
                "X-A",
            ] {
                assert!(out.contains(want), "{want}\n{out}");
            }
            // 再次导入去掉这些选项：回到默认。
            let plain = "serverAddr='x'\n[[proxies]]\nname='t'\ntype='tcp'\nlocalPort=22\nremotePort=6000\n";
            import(&mut c, &cfg, "c", plain, ("a", "r", "")).unwrap();
            let t = store::list_tunnels(&c, Some("c")).unwrap().remove(0);
            assert!(t.lb_group.is_empty() && t.health_check.is_none());
            assert!(!export(&c, &cfg, "c").unwrap().contains("healthCheck"));
            let udp = "serverAddr='x'\n[[proxies]]\nname='u'\ntype='udp'\nlocalPort=53\nremotePort=5353\nloadBalancer.group='g'\nhealthCheck.type='tcp'\n";
            let w = preview(&mut c, &cfg, "c", udp).unwrap().warnings.join("\n");
            assert!(
                w.contains("loadBalancer on udp") && w.contains("healthCheck on udp"),
                "{w}"
            );
        }
        #[test]
        fn bandwidth_limit_roundtrips_in_both_modes_and_udp_is_flagged() {
            let mut c = c();
            let cfg = Config {
                allow_ports: vec![(1, 65535)],
                ..Default::default()
            };
            let head = "serverAddr='x'\n[[proxies]]\nname='t'\ntype='tcp'\nlocalPort=22\nremotePort=6000\n";
            let mb = format!(
                "{head}transport.bandwidthLimit='2MB'\ntransport.bandwidthLimitMode='server'\n"
            );
            import(&mut c, &cfg, "c", &mb, ("a", "r", "")).unwrap();
            let t = &store::list_tunnels(&c, Some("c")).unwrap()[0];
            assert_eq!((t.bandwidth_bps, t.bandwidth_server), (2 << 20, true));
            let out = export(&c, &cfg, "c").unwrap();
            assert!(out.contains("bandwidthLimit = \"2MB\""), "{out}");
            assert!(out.contains("bandwidthLimitMode = \"server\""), "{out}");
            let kb = format!("{head}transport.bandwidthLimit='500KB'\n");
            import(&mut c, &cfg, "c", &kb, ("a", "r", "")).unwrap();
            let t = &store::list_tunnels(&c, Some("c")).unwrap()[0];
            assert_eq!((t.bandwidth_bps, t.bandwidth_server), (500 * 1024, false));
            assert!(export(&c, &cfg, "c")
                .unwrap()
                .contains("bandwidthLimit = \"500KB\""));
            import(&mut c, &cfg, "c", head, ("a", "r", "")).unwrap();
            assert_eq!(
                store::list_tunnels(&c, Some("c")).unwrap()[0].bandwidth_bps,
                0
            );
            assert!(!export(&c, &cfg, "c").unwrap().contains("bandwidthLimit"));
            let udp = "serverAddr='x'\n[[proxies]]\nname='u'\ntype='udp'\nlocalPort=53\nremotePort=5353\ntransport.bandwidthLimit='1MB'\n";
            let p = preview(&mut c, &cfg, "c", udp).unwrap();
            assert!(p.warnings.join("\n").contains("bandwidthLimit on udp"));
        }
        #[test]
        fn proxy_protocol_roundtrips_and_bad_versions_are_refused() {
            let mut c = c();
            let cfg = Config {
                allow_ports: vec![(1, 65535)],
                ..Default::default()
            };
            let text = "serverAddr='x'\n[[proxies]]\nname='t'\ntype='tcp'\nlocalPort=22\nremotePort=6000\ntransport.proxyProtocolVersion='v2'\n";
            let p = preview(&mut c, &cfg, "c", text).unwrap();
            assert!(
                !p.warnings.join("\n").contains("PROXY protocol"),
                "{:?}",
                p.warnings
            );
            assert_eq!(import(&mut c, &cfg, "c", text, ("a", "r", "")).unwrap(), 1);
            assert_eq!(
                store::list_tunnels(&c, Some("c")).unwrap()[0].proxy_protocol,
                2
            );
            assert!(export(&c, &cfg, "c")
                .unwrap()
                .contains("proxyProtocolVersion = \"v2\""));
            let bad = text.replace("'v2'", "'v7'");
            assert!(preview(&mut c, &cfg, "c", &bad).is_err());
            // 再次导入且去掉该选项：回到 0。
            let plain = "serverAddr='x'\n[[proxies]]\nname='t'\ntype='tcp'\nlocalPort=22\nremotePort=6000\n";
            import(&mut c, &cfg, "c", plain, ("a", "r", "")).unwrap();
            assert_eq!(
                store::list_tunnels(&c, Some("c")).unwrap()[0].proxy_protocol,
                0
            );
            let v1 = text
                .replace("name='t'", "name='t-v1'")
                .replace("remotePort=6000", "remotePort=6001")
                .replace("'v2'", "'v1'");
            import(&mut c, &cfg, "c", &v1, ("a", "r", "")).unwrap();
            let v1_tunnel = store::list_tunnels(&c, Some("c"))
                .unwrap()
                .into_iter()
                .find(|t| t.name == "t-v1")
                .unwrap();
            assert_eq!(v1_tunnel.proxy_protocol, 1);
            assert!(export(&c, &cfg, "c")
                .unwrap()
                .contains("proxyProtocolVersion = \"v1\""));
            // udp：frp 不支持，导入时提示。
            let udp = "serverAddr='x'\n[[proxies]]\nname='u'\ntype='udp'\nlocalPort=53\nremotePort=5353\ntransport.proxyProtocolVersion='v1'\n";
            assert!(preview(&mut c, &cfg, "c", udp)
                .unwrap()
                .warnings
                .join("\n")
                .contains("PROXY protocol on udp"));
        }
        #[test]
        fn quota_and_reserved_ports_apply_to_imports() {
            let mut c = c();
            let mut cfg = Config {
                max_tunnels_per_client: 1,
                reserved_ports: vec![(6001, 6001)],
                ..Default::default()
            };
            let two = "serverAddr='x'\n[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\nremotePort=6000\n[[proxies]]\nname='b'\ntype='tcp'\nlocalPort=2\nremotePort=6002\n";
            assert!(matches!(
                preview(&mut c, &cfg, "c", two),
                Err(StoreError::Conflict(m)) if m.contains("limit")
            ));
            assert!(import(&mut c, &cfg, "c", two, ("a", "r", "")).is_err());
            cfg.max_tunnels_per_client = 2;
            let reserved =
                "serverAddr='x'\n[[proxies]]\nname='a'\ntype='tcp'\nlocalPort=1\nremotePort=6001\n";
            assert!(matches!(
                preview(&mut c, &cfg, "c", reserved),
                Err(StoreError::Invalid(m)) if m.contains("reserved")
            ));
            assert_eq!(import(&mut c, &cfg, "c", two, ("a", "r", "")).unwrap(), 2);
        }
        #[test]
        fn import_update_and_source_export_paths() {
            let mut c = c();
            let cfg = Config {
                vhost_http: Some(80),
                ..Default::default()
            };
            let text="serverAddr='x'\n[[proxies]]\nname='p'\ntype='tcp'\nlocalPort=80\nremotePort=6000\n";
            assert_eq!(import(&mut c, &cfg, "c", text, ("a", "r", "")).unwrap(), 1);
            let update="serverAddr='x'\n[[proxies]]\nname='p'\ntype='tcp'\nlocalPort=81\nremotePort=6001\n";
            assert_eq!(
                import(&mut c, &cfg, "c", update, ("a", "r", "")).unwrap(),
                1
            );
            assert!(export(&c, &cfg, "c").unwrap().contains("localPort = 81"));
        }

        #[test]
        fn managed_plugins_activate_roundtrip_and_keep_unsupported_source() {
            let cfg = Config {
                allow_ports: vec![(1, 65535)],
                vhost_http: Some(80),
                vhost_https: Some(443),
                subdomain_host: "example.com".into(),
                ..Default::default()
            };
            let supported = "serverAddr='x'\n[[proxies]]\nname='socks'\ntype='tcp'\nlocalPort=1080\nremotePort=6000\n[proxies.plugin]\ntype='socks5'\nusername='alice'\npassword='secret'\n[[proxies]]\nname='secure'\ntype='https'\nlocalPort=443\ncustomDomains=['secure.example.com']\n[proxies.plugin]\ntype='https2http'\nlocalAddr='127.0.0.1:8080'\ncrtPath='/client/cert.pem'\nkeyPath='/client/key.pem'\nenableHTTP2=true\n";
            let loaded = load(supported).unwrap();
            assert_eq!(loaded.proxies.len(), 2);
            assert!(plugin_opts(&loaded.proxies[0]).is_some());
            let mut first = c();
            let p = preview(&mut first, &cfg, "c", supported).unwrap();
            let warning_text = p.warnings.join("\n");
            assert!(!warning_text.contains("not activated"));
            assert!(warning_text.contains("enableHTTP2"));
            assert_eq!(
                import(&mut first, &cfg, "c", supported, ("root", "r", "")).unwrap(),
                2
            );
            let tunnels = store::list_tunnels(&first, Some("c")).unwrap();
            let tunnel = tunnels.iter().find(|t| t.name == "socks").unwrap();
            let options = store::tunnel_plugin_opts(&first, &tunnel.id)
                .unwrap()
                .unwrap();
            assert_eq!(
                (options.kind.as_str(), options.username.as_str()),
                ("socks5", "alice")
            );
            assert_eq!(options.password, "secret");
            let secure = tunnels.iter().find(|t| t.name == "secure").unwrap();
            let secure_plugin = store::tunnel_plugin_opts(&first, &secure.id)
                .unwrap()
                .unwrap();
            assert_eq!(secure_plugin.kind, "https2http");
            assert_eq!(secure_plugin.enable_http2, Some(true));
            let out = export(&first, &cfg, "c").unwrap();
            assert!(out.contains("type = \"socks5\""), "{out}");
            assert!(out.contains("password = \"secret\""), "{out}");
            assert!(out.contains("type = \"https2http\""), "{out}");
            assert!(out.contains("enableHTTP2 = true"), "{out}");

            let mut c2 = c();
            let unsupported = "serverAddr='x'\n[[proxies]]\nname='legacy-plugin'\ntype='tcp'\nlocalPort=80\nremotePort=6001\n[proxies.plugin]\ntype='http2http'\nlocalAddr='127.0.0.1:8080'\n";
            let loaded = load(unsupported).unwrap();
            assert!(plugin_opts(&loaded.proxies[0]).is_none());
            let p = preview(&mut c2, &cfg, "c", unsupported).unwrap();
            assert!(p.warnings.join("\n").contains("not activated"));
            assert_eq!(
                import(&mut c2, &cfg, "c", unsupported, ("root", "r", "")).unwrap(),
                0
            );
            assert!(store::list_tunnels(&c2, Some("c")).unwrap().is_empty());
            assert!(export(&c2, &cfg, "c").unwrap().contains("http2http"));

            let plain = load(&text("")).unwrap();
            assert!(plugin_opts(&plain.proxies[0]).is_none());
            let many = format!(
                "serverAddr='x'\n{}",
                (0..101)
                    .map(|i| format!(
                        "[[proxies]]\nname='p{i}'\ntype='tcp'\nlocalPort=1\nremotePort={}\n",
                        1000 + i
                    ))
                    .collect::<Vec<_>>()
                    .join("")
            );
            assert!(load(&many).is_err());
        }

        #[test]
        fn imports_validate_external_node_vhost_capabilities() {
            fn preview_with_node(
                info: rustfrp_core::node::NodeInfo,
                proxy: &str,
            ) -> Result<Preview, StoreError> {
                let mut c = c();
                store::create_node(
                    &c,
                    "edge",
                    "edge",
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    &info,
                    1,
                )
                .unwrap();
                store::set_client_node(&c, "c", "edge").unwrap();
                preview(
                    &mut c,
                    &Config::default(),
                    "c",
                    &format!("serverAddr='x'\n{proxy}"),
                )
            }

            let full = rustfrp_core::node::NodeInfo {
                vhost_http: Some(80),
                vhost_https: Some(443),
                tcpmux_http_connect_port: Some(7443),
                subdomain_host: "node.example.test".into(),
                ..Default::default()
            };
            let all_kinds = "[[proxies]]\nname='web'\ntype='http'\nlocalPort=80\ncustomDomains=['web.example.test']\n[[proxies]]\nname='secure'\ntype='https'\nlocalPort=443\ncustomDomains=['secure.example.test']\n[[proxies]]\nname='mux'\ntype='tcpmux'\nlocalPort=22\nmultiplexer='httpconnect'\ncustomDomains=['mux.example.test']\n[[proxies]]\nname='sub'\ntype='http'\nlocalPort=80\nsubdomain='api'\n";
            assert!(preview_with_node(full.clone(), all_kinds).is_ok());

            for (info, proxy) in [
                (
                    rustfrp_core::node::NodeInfo {
                        vhost_http: None,
                        ..full.clone()
                    },
                    "[[proxies]]\nname='web'\ntype='http'\nlocalPort=80\ncustomDomains=['web.example.test']\n",
                ),
                (
                    rustfrp_core::node::NodeInfo {
                        vhost_https: None,
                        ..full.clone()
                    },
                    "[[proxies]]\nname='secure'\ntype='https'\nlocalPort=443\ncustomDomains=['secure.example.test']\n",
                ),
                (
                    rustfrp_core::node::NodeInfo {
                        tcpmux_http_connect_port: None,
                        ..full.clone()
                    },
                    "[[proxies]]\nname='mux'\ntype='tcpmux'\nlocalPort=22\nmultiplexer='httpconnect'\ncustomDomains=['mux.example.test']\n",
                ),
                (
                    rustfrp_core::node::NodeInfo {
                        subdomain_host: String::new(),
                        ..full.clone()
                    },
                    "[[proxies]]\nname='sub'\ntype='http'\nlocalPort=80\nsubdomain='api'\n",
                ),
            ] {
                assert!(preview_with_node(info, proxy).is_err());
            }
        }
    }
}
