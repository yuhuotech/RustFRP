//! 配置校验，规则与错误文案对齐 frp `pkg/config/v1/validation`。

use super::error::ConfigError;
use super::types::*;

const AUTH_METHODS: &[&str] = &["token", "oidc"];
const AUTH_SCOPES: &[&str] = &["HeartBeats", "NewWorkConns"];
const TRANSPORT_PROTOCOLS: &[&str] = &["tcp", "kcp", "quic", "websocket", "wss"];
const WIRE_PROTOCOLS: &[&str] = &["v1", "v2"];
const LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];
const HTTP_PLUGIN_OPS: &[&str] = &[
    "Login",
    "NewProxy",
    "CloseProxy",
    "Ping",
    "NewWorkConn",
    "NewUserConn",
];
const KNOWN_FEATURE_GATES: &[&str] = &["VirtualNet"];
/// frp 的「不安全特性」名称：从命令行 `--allow-unsafe` 显式放开。
pub const UNSAFE_TOKEN_SOURCE_EXEC: &str = "TokenSourceExec";
const MAX_ANNOTATIONS_BYTES: usize = 256 * 1024;

fn join(errs: Vec<String>) -> Result<(), ConfigError> {
    if errs.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::Validation(errs.join("\n")))
    }
}

fn fmt_list(items: &[&str]) -> String {
    format!("[{}]", items.join(" "))
}

/// 端口范围 0..=65535。
pub fn validate_port(port: i64, field: &str) -> Result<(), String> {
    if (0..=65535).contains(&port) {
        Ok(())
    } else {
        Err(format!(
            "{field}: port number {port} must be in the range 0..65535"
        ))
    }
}

fn unsafe_allowed(allowed: &[String], feature: &str) -> Result<(), String> {
    if allowed.iter().any(|a| a == feature) {
        Ok(())
    } else {
        Err(format!(
            "unsafe feature \"{feature}\" is not enabled. To enable it, ensure it is allowed in the configuration or command line flags"
        ))
    }
}

fn validate_value_source(vs: &ValueSource) -> Result<(), String> {
    match vs.kind.as_str() {
        "file" => match &vs.file {
            None => Err("file configuration is required when type is 'file'".to_string()),
            Some(f) if f.path.is_empty() => Err("file path cannot be empty".to_string()),
            Some(_) => Ok(()),
        },
        "exec" => match &vs.exec {
            None => Err("exec configuration is required when type is 'exec'".to_string()),
            Some(e) => {
                if e.command.is_empty() {
                    return Err("exec command cannot be empty".to_string());
                }
                for env in &e.env {
                    if env.name.is_empty() {
                        return Err("exec env name cannot be empty".to_string());
                    }
                    if env.name.contains('=') {
                        return Err("exec env name cannot contain '='".to_string());
                    }
                }
                Ok(())
            }
        },
        other => Err(format!(
            "unsupported value source type: {other} (only 'file' and 'exec' are supported)"
        )),
    }
}

fn validate_token_source(
    token: &str,
    source: Option<&ValueSource>,
    allowed: &[String],
    errs: &mut Vec<String>,
) {
    if !token.is_empty() && source.is_some() {
        errs.push("cannot specify both auth.token and auth.tokenSource".to_string());
    }
    let Some(vs) = source else { return };
    if vs.kind == "exec" {
        if let Err(e) = unsafe_allowed(allowed, UNSAFE_TOKEN_SOURCE_EXEC) {
            errs.push(e);
        }
    }
    if let Err(e) = validate_value_source(vs) {
        errs.push(format!("invalid auth.tokenSource: {e}"));
    }
}

fn check_scopes_and_method(method: &str, scopes: &[String], errs: &mut Vec<String>) {
    if !AUTH_METHODS.contains(&method) {
        errs.push(format!(
            "invalid auth method, optional values are {}",
            fmt_list(AUTH_METHODS)
        ));
    }
    if !scopes.iter().all(|s| AUTH_SCOPES.contains(&s.as_str())) {
        errs.push(format!(
            "invalid auth additional scopes, optional values are {}",
            fmt_list(AUTH_SCOPES)
        ));
    }
}

fn validate_oidc_client(c: &AuthOidcClient, allowed: &[String], errs: &mut Vec<String>) {
    let Some(src) = &c.token_source else { return };
    let others = !c.client_id.is_empty()
        || !c.client_secret.is_empty()
        || !c.audience.is_empty()
        || !c.scope.is_empty()
        || !c.token_endpoint_url.is_empty()
        || !c.additional_endpoint_params.is_empty()
        || !c.trusted_ca_file.is_empty()
        || c.insecure_skip_verify
        || !c.proxy_url.is_empty();
    if others {
        errs.push(
            "cannot specify both auth.oidc.tokenSource and any other field of auth.oidc"
                .to_string(),
        );
    }
    if src.kind == "exec" {
        if let Err(e) = unsafe_allowed(allowed, UNSAFE_TOKEN_SOURCE_EXEC) {
            errs.push(e);
        }
    }
    if let Err(e) = validate_value_source(src) {
        errs.push(format!("invalid auth.oidc.tokenSource: {e}"));
    }
}

/// 解析绝对 http(s) URL 的 scheme 与 host（够用的轻量检查，避免引入 url 依赖）。
fn absolute_http_url(url: &str) -> Result<(), &'static str> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err("auth.oidc.tokenEndpointURL must be an absolute http or https URL");
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if scheme.is_empty() || host.is_empty() {
        return Err("auth.oidc.tokenEndpointURL must be an absolute http or https URL");
    }
    if scheme != "http" && scheme != "https" {
        return Err("auth.oidc.tokenEndpointURL must use http or https");
    }
    Ok(())
}

fn validate_oidc_credentials(c: &AuthOidcClient) -> Result<(), String> {
    let mut errs: Vec<String> = Vec::new();
    if c.client_id.is_empty() {
        errs.push("auth.oidc.clientID is required".to_string());
    }
    if c.token_endpoint_url.is_empty() {
        errs.push("auth.oidc.tokenEndpointURL is required".to_string());
    } else if let Err(e) = absolute_http_url(&c.token_endpoint_url) {
        errs.push(e.to_string());
    }
    if c.additional_endpoint_params.contains_key("scope") {
        errs.push(
            "auth.oidc.additionalEndpointParams.scope is not allowed; use auth.oidc.scope instead"
                .to_string(),
        );
    }
    if !c.audience.is_empty() && c.additional_endpoint_params.contains_key("audience") {
        errs.push(
            "cannot specify both auth.oidc.audience and auth.oidc.additionalEndpointParams.audience".to_string(),
        );
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("; "))
    }
}

fn validate_log(c: &LogConfig, errs: &mut Vec<String>) {
    if !LOG_LEVELS.contains(&c.level.as_str()) {
        errs.push(format!(
            "invalid log level, optional values are {}",
            fmt_list(LOG_LEVELS)
        ));
    }
}

fn validate_web_server(c: &WebServerConfig, errs: &mut Vec<String>) {
    if let Some(tls) = &c.tls {
        if tls.cert_file.is_empty() {
            errs.push("tls.certFile must be specified when tls is enabled".to_string());
            return;
        }
        if tls.key_file.is_empty() {
            errs.push("tls.keyFile must be specified when tls is enabled".to_string());
            return;
        }
    }
    if let Err(e) = validate_port(c.port, "webServer.port") {
        errs.push(e);
    }
}

/// 校验客户端公共配置（不含代理）。返回 `(警告, 错误)`。
pub fn validate_client_common(
    c: &ClientConfig,
    allowed_unsafe: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut errs = Vec::new();

    for gate in c.feature_gates.keys() {
        if !KNOWN_FEATURE_GATES.contains(&gate.as_str()) {
            errs.push(format!("unrecognized feature gate: {gate}"));
        }
    }
    if !c.virtual_net.address.is_empty() && c.feature_gates.get("VirtualNet") != Some(&true) {
        errs.push(
            "VirtualNet feature is not enabled; enable it by setting the appropriate feature gate flag".to_string(),
        );
    }

    check_scopes_and_method(&c.auth.method, &c.auth.additional_scopes, &mut errs);
    validate_token_source(
        &c.auth.token,
        c.auth.token_source.as_ref(),
        allowed_unsafe,
        &mut errs,
    );
    validate_oidc_client(&c.auth.oidc, allowed_unsafe, &mut errs);
    if c.auth.method == "oidc" && c.auth.oidc.token_source.is_none() {
        if let Err(e) = validate_oidc_credentials(&c.auth.oidc) {
            errs.push(e);
        }
    }

    validate_log(&c.log, &mut errs);
    validate_web_server(&c.web_server, &mut errs);

    let t = &c.transport;
    if t.heartbeat_timeout > 0
        && t.heartbeat_interval > 0
        && t.heartbeat_timeout < t.heartbeat_interval
    {
        errs.push(
            "invalid transport.heartbeatTimeout, heartbeat timeout should not less than heartbeat interval"
                .to_string(),
        );
    }
    if t.tls.enable != Some(true) {
        for (name, value) in [
            ("transport.tls.certFile", &t.tls.cert_file),
            ("transport.tls.keyFile", &t.tls.key_file),
            ("transport.tls.trustedCaFile", &t.tls.trusted_ca_file),
        ] {
            if !value.is_empty() {
                warnings.push(format!(
                    "{name} is invalid when transport.tls.enable is false"
                ));
            }
        }
    }
    if !TRANSPORT_PROTOCOLS.contains(&t.protocol.as_str()) {
        errs.push(format!(
            "invalid transport.protocol, optional values are {}",
            fmt_list(TRANSPORT_PROTOCOLS)
        ));
    }
    if !WIRE_PROTOCOLS.contains(&t.wire_protocol.as_str()) {
        errs.push(format!(
            "invalid transport.wireProtocol, optional values are {}",
            fmt_list(WIRE_PROTOCOLS)
        ));
    }
    (warnings, errs)
}

/// k8s 风格的限定名检查（用于注解键），返回错误描述。
fn qualified_name_errors(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let parts: Vec<&str> = name.split('/').collect();
    let (prefix, local) = match parts.as_slice() {
        [local] => (None, *local),
        [prefix, local] => (Some(*prefix), *local),
        _ => {
            out.push("a qualified name must consist of an optional DNS subdomain prefix and a name separated by a '/'".to_string());
            return out;
        }
    };
    if let Some(p) = prefix {
        if p.is_empty() {
            out.push("prefix part must be non-empty".to_string());
        } else if !is_dns_subdomain(p) {
            out.push("prefix part must be a lowercase RFC 1123 subdomain".to_string());
        }
    }
    if local.is_empty() {
        out.push("name part must be non-empty".to_string());
    } else {
        if local.len() > 63 {
            out.push("name part must be no more than 63 characters".to_string());
        }
        let ok_chars = local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        let ok_ends = local
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
            && local
                .chars()
                .last()
                .is_some_and(|c| c.is_ascii_alphanumeric());
        if !ok_chars || !ok_ends {
            out.push("name part must consist of alphanumeric characters, '-', '_' or '.', and must start and end with an alphanumeric character".to_string());
        }
    }
    out
}

fn is_dns_subdomain(s: &str) -> bool {
    s.len() <= 253
        && s.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

fn validate_annotations(p: &Proxy) -> Result<(), String> {
    let mut errs = Vec::new();
    for k in p.annotations.keys() {
        for m in qualified_name_errors(&k.to_lowercase()) {
            errs.push(format!("annotation key {k} is invalid: {m}"));
        }
    }
    let total: usize = p.annotations.iter().map(|(k, v)| k.len() + v.len()).sum();
    if total > MAX_ANNOTATIONS_BYTES {
        errs.push(format!(
            "annotations size {total} is larger than limit {MAX_ANNOTATIONS_BYTES}"
        ));
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs.join("\n"))
    }
}

fn validate_plugin(p: &Plugin) -> Result<(), String> {
    let need = |v: &str, name: &str| {
        if v.is_empty() {
            Err(format!("{name} is required"))
        } else {
            Ok(())
        }
    };
    match p.kind.as_str() {
        "http2https" | "https2http" | "https2https" | "tls2raw" => need(&p.local_addr, "localAddr"),
        "static_file" => need(&p.local_path, "localPath"),
        "unix_domain_socket" => need(&p.unix_path, "unixPath"),
        _ => Ok(()),
    }
}

fn validate_domain_for_client(p: &Proxy) -> Result<(), String> {
    if p.subdomain.is_empty() && p.custom_domains.is_empty() {
        Err("subdomain and custom domains should not be both empty".to_string())
    } else {
        Ok(())
    }
}

/// 校验单个代理（客户端视角）。
pub fn validate_proxy_for_client(p: &Proxy) -> Result<(), String> {
    if p.name.is_empty() {
        return Err("name should not be empty".to_string());
    }
    validate_annotations(p)?;
    if !["", "v1", "v2"].contains(&p.transport.proxy_protocol_version.as_str()) {
        return Err(format!(
            "not support proxy protocol version: {}",
            p.transport.proxy_protocol_version
        ));
    }
    if !["client", "server"].contains(&p.transport.bandwidth_limit_mode.as_str()) {
        return Err("bandwidth limit mode should be client or server".to_string());
    }
    let plugin_kind = p.plugin.as_ref().map(|x| x.kind.as_str()).unwrap_or("");
    if plugin_kind.is_empty() {
        validate_port(p.local_port, "localPort").map_err(|e| format!("localPort: {e}"))?;
    }
    if !["", "tcp", "http"].contains(&p.health_check.kind.as_str()) {
        return Err(format!(
            "not support health check type: {}",
            p.health_check.kind
        ));
    }
    if p.health_check.kind == "http" && p.health_check.path.is_empty() {
        return Err("health check path should not be empty".to_string());
    }
    if let Some(plugin) = p.plugin.as_ref().filter(|x| !x.kind.is_empty()) {
        validate_plugin(plugin).map_err(|e| format!("plugin {}: {e}", plugin.kind))?;
    }
    match p.kind.as_str() {
        "tcpmux" => {
            validate_domain_for_client(p)?;
            if p.multiplexer != "httpconnect" {
                return Err(format!("not support multiplexer: {}", p.multiplexer));
            }
            Ok(())
        }
        "http" | "https" => validate_domain_for_client(p),
        "tcp" | "udp" | "stcp" | "xtcp" | "sudp" => Ok(()),
        _ => Err("unknown proxy config type".to_string()),
    }
}

/// 校验访问者。
pub fn validate_visitor(v: &Visitor) -> Result<(), String> {
    if v.name.is_empty() {
        return Err("name is required".to_string());
    }
    if v.server_name.is_empty() {
        return Err("server name is required".to_string());
    }
    if v.bind_port == 0 {
        return Err("bind port is required".to_string());
    }
    match v.kind.as_str() {
        "xtcp" if !["kcp", "quic"].contains(&v.protocol.as_str()) => {
            Err("protocol should be kcp or quic".to_string())
        }
        "stcp" | "sudp" | "xtcp" => Ok(()),
        _ => Err("unknown visitor config type".to_string()),
    }
}

/// 校验客户端全部配置：公共部分的所有错误一并返回，随后按 frp 行为在第一个坏代理 / 访问者处停止。
///
/// 返回警告列表；失败时返回 [`ConfigError::Validation`]。
pub fn validate_client_all(
    common: &ClientConfig,
    proxies: &[Proxy],
    visitors: &[Visitor],
    allowed_unsafe: &[String],
) -> Result<Vec<String>, ConfigError> {
    let (warnings, errs) = validate_client_common(common, allowed_unsafe);
    join(errs)?;
    for p in proxies {
        validate_proxy_for_client(p)
            .map_err(|e| ConfigError::Validation(format!("proxy {}: {e}", p.name)))?;
    }
    for v in visitors {
        validate_visitor(v)
            .map_err(|e| ConfigError::Validation(format!("visitor {}: {e}", v.name)))?;
    }
    Ok(warnings)
}

/// 校验服务端配置。
pub fn validate_server(c: &ServerConfig, allowed_unsafe: &[String]) -> Result<(), ConfigError> {
    let mut errs = Vec::new();
    check_scopes_and_method(&c.auth.method, &c.auth.additional_scopes, &mut errs);
    validate_token_source(
        &c.auth.token,
        c.auth.token_source.as_ref(),
        allowed_unsafe,
        &mut errs,
    );
    validate_log(&c.log, &mut errs);
    validate_web_server(&c.web_server, &mut errs);
    for (port, name) in [
        (c.bind_port, "bindPort"),
        (c.kcp_bind_port, "kcpBindPort"),
        (c.quic_bind_port, "quicBindPort"),
        (c.vhost_http_port, "vhostHTTPPort"),
        (c.vhost_https_port, "vhostHTTPSPort"),
        (c.tcpmux_http_connect_port, "tcpMuxHTTPConnectPort"),
    ] {
        if let Err(e) = validate_port(port, name) {
            errs.push(e);
        }
    }
    if c.transport.max_pool_count < 0 {
        errs.push("invalid transport.maxPoolCount, must be non-negative".to_string());
    }
    for p in &c.http_plugins {
        if !p.ops.iter().all(|o| HTTP_PLUGIN_OPS.contains(&o.as_str())) {
            errs.push(format!(
                "invalid http plugin ops, optional values are {}",
                fmt_list(HTTP_PLUGIN_OPS)
            ));
        }
    }
    join(errs)
}

fn validate_domain_for_server(p: &Proxy, s: &ServerConfig) -> Result<(), String> {
    let host = s.sub_domain_host.to_lowercase();
    for domain in &p.custom_domains {
        let canonical = domain.to_lowercase();
        if !host.is_empty()
            && host.split('.').count() < canonical.split('.').count()
            && canonical.ends_with(&format!(".{host}"))
        {
            return Err(format!(
                "custom domain [{domain}] should not belong to subdomain host [{}]",
                s.sub_domain_host
            ));
        }
    }
    if !p.subdomain.is_empty() {
        if s.sub_domain_host.is_empty() {
            return Err(
                "subdomain is not supported because this feature is not enabled in server"
                    .to_string(),
            );
        }
        if p.subdomain.contains('.') || p.subdomain.contains('*') {
            return Err("'.' and '*' are not supported in subdomain".to_string());
        }
    }
    Ok(())
}

/// 校验代理在服务端是否可注册（端口 / 域名 / 功能开关）。
pub fn validate_proxy_for_server(p: &Proxy, s: &ServerConfig) -> Result<(), String> {
    validate_annotations(p)?;
    match p.kind.as_str() {
        "tcp" | "udp" | "stcp" | "xtcp" | "sudp" => Ok(()),
        "tcpmux" => {
            if p.multiplexer == "httpconnect" && s.tcpmux_http_connect_port == 0 {
                return Err(
                    "tcpmux with multiplexer httpconnect not supported because this feature is not enabled in server"
                        .to_string(),
                );
            }
            validate_domain_for_server(p, s)
        }
        "http" => {
            if s.vhost_http_port == 0 {
                return Err("type [http] not supported when vhost http port is not set".to_string());
            }
            validate_domain_for_server(p, s)
        }
        "https" => {
            if s.vhost_https_port == 0 {
                return Err(
                    "type [https] not supported when vhost https port is not set".to_string(),
                );
            }
            validate_domain_for_server(p, s)
        }
        _ => Err("unknown proxy config type".to_string()),
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use crate::frpcfg::env::MapEnv;

    fn client() -> ClientConfig {
        let mut c = ClientConfig::default();
        c.complete(&MapEnv::default());
        c
    }

    fn server() -> ServerConfig {
        let mut s = ServerConfig::default();
        s.complete();
        s
    }

    fn proxy(kind: &str) -> Proxy {
        let mut p = Proxy {
            name: "p".into(),
            kind: kind.into(),
            local_port: 80,
            ..Default::default()
        };
        p.complete();
        p
    }

    fn errs(c: &ClientConfig) -> String {
        validate_client_common(c, &[]).1.join("|")
    }

    #[test]
    fn default_configs_are_valid() {
        let (w, e) = validate_client_common(&client(), &[]);
        assert!(w.is_empty() && e.is_empty());
        assert!(validate_server(&server(), &[]).is_ok());
    }

    #[test]
    fn port_validation() {
        assert!(validate_port(0, "x").is_ok() && validate_port(65535, "x").is_ok());
        assert_eq!(
            validate_port(65536, "bindPort").unwrap_err(),
            "bindPort: port number 65536 must be in the range 0..65535"
        );
        assert!(validate_port(-1, "x").is_err());
    }

    #[test]
    fn feature_gates() {
        let mut c = client();
        c.feature_gates.insert("Nope".into(), true);
        assert!(errs(&c).contains("unrecognized feature gate: Nope"));
        let mut c = client();
        c.virtual_net.address = "1.1.1.1/24".into();
        assert!(errs(&c).contains("VirtualNet feature is not enabled"));
        c.feature_gates.insert("VirtualNet".into(), true);
        assert_eq!(errs(&c), "");
    }

    #[test]
    fn auth_method_and_scopes() {
        let mut c = client();
        c.auth.method = "basic".into();
        c.auth.additional_scopes = vec!["Nope".into()];
        let e = errs(&c);
        assert!(e.contains("invalid auth method") && e.contains("invalid auth additional scopes"));
        c.auth.method = "token".into();
        c.auth.additional_scopes = vec!["HeartBeats".into(), "NewWorkConns".into()];
        assert_eq!(errs(&c), "");
    }

    #[test]
    fn token_and_token_source() {
        let mut c = client();
        c.auth.token = "t".into();
        c.auth.token_source = Some(ValueSource {
            kind: "file".into(),
            file: Some(FileSource { path: "/t".into() }),
            exec: None,
        });
        assert!(errs(&c).contains("cannot specify both auth.token and auth.tokenSource"));

        c.auth.token.clear();
        assert_eq!(errs(&c), "");

        let exec = ValueSource {
            kind: "exec".into(),
            file: None,
            exec: Some(ExecSource {
                command: "echo".into(),
                args: vec![],
                env: vec![ExecEnvVar {
                    name: "A".into(),
                    value: "1".into(),
                }],
            }),
        };
        c.auth.token_source = Some(exec);
        assert!(errs(&c).contains("unsafe feature \"TokenSourceExec\" is not enabled"));
        assert_eq!(
            validate_client_common(&c, &["TokenSourceExec".to_string()])
                .1
                .len(),
            0
        );
    }

    #[test]
    fn value_source_errors() {
        let cases = [
            (
                ValueSource {
                    kind: "file".into(),
                    ..Default::default()
                },
                "file configuration is required",
            ),
            (
                ValueSource {
                    kind: "file".into(),
                    file: Some(FileSource::default()),
                    exec: None,
                },
                "file path cannot be empty",
            ),
            (
                ValueSource {
                    kind: "exec".into(),
                    ..Default::default()
                },
                "exec configuration is required",
            ),
            (
                ValueSource {
                    kind: "exec".into(),
                    file: None,
                    exec: Some(ExecSource::default()),
                },
                "exec command cannot be empty",
            ),
            (
                ValueSource {
                    kind: "exec".into(),
                    file: None,
                    exec: Some(ExecSource {
                        command: "c".into(),
                        args: vec![],
                        env: vec![ExecEnvVar::default()],
                    }),
                },
                "exec env name cannot be empty",
            ),
            (
                ValueSource {
                    kind: "exec".into(),
                    file: None,
                    exec: Some(ExecSource {
                        command: "c".into(),
                        args: vec![],
                        env: vec![ExecEnvVar {
                            name: "A=B".into(),
                            value: String::new(),
                        }],
                    }),
                },
                "cannot contain '='",
            ),
            (
                ValueSource {
                    kind: "http".into(),
                    ..Default::default()
                },
                "unsupported value source type: http",
            ),
        ];
        for (vs, want) in cases {
            let mut c = client();
            c.auth.token_source = Some(vs);
            let e = validate_client_common(&c, &["TokenSourceExec".to_string()])
                .1
                .join("|");
            assert!(e.contains(want), "{want} in {e}");
            assert!(e.contains("invalid auth.tokenSource"));
        }
    }

    #[test]
    fn oidc_credentials() {
        let mut c = client();
        c.auth.method = "oidc".into();
        let e = errs(&c);
        assert!(
            e.contains("auth.oidc.clientID is required")
                && e.contains("auth.oidc.tokenEndpointURL is required")
        );

        c.auth.oidc.client_id = "id".into();
        for (url, want) in [
            ("nourl", "must be an absolute http or https URL"),
            ("http://", "must be an absolute http or https URL"),
            ("://host", "must be an absolute http or https URL"),
            ("ftp://host/x", "must use http or https"),
        ] {
            c.auth.oidc.token_endpoint_url = url.into();
            assert!(errs(&c).contains(want), "{url}");
        }
        c.auth.oidc.token_endpoint_url = "https://idp.example.com/token?x=1".into();
        assert_eq!(errs(&c), "");

        c.auth
            .oidc
            .additional_endpoint_params
            .insert("scope".into(), "x".into());
        c.auth.oidc.audience = "aud".into();
        c.auth
            .oidc
            .additional_endpoint_params
            .insert("audience".into(), "y".into());
        let e = errs(&c);
        assert!(e.contains("additionalEndpointParams.scope is not allowed"));
        assert!(e.contains("cannot specify both auth.oidc.audience"));
    }

    #[test]
    fn oidc_token_source_excludes_other_fields() {
        let mut c = client();
        c.auth.method = "oidc".into();
        c.auth.oidc.token_source = Some(ValueSource {
            kind: "file".into(),
            file: Some(FileSource { path: "p".into() }),
            exec: None,
        });
        assert_eq!(errs(&c), "");
        c.auth.oidc.client_id = "id".into();
        assert!(errs(&c).contains("cannot specify both auth.oidc.tokenSource"));
        c.auth.oidc.client_id.clear();
        c.auth.oidc.token_source = Some(ValueSource {
            kind: "exec".into(),
            ..Default::default()
        });
        let e = errs(&c);
        assert!(e.contains("TokenSourceExec") && e.contains("invalid auth.oidc.tokenSource"));
        for set in [
            |o: &mut AuthOidcClient| o.client_secret = "x".into(),
            |o: &mut AuthOidcClient| o.audience = "x".into(),
            |o: &mut AuthOidcClient| o.scope = "x".into(),
            |o: &mut AuthOidcClient| o.token_endpoint_url = "x".into(),
            |o: &mut AuthOidcClient| {
                o.additional_endpoint_params.insert("a".into(), "b".into());
            },
            |o: &mut AuthOidcClient| o.trusted_ca_file = "x".into(),
            |o: &mut AuthOidcClient| o.insecure_skip_verify = true,
            |o: &mut AuthOidcClient| o.proxy_url = "x".into(),
        ] {
            let mut c = client();
            c.auth.oidc.token_source = Some(ValueSource {
                kind: "file".into(),
                file: Some(FileSource { path: "p".into() }),
                exec: None,
            });
            set(&mut c.auth.oidc);
            assert!(errs(&c).contains("cannot specify both auth.oidc.tokenSource"));
        }
    }

    #[test]
    fn log_and_web_server() {
        let mut c = client();
        c.log.level = "loud".into();
        assert!(errs(&c).contains("invalid log level"));
        let mut c = client();
        c.web_server.port = 70000;
        assert!(errs(&c).contains("webServer.port"));
        c.web_server.port = 0;
        c.web_server.tls = Some(TlsConfig::default());
        assert!(errs(&c).contains("tls.certFile must be specified"));
        c.web_server.tls = Some(TlsConfig {
            cert_file: "c".into(),
            ..Default::default()
        });
        assert!(errs(&c).contains("tls.keyFile must be specified"));
        c.web_server.tls = Some(TlsConfig {
            cert_file: "c".into(),
            key_file: "k".into(),
            ..Default::default()
        });
        assert_eq!(errs(&c), "");
    }

    #[test]
    fn transport_rules() {
        let mut c = client();
        c.transport.heartbeat_interval = 30;
        c.transport.heartbeat_timeout = 10;
        assert!(errs(&c).contains("heartbeat timeout should not less than heartbeat interval"));
        c.transport.heartbeat_timeout = 90;
        assert_eq!(errs(&c), "");
        c.transport.protocol = "udp".into();
        c.transport.wire_protocol = "v9".into();
        let e = errs(&c);
        assert!(
            e.contains("invalid transport.protocol")
                && e.contains("invalid transport.wireProtocol")
        );
    }

    #[test]
    fn tls_files_warn_when_tls_disabled() {
        let mut c = client();
        c.transport.tls.enable = Some(false);
        c.transport.tls.cert_file = "c".into();
        c.transport.tls.key_file = "k".into();
        c.transport.tls.trusted_ca_file = "ca".into();
        let (w, e) = validate_client_common(&c, &[]);
        assert!(e.is_empty());
        assert_eq!(w.len(), 3);
        assert!(
            w[0].contains("transport.tls.certFile is invalid when transport.tls.enable is false")
        );
    }

    #[test]
    fn proxy_client_rules() {
        let mut p = proxy("tcp");
        assert!(validate_proxy_for_client(&p).is_ok());
        p.name.clear();
        assert_eq!(
            validate_proxy_for_client(&p).unwrap_err(),
            "name should not be empty"
        );

        let mut p = proxy("tcp");
        p.transport.proxy_protocol_version = "v3".into();
        assert!(validate_proxy_for_client(&p)
            .unwrap_err()
            .contains("not support proxy protocol version: v3"));
        let mut p = proxy("tcp");
        p.transport.bandwidth_limit_mode = "both".into();
        assert!(validate_proxy_for_client(&p)
            .unwrap_err()
            .contains("bandwidth limit mode"));
        let mut p = proxy("tcp");
        p.local_port = 70000;
        assert!(validate_proxy_for_client(&p)
            .unwrap_err()
            .starts_with("localPort: localPort: port number"));
        // 有插件时不要求 localPort。
        p.plugin = Some(Plugin {
            kind: "socks5".into(),
            ..Default::default()
        });
        assert!(validate_proxy_for_client(&p).is_ok());
        let mut p = proxy("tcp");
        p.health_check.kind = "icmp".into();
        assert!(validate_proxy_for_client(&p)
            .unwrap_err()
            .contains("not support health check type: icmp"));
        p.health_check.kind = "http".into();
        assert!(validate_proxy_for_client(&p)
            .unwrap_err()
            .contains("health check path should not be empty"));
        p.health_check.path = "/h".into();
        assert!(validate_proxy_for_client(&p).is_ok());
        p.health_check.kind = "tcp".into();
        assert!(validate_proxy_for_client(&p).is_ok());
        assert!(validate_proxy_for_client(&proxy("ftp"))
            .unwrap_err()
            .contains("unknown proxy config type"));
    }

    #[test]
    fn proxy_domain_and_multiplexer_rules() {
        for kind in ["http", "https", "tcpmux"] {
            let p = proxy(kind);
            let e = validate_proxy_for_client(&p).unwrap_err();
            assert!(
                e.contains("subdomain and custom domains should not be both empty"),
                "{kind}"
            );
        }
        let mut p = proxy("tcpmux");
        p.subdomain = "s".into();
        assert!(validate_proxy_for_client(&p)
            .unwrap_err()
            .contains("not support multiplexer"));
        p.multiplexer = "httpconnect".into();
        assert!(validate_proxy_for_client(&p).is_ok());
        let mut p = proxy("http");
        p.custom_domains = vec!["a.com".into()];
        assert!(validate_proxy_for_client(&p).is_ok());
    }

    #[test]
    fn plugin_required_fields() {
        for (kind, want) in [
            ("http2https", "localAddr"),
            ("https2http", "localAddr"),
            ("https2https", "localAddr"),
            ("tls2raw", "localAddr"),
            ("static_file", "localPath"),
            ("unix_domain_socket", "unixPath"),
        ] {
            let mut p = proxy("tcp");
            p.plugin = Some(Plugin {
                kind: kind.into(),
                ..Default::default()
            });
            let e = validate_proxy_for_client(&p).unwrap_err();
            assert_eq!(e, format!("plugin {kind}: {want} is required"));
        }
        let mut p = proxy("tcp");
        p.plugin = Some(Plugin {
            kind: "http_proxy".into(),
            ..Default::default()
        });
        assert!(validate_proxy_for_client(&p).is_ok());
    }

    #[test]
    fn annotations() {
        let mut p = proxy("tcp");
        p.annotations
            .insert("example.com/ok-key_1".into(), "v".into());
        assert!(validate_proxy_for_client(&p).is_ok());
        for bad in [
            "",
            "a/b/c",
            "/x",
            "Bad_Prefix.COM/x",
            "pre fix/x",
            "x/",
            "-a",
            "a-",
            "a b",
            &"a".repeat(64),
        ] {
            let mut p = proxy("tcp");
            p.annotations.insert(bad.to_string(), "v".into());
            assert!(validate_proxy_for_client(&p).is_err(), "{bad:?}");
        }
        let mut p = proxy("tcp");
        p.annotations
            .insert("big".into(), "x".repeat(MAX_ANNOTATIONS_BYTES + 1));
        assert!(validate_proxy_for_client(&p)
            .unwrap_err()
            .contains("is larger than limit"));
        assert!(is_dns_subdomain("a.b-c.d"));
        assert!(!is_dns_subdomain(&format!("{}.com", "a".repeat(64))));
        assert!(!is_dns_subdomain(&"a.".repeat(130)));
    }

    #[test]
    fn visitors() {
        let ok = Visitor {
            name: "v".into(),
            kind: "stcp".into(),
            server_name: "s".into(),
            bind_port: 1,
            ..Default::default()
        };
        assert!(validate_visitor(&ok).is_ok());
        assert_eq!(
            validate_visitor(&Visitor {
                name: String::new(),
                ..ok.clone()
            })
            .unwrap_err(),
            "name is required"
        );
        assert_eq!(
            validate_visitor(&Visitor {
                server_name: String::new(),
                ..ok.clone()
            })
            .unwrap_err(),
            "server name is required"
        );
        assert_eq!(
            validate_visitor(&Visitor {
                bind_port: 0,
                ..ok.clone()
            })
            .unwrap_err(),
            "bind port is required"
        );
        assert!(validate_visitor(&Visitor {
            bind_port: -1,
            ..ok.clone()
        })
        .is_ok());
        assert_eq!(
            validate_visitor(&Visitor {
                kind: "xtcp".into(),
                protocol: "tcp".into(),
                ..ok.clone()
            })
            .unwrap_err(),
            "protocol should be kcp or quic"
        );
        assert!(validate_visitor(&Visitor {
            kind: "xtcp".into(),
            protocol: "kcp".into(),
            ..ok.clone()
        })
        .is_ok());
        assert!(validate_visitor(&Visitor {
            kind: "sudp".into(),
            ..ok.clone()
        })
        .is_ok());
        assert_eq!(
            validate_visitor(&Visitor {
                kind: "tcp".into(),
                ..ok
            })
            .unwrap_err(),
            "unknown visitor config type"
        );
    }

    #[test]
    fn validate_all_client_flow() {
        let c = client();
        let good = proxy("tcp");
        assert!(
            validate_client_all(&c, std::slice::from_ref(&good), &[], &[])
                .unwrap()
                .is_empty()
        );

        let mut bad_common = client();
        bad_common.log.level = "x".into();
        assert!(
            matches!(validate_client_all(&bad_common, &[], &[], &[]), Err(ConfigError::Validation(m)) if m.contains("log level"))
        );

        let mut bad = good;
        bad.name = "web".into();
        bad.local_port = 99999;
        let e = validate_client_all(&c, &[bad], &[], &[])
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("proxy web: localPort:"));

        let v = Visitor {
            name: "vv".into(),
            kind: "stcp".into(),
            ..Default::default()
        };
        let e = validate_client_all(&c, &[], &[v], &[])
            .unwrap_err()
            .to_string();
        assert_eq!(e, "visitor vv: server name is required");
    }

    #[test]
    fn server_rules() {
        let mut s = server();
        s.auth.method = "x".into();
        s.auth.additional_scopes = vec!["Q".into()];
        s.auth.token = "t".into();
        s.auth.token_source = Some(ValueSource {
            kind: "exec".into(),
            ..Default::default()
        });
        s.log.level = "x".into();
        s.web_server.port = -5;
        s.bind_port = 70000;
        s.kcp_bind_port = 70000;
        s.quic_bind_port = 70000;
        s.vhost_http_port = 70000;
        s.vhost_https_port = 70000;
        s.tcpmux_http_connect_port = 70000;
        s.transport.max_pool_count = -1;
        s.http_plugins = vec![HttpPluginOptions {
            ops: vec!["Bad".into()],
            ..Default::default()
        }];
        let e = validate_server(&s, &[]).unwrap_err().to_string();
        for want in [
            "invalid auth method",
            "additional scopes",
            "cannot specify both auth.token",
            "TokenSourceExec",
            "log level",
            "webServer.port",
            "bindPort",
            "kcpBindPort",
            "quicBindPort",
            "vhostHTTPPort",
            "vhostHTTPSPort",
            "tcpMuxHTTPConnectPort",
            "maxPoolCount",
            "http plugin ops",
        ] {
            assert!(e.contains(want), "{want} in {e}");
        }
        let mut s = server();
        s.http_plugins = vec![HttpPluginOptions {
            ops: vec!["Login".into(), "NewProxy".into()],
            ..Default::default()
        }];
        assert!(validate_server(&s, &[]).is_ok());
    }

    #[test]
    fn proxy_for_server_rules() {
        let mut s = server();
        for kind in ["tcp", "udp", "stcp", "xtcp", "sudp"] {
            assert!(validate_proxy_for_server(&proxy(kind), &s).is_ok());
        }
        assert!(validate_proxy_for_server(&proxy("ftp"), &s)
            .unwrap_err()
            .contains("unknown proxy config type"));

        assert!(validate_proxy_for_server(&proxy("http"), &s)
            .unwrap_err()
            .contains("vhost http port is not set"));
        assert!(validate_proxy_for_server(&proxy("https"), &s)
            .unwrap_err()
            .contains("vhost https port is not set"));
        let mut mux = proxy("tcpmux");
        mux.multiplexer = "httpconnect".into();
        assert!(validate_proxy_for_server(&mux, &s)
            .unwrap_err()
            .contains("tcpmux with multiplexer httpconnect not supported"));

        s.vhost_http_port = 80;
        s.vhost_https_port = 443;
        s.tcpmux_http_connect_port = 1337;
        s.sub_domain_host = "frps.com".into();
        assert!(validate_proxy_for_server(&proxy("http"), &s).is_ok());
        assert!(validate_proxy_for_server(&proxy("https"), &s).is_ok());
        assert!(validate_proxy_for_server(&mux, &s).is_ok());

        let mut p = proxy("http");
        p.custom_domains = vec!["a.FRPS.com".into()];
        assert!(validate_proxy_for_server(&p, &s)
            .unwrap_err()
            .contains("should not belong to subdomain host"));
        p.custom_domains = vec!["frps.com".into(), "other.org".into()];
        assert!(validate_proxy_for_server(&p, &s).is_ok());
        p.subdomain = "a.b".into();
        assert!(validate_proxy_for_server(&p, &s)
            .unwrap_err()
            .contains("'.' and '*' are not supported"));
        p.subdomain = "ok".into();
        assert!(validate_proxy_for_server(&p, &s).is_ok());
        s.sub_domain_host.clear();
        assert!(validate_proxy_for_server(&p, &s)
            .unwrap_err()
            .contains("subdomain is not supported"));

        let mut p = proxy("tcp");
        p.annotations.insert("-bad".into(), "v".into());
        assert!(validate_proxy_for_server(&p, &s).is_err());
    }

    #[test]
    fn audience_alone_is_fine() {
        let mut c = client();
        c.auth.method = "oidc".into();
        c.auth.oidc.client_id = "id".into();
        c.auth.oidc.token_endpoint_url = "http://idp/t".into();
        c.auth.oidc.audience = "aud".into();
        assert_eq!(errs(&c), "");
        c.auth
            .oidc
            .additional_endpoint_params
            .insert("other".into(), "x".into());
        assert_eq!(errs(&c), "");
    }

    #[test]
    fn heartbeat_checks_skip_disabled_values() {
        for (interval, timeout) in [(-1, 5), (30, -1), (0, 5), (30, 0), (30, 30)] {
            let mut c = client();
            c.transport.heartbeat_interval = interval;
            c.transport.heartbeat_timeout = timeout;
            assert_eq!(errs(&c), "", "{interval}/{timeout}");
        }
    }

    #[test]
    fn tls_warnings_only_for_set_files() {
        let mut c = client();
        c.transport.tls.enable = Some(false);
        assert!(validate_client_common(&c, &[]).0.is_empty());
        c.transport.tls.key_file = "k".into();
        assert_eq!(validate_client_common(&c, &[]).0.len(), 1);
    }

    #[test]
    fn dns_subdomain_label_rules() {
        for bad in ["-a.com", "a-.com", "A.com", "a..com", "a_b.com", "", "a.-b"] {
            assert!(!is_dns_subdomain(bad), "{bad:?}");
        }
        assert!(is_dns_subdomain("a1.b-2.example"));
    }

    #[test]
    fn server_side_subdomain_and_tcpmux_variants() {
        let mut s = server();
        s.vhost_http_port = 80;
        s.sub_domain_host = "frps.com".into();
        let mut p = proxy("http");
        p.subdomain = "a*".into();
        assert!(validate_proxy_for_server(&p, &s)
            .unwrap_err()
            .contains("'.' and '*'"));
        // 自定义域名与子域名后缀等长 / 更短时不触发归属检查。
        p.subdomain.clear();
        p.custom_domains = vec!["frps.com".into(), "x.com".into()];
        assert!(validate_proxy_for_server(&p, &s).is_ok());
        // 级数更深但并非该后缀的域名也放行。
        p.custom_domains = vec!["a.b.notfrps.com".into()];
        assert!(validate_proxy_for_server(&p, &s).is_ok());
        let mut mux = proxy("tcpmux");
        mux.multiplexer = "other".into();
        assert!(validate_proxy_for_server(&mux, &s).is_ok());
    }

    #[test]
    fn plugin_with_empty_kind_is_ignored_by_validation() {
        let mut p = proxy("tcp");
        p.plugin = Some(Plugin::default());
        assert!(validate_proxy_for_client(&p).is_ok());
        p.plugin = Some(Plugin {
            kind: "http2https".into(),
            local_addr: "127.0.0.1:80".into(),
            ..Default::default()
        });
        assert!(validate_proxy_for_client(&p).is_ok());
    }

    #[test]
    fn oidc_scheme_variants() {
        for (url, ok) in [
            ("http://h", true),
            ("https://h:8443/p", true),
            ("HTTP://h", false),
            ("https:///x", false),
        ] {
            assert_eq!(absolute_http_url(url).is_ok(), ok, "{url}");
        }
    }
}
