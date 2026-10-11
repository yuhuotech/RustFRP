//! 旧版 INI 到 frp v1 JSON 的转换；未映射字段报错，绝不静默丢弃。
use super::ConfigError;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_SECTIONS: usize = 1024;
type Section = BTreeMap<String, String>;
fn bad(message: impl Into<String>) -> ConfigError {
    ConfigError::Parse(format!("INI conversion: {}", message.into()))
}

fn parse(text: &str) -> Result<BTreeMap<String, Section>, ConfigError> {
    if text.len() > MAX_BYTES {
        return Err(bad("input exceeds 1 MiB"));
    }
    let mut sections = BTreeMap::<String, Section>::new();
    let mut current = String::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if line.starts_with('[') {
            let name = line
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad(format!("invalid section on line {}", number + 1)))?;
            if sections.len() >= MAX_SECTIONS && !sections.contains_key(name) {
                return Err(bad("too many sections"));
            }
            current = name.into();
            sections.entry(current.clone()).or_default();
            continue;
        }
        let (key, value) = line.split_once('=').unwrap_or((line, "true"));
        let key = key.trim();
        if current.is_empty() || key.is_empty() {
            return Err(bad(format!("key outside section on line {}", number + 1)));
        }
        let value = value.trim();
        let value = if value.starts_with('"') && value.ends_with('"')
            || value.starts_with('\'') && value.ends_with('\'')
        {
            value
                .get(1..value.len().saturating_sub(1))
                .unwrap_or_default()
        } else {
            value
        };
        sections
            .entry(current.clone())
            .or_default()
            .insert(key.into(), value.into());
    }
    Ok(sections)
}
fn put(object: &mut Value, path: &str, value: Value) {
    let (head, tail) = path.split_once('.').unwrap_or((path, ""));
    let Value::Object(map) = object else {
        *object = Value::Object(Map::new());
        return put(object, path, value);
    };
    if tail.is_empty() {
        map.insert(head.into(), value);
    } else {
        put(map.entry(head).or_insert_with(|| json!({})), tail, value);
    }
}
fn put_map(object: &mut Value, path: &str, key: &str, value: Value) {
    let mut entry = Map::new();
    entry.insert(key.into(), value);
    let pointer = format!("/{}", path.replace('.', "/"));
    if let Some(existing) = object.pointer_mut(&pointer).and_then(Value::as_object_mut) {
        existing.extend(entry);
    } else {
        put(object, path, Value::Object(entry));
    }
}
fn scalar(value: &str, kind: char) -> Result<Value, ConfigError> {
    match kind {
        'n' => value
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| bad("invalid integer")),
        'b' => match value.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
            _ => Err(bad("invalid boolean")),
        },
        'a' => Ok(Value::Array(
            value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.into()))
                .collect(),
        )),
        _ => Ok(Value::String(value.into())),
    }
}
fn common_field(key: &str, client: bool) -> Option<(&'static str, char)> {
    let common = match key {
        "token" => ("auth.token", 's'),
        "authentication_method" => ("auth.method", 's'),
        "log_file" => ("log.to", 's'),
        "log_level" => ("log.level", 's'),
        "log_max_days" => ("log.maxDays", 'n'),
        "disable_log_color" => ("log.disablePrintColor", 'b'),
        "tcp_mux" => ("transport.tcpMux", 'b'),
        "tcp_mux_keepalive_interval" => ("transport.tcpMuxKeepaliveInterval", 'n'),
        "heartbeat_timeout" => ("transport.heartbeatTimeout", 'n'),
        "tls_cert_file" => ("transport.tls.certFile", 's'),
        "tls_key_file" => ("transport.tls.keyFile", 's'),
        "tls_trusted_ca_file" => ("transport.tls.trustedCaFile", 's'),
        "udp_packet_size" => ("udpPacketSize", 'n'),
        "assets_dir" => ("webServer.assetsDir", 's'),
        "pprof_enable" => ("webServer.pprofEnable", 'b'),
        "quic_keepalive_period" => ("transport.quic.keepalivePeriod", 'n'),
        "quic_max_idle_timeout" => ("transport.quic.maxIdleTimeout", 'n'),
        "quic_max_incoming_streams" => ("transport.quic.maxIncomingStreams", 'n'),
        _ => ("", 's'),
    };
    if !common.0.is_empty() {
        return Some(common);
    }
    if client {
        Some(match key {
            "server_addr" => ("serverAddr", 's'),
            "server_port" => ("serverPort", 'n'),
            "user" => ("user", 's'),
            "protocol" => ("transport.protocol", 's'),
            "pool_count" => ("transport.poolCount", 'n'),
            "nat_hole_stun_server" => ("natHoleStunServer", 's'),
            "dns_server" => ("dnsServer", 's'),
            "login_fail_exit" => ("loginFailExit", 'b'),
            "start" => ("start", 'a'),
            "includes" => ("includes", 'a'),
            "dial_server_timeout" => ("transport.dialServerTimeout", 'n'),
            "dial_server_keepalive" => ("transport.dialServerKeepalive", 'n'),
            "connect_server_local_ip" => ("transport.connectServerLocalIP", 's'),
            "http_proxy" => ("transport.proxyURL", 's'),
            "heartbeat_interval" => ("transport.heartbeatInterval", 'n'),
            "tls_enable" => ("transport.tls.enable", 'b'),
            "disable_custom_tls_first_byte" => ("transport.tls.disableCustomTLSFirstByte", 'b'),
            "tls_server_name" => ("transport.tls.serverName", 's'),
            "admin_addr" => ("webServer.addr", 's'),
            "admin_port" => ("webServer.port", 'n'),
            "admin_user" => ("webServer.user", 's'),
            "admin_pwd" => ("webServer.password", 's'),
            "oidc_client_id" => ("auth.oidc.clientID", 's'),
            "oidc_client_secret" => ("auth.oidc.clientSecret", 's'),
            "oidc_audience" => ("auth.oidc.audience", 's'),
            "oidc_scope" => ("auth.oidc.scope", 's'),
            "oidc_token_endpoint_url" => ("auth.oidc.tokenEndpointURL", 's'),
            _ => return None,
        })
    } else {
        Some(match key {
            "bind_addr" => ("bindAddr", 's'),
            "bind_port" => ("bindPort", 'n'),
            "kcp_bind_port" => ("kcpBindPort", 'n'),
            "quic_bind_port" => ("quicBindPort", 'n'),
            "proxy_bind_addr" => ("proxyBindAddr", 's'),
            "vhost_http_port" => ("vhostHTTPPort", 'n'),
            "vhost_https_port" => ("vhostHTTPSPort", 'n'),
            "vhost_http_timeout" => ("vhostHTTPTimeout", 'n'),
            "tcpmux_httpconnect_port" => ("tcpmuxHTTPConnectPort", 'n'),
            "tcpmux_passthrough" => ("tcpmuxPassthrough", 'b'),
            "dashboard_addr" => ("webServer.addr", 's'),
            "dashboard_port" => ("webServer.port", 'n'),
            "dashboard_user" => ("webServer.user", 's'),
            "dashboard_pwd" => ("webServer.password", 's'),
            "dashboard_tls_cert_file" => ("webServer.tls.certFile", 's'),
            "dashboard_tls_key_file" => ("webServer.tls.keyFile", 's'),
            "enable_prometheus" => ("enablePrometheus", 'b'),
            "detailed_errors_to_client" => ("detailedErrorsToClient", 'b'),
            "subdomain_host" => ("subDomainHost", 's'),
            "custom_404_page" => ("custom404Page", 's'),
            "user_conn_timeout" => ("userConnTimeout", 'n'),
            "max_pool_count" => ("transport.maxPoolCount", 'n'),
            "tcp_keepalive" => ("transport.tcpKeepAlive", 'n'),
            "max_ports_per_client" => ("maxPortsPerClient", 'n'),
            "tls_only" => ("transport.tls.force", 'b'),
            "nat_hole_analysis_data_reserve_hours" => ("natholeAnalysisDataReserveHours", 'n'),
            "oidc_audience" => ("auth.oidc.audience", 's'),
            "oidc_issuer" => ("auth.oidc.issuer", 's'),
            "oidc_skip_expiry_check" => ("auth.oidc.skipExpiryCheck", 'b'),
            "oidc_skip_issuer_check" => ("auth.oidc.skipIssuerCheck", 'b'),
            _ => return None,
        })
    }
}
fn proxy_field(key: &str, visitor: bool) -> Option<(&'static str, char)> {
    let shared = match key {
        "type" => ("type", 's'),
        "use_encryption" => ("transport.useEncryption", 'b'),
        "use_compression" => ("transport.useCompression", 'b'),
        "sk" => ("secretKey", 's'),
        _ => ("", 's'),
    };
    if !shared.0.is_empty() {
        return Some(shared);
    }
    if visitor {
        Some(match key {
            "server_name" => ("serverName", 's'),
            "server_user" => ("serverUser", 's'),
            "bind_addr" => ("bindAddr", 's'),
            "bind_port" => ("bindPort", 'n'),
            "protocol" => ("protocol", 's'),
            "keep_tunnel_open" => ("keepTunnelOpen", 'b'),
            "max_retries_an_hour" => ("maxRetriesAnHour", 'n'),
            "min_retry_interval" => ("minRetryInterval", 'n'),
            "fallback_to" => ("fallbackTo", 's'),
            "fallback_timeout_ms" => ("fallbackTimeoutMs", 'n'),
            _ => return None,
        })
    } else {
        Some(match key {
            "local_ip" => ("localIP", 's'),
            "local_port" => ("localPort", 'n'),
            "remote_port" => ("remotePort", 'n'),
            "custom_domains" => ("customDomains", 'a'),
            "subdomain" => ("subdomain", 's'),
            "locations" => ("locations", 'a'),
            "http_user" => ("httpUser", 's'),
            "http_pwd" => ("httpPassword", 's'),
            "host_header_rewrite" => ("hostHeaderRewrite", 's'),
            "route_by_http_user" => ("routeByHTTPUser", 's'),
            "multiplexer" => ("multiplexer", 's'),
            "allow_users" => ("allowUsers", 'a'),
            "bandwidth_limit" => ("transport.bandwidthLimit", 's'),
            "bandwidth_limit_mode" => ("transport.bandwidthLimitMode", 's'),
            "proxy_protocol_version" => ("transport.proxyProtocolVersion", 's'),
            "group" => ("loadBalancer.group", 's'),
            "group_key" => ("loadBalancer.groupKey", 's'),
            "health_check_type" => ("healthCheck.type", 's'),
            "health_check_timeout_s" => ("healthCheck.timeoutSeconds", 'n'),
            "health_check_max_failed" => ("healthCheck.maxFailed", 'n'),
            "health_check_interval_s" => ("healthCheck.intervalSeconds", 'n'),
            "health_check_url" => ("healthCheck.path", 's'),
            "plugin" => ("plugin.type", 's'),
            "plugin_local_addr" => ("plugin.localAddr", 's'),
            "plugin_host_header_rewrite" => ("plugin.hostHeaderRewrite", 's'),
            "plugin_crt_path" => ("plugin.crtPath", 's'),
            "plugin_key_path" => ("plugin.keyPath", 's'),
            "plugin_http_user" => ("plugin.httpUser", 's'),
            "plugin_http_passwd" => ("plugin.httpPassword", 's'),
            "plugin_user" => ("plugin.username", 's'),
            "plugin_passwd" => ("plugin.password", 's'),
            "plugin_local_path" => ("plugin.localPath", 's'),
            "plugin_strip_prefix" => ("plugin.stripPrefix", 's'),
            "plugin_unix_path" => ("plugin.unixPath", 's'),
            _ => return None,
        })
    }
}
/// 转换为 JSON；未知字段必须报出位置，不生成不完整迁移配置。
pub fn convert(text: &str, client: bool) -> Result<Value, ConfigError> {
    let mut sections = parse(text)?;
    let common = sections
        .remove("common")
        .ok_or_else(|| bad("missing [common] section"))?;
    let mut root = if client {
        json!({"transport":{"tcpMux":true,"tls":{"enable":true,"disableCustomTLSFirstByte":true}},"loginFailExit":true})
    } else {
        json!({"webServer":{"addr":"0.0.0.0"},"detailedErrorsToClient":true,"transport":{"tcpMux":true}})
    };
    let dashboard_material = common
        .keys()
        .any(|key| ["dashboard_tls_cert_file", "dashboard_tls_key_file"].contains(&key.as_str()));
    let mut scopes = vec![];
    for (key, value) in common {
        if key == "authenticate_heartbeats" || key == "authenticate_new_work_conns" {
            if scalar(&value, 'b')? == Value::Bool(true) {
                scopes.push(Value::String(
                    if key == "authenticate_heartbeats" {
                        "HeartBeats"
                    } else {
                        "NewWorkConns"
                    }
                    .into(),
                ));
            }
            continue;
        }
        if key == "allow_ports" && !client {
            let ports = port_ranges(&value)?;
            put(&mut root, "allowPorts", Value::Array(ports));
            continue;
        }
        if key == "dashboard_tls_mode" && !client {
            if scalar(&value, 'b')? == Value::Bool(false) && dashboard_material {
                return Err(bad("dashboard TLS fields require dashboard_tls_mode=true"));
            }
            continue;
        }
        if key == "log_way" {
            if value != "console" && value != "file" {
                return Err(bad("invalid log_way"));
            }
            continue;
        }
        if let Some(name) = key.strip_prefix("meta_") {
            put_map(&mut root, "metadatas", name, Value::String(value));
            continue;
        }
        if client {
            if let Some(name) = key.strip_prefix("oidc_additional_") {
                put_map(
                    &mut root,
                    "auth.oidc.additionalEndpointParams",
                    name,
                    Value::String(value),
                );
                continue;
            }
        }
        let (path, kind) = common_field(&key, client)
            .ok_or_else(|| ConfigError::UnknownField(format!("[common].{key}")))?;
        put(&mut root, path, scalar(&value, kind)?);
    }
    if !scopes.is_empty() {
        put(&mut root, "auth.additionalScopes", Value::Array(scopes));
    }
    let mut proxies = vec![];
    let mut visitors = vec![];
    let mut plugins = vec![];
    for (name, section) in sections {
        if !client {
            let plugin = name
                .strip_prefix("plugin.")
                .ok_or_else(|| bad(format!("unexpected server section [{name}]")))?;
            let mut item = json!({"name":plugin});
            for (key, value) in section {
                let kind = match key.as_str() {
                    "addr" | "path" => 's',
                    "ops" => 'a',
                    "tlsVerify" => 'b',
                    _ => return Err(ConfigError::UnknownField(format!("[{name}].{key}"))),
                };
                put(&mut item, &key, scalar(&value, kind)?);
            }
            plugins.push(item);
            continue;
        }
        let visitor = match section.get("role").map(String::as_str).unwrap_or("server") {
            "server" => false,
            "visitor" => true,
            _ => return Err(bad(format!("invalid role [{name}]"))),
        };
        let mut item =
            json!({"name":name,"type":section.get("type").map(String::as_str).unwrap_or("tcp")});
        for (key, value) in &section {
            if key == "role" {
                continue;
            }
            let dynamic = key
                .strip_prefix("plugin_header_")
                .map(|k| format!("plugin.requestHeaders.set.{k}"))
                .or_else(|| {
                    key.strip_prefix("header_")
                        .map(|k| format!("requestHeaders.set.{k}"))
                })
                .or_else(|| key.strip_prefix("meta_").map(|k| format!("metadatas.{k}")));
            if let Some(path) = dynamic {
                let (base, literal) = if let Some(k) = key.strip_prefix("plugin_header_") {
                    ("plugin.requestHeaders.set", k)
                } else if let Some(k) = key.strip_prefix("header_") {
                    ("requestHeaders.set", k)
                } else {
                    ("metadatas", key.trim_start_matches("meta_"))
                };
                let _ = path;
                put_map(&mut item, base, literal, Value::String(value.clone()));
                continue;
            }
            if name.starts_with("range:") && ["local_port", "remote_port"].contains(&key.as_str()) {
                continue;
            }
            let (path, kind) = proxy_field(key, visitor)
                .ok_or_else(|| ConfigError::UnknownField(format!("[{name}].{key}")))?;
            put(&mut item, path, scalar(value, kind)?);
        }
        if name.starts_with("range:") {
            if visitor {
                return Err(bad("range visitor is not supported by frp"));
            }
            let local = expand_ports(
                section
                    .get("local_port")
                    .map(String::as_str)
                    .unwrap_or_default(),
            )?;
            let remote = expand_ports(
                section
                    .get("remote_port")
                    .map(String::as_str)
                    .unwrap_or_default(),
            )?;
            if local.is_empty() || local.len() != remote.len() {
                return Err(bad("range local and remote port counts differ"));
            }
            for (index, (local, remote)) in local.into_iter().zip(remote).enumerate() {
                let mut expanded = item.clone();
                put(
                    &mut expanded,
                    "name",
                    Value::String(format!(
                        "{}_{index}",
                        name.trim_start_matches("range:").trim()
                    )),
                );
                put(&mut expanded, "localPort", local.into());
                put(&mut expanded, "remotePort", remote.into());
                proxies.push(expanded);
            }
        } else if visitor {
            visitors.push(item);
        } else {
            proxies.push(item);
        }
        if proxies.len() + visitors.len() > MAX_SECTIONS {
            return Err(bad("expanded proxies exceed 1024"));
        }
    }
    if client {
        put(&mut root, "proxies", Value::Array(proxies));
        put(&mut root, "visitors", Value::Array(visitors));
    } else {
        put(&mut root, "httpPlugins", Value::Array(plugins));
    }
    Ok(root)
}
fn port_ranges(value: &str) -> Result<Vec<Value>, ConfigError> {
    let mut out = vec![];
    for part in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (lo, hi) = match part.split_once('-') {
            Some((lo, hi)) => (lo, hi),
            None => (part, part),
        };
        let lo = lo
            .trim()
            .parse::<u16>()
            .map_err(|_| bad("invalid port range"))?;
        let hi = hi
            .trim()
            .parse::<u16>()
            .map_err(|_| bad("invalid port range"))?;
        if lo == 0 || hi < lo {
            return Err(bad("invalid port range"));
        }
        out.push(if lo == hi {
            json!({"single":lo})
        } else {
            json!({"start":lo,"end":hi})
        });
    }
    Ok(out)
}
fn expand_ports(value: &str) -> Result<Vec<u16>, ConfigError> {
    let mut out = vec![];
    for range in port_ranges(value)? {
        let lo = range
            .get("single")
            .or_else(|| range.get("start"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u16;
        let hi = range
            .get("end")
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(lo)) as u16;
        if out.len() + usize::from(hi - lo) + 1 > MAX_SECTIONS {
            return Err(bad("expanded ports exceed 1024"));
        }
        out.extend(lo..=hi);
    }
    Ok(out)
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use crate::frpcfg::{load_client_str, load_server_str, Format, LoadOptions, MapEnv, MemFs};
    #[test]
    fn common_proxy_visitor_range_and_server_roundtrip() {
        let raw="[common]\nserver_addr=example.com\nserver_port=7000\ntoken=secret\ntcp_mux=true\nauthenticate_heartbeats=true\nauthenticate_new_work_conns=false\nmeta_team=ops\noidc_additional_key=value\n[ssh]\ntype=tcp\nlocal_port=22\nremote_port=6000\n[web]\ntype=http\nlocal_port=80\ncustom_domains=a.example.com,b.example.com\nheader_X-Test=value\nplugin_header_X-Plugin=plugin\nmeta_team=ops\n[private]\ntype=stcp\nrole=visitor\nserver_name=ssh\nsk=private\nbind_port=6001\n[range:ports]\nlocal_port=1000-1001\nremote_port=2000-2001\n";
        let value = convert(raw, true).unwrap();
        assert_eq!(value["auth"]["token"], "secret");
        assert_eq!(value["auth"]["additionalScopes"], json!(["HeartBeats"]));
        assert_eq!(value["proxies"].as_array().unwrap().len(), 4);
        assert_eq!(value["visitors"][0]["serverName"], "ssh");
        assert!(value["proxies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "ports_1" && p["localPort"] == 1001));
        // Inspect all mappings; plugin fields are represented, never silently dropped.
        assert!(value["proxies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["requestHeaders"]["set"]["X-Test"] == "value"));
        let value = convert(
            "[common]\nserver_addr=example.com\n[ssh]\nlocal_port=22\nremote_port=6000",
            true,
        )
        .unwrap();
        let (fs, env) = (MemFs::new(), MapEnv::default());
        let opts = LoadOptions {
            strict: true,
            fs: &fs,
            env: &env,
        };
        let loaded = load_client_str(&value.to_string(), Format::Auto, &opts).unwrap();
        assert_eq!(loaded.proxies[0].remote_port, 6000);
        assert_eq!(loaded.common.server_addr, "example.com");
        let server=convert("[common]\nbind_port=7000\nallow_ports=6000-6002,7000\ndashboard_tls_mode=true\n[plugin.auth]\naddr=127.0.0.1:9000\npath=/auth\nops=Login,NewProxy\ntlsVerify=false",false).unwrap();
        let loaded = load_server_str(&server.to_string(), Format::Auto, &opts).unwrap();
        assert_eq!(loaded.source.allow_ports.len(), 2);
        assert_eq!(loaded.source.http_plugins.len(), 1);
        assert_eq!(server["httpPlugins"][0]["name"], "auth");
    }
    #[test]
    fn malformed_limits_and_unknown_fields_are_explicit() {
        for (text, client) in [
            ("x=1", true),
            ("[]=x", true),
            ("[common", true),
            ("[common]\n=x", true),
            ("", true),
            ("[common]\nunknown=1", true),
            ("[common]\n[a]\nunknown=1", true),
            ("[common]\n[a]\nrole=other", true),
            (
                "[common]\n[a]\nrole=visitor\nserver_name=x\nunknown=1",
                true,
            ),
            ("[common]\n[unexpected]", false),
            ("[common]\n[plugin.x]\nunknown=1", false),
            ("[common]\nlog_way=invalid", true),
            (
                "[common]\ndashboard_tls_mode=false\ndashboard_tls_cert_file=cert.pem",
                false,
            ),
            ("[common]\nserver_port=no", true),
            ("[common]\ntcp_mux=maybe", true),
            ("[common]\nallow_ports=10-1", false),
            ("[common]\nallow_ports=0", false),
            ("[common]\nallow_ports=abc", false),
            ("[common]\nallow_ports=1-abc", false),
            ("[common]\n[range:x]\nlocal_port=1-2\nremote_port=1", true),
            ("[common]\n[range:x]", true),
            ("[common]\n[range:x]\nrole=visitor", true),
            (
                "[common]\n[range:x]\nlocal_port=1-1025\nremote_port=1-1025",
                true,
            ),
        ] {
            assert!(convert(text, client).is_err(), "{text}");
        }
        assert!(convert(&"x".repeat(MAX_BYTES + 1), true).is_err());
        assert!(convert("[common]\n[range:a]\nlocal_port=1-600\nremote_port=1-600\n[range:b]\nlocal_port=601-1200\nremote_port=601-1200",true).is_err());
        let sections = (0..MAX_SECTIONS + 1)
            .map(|n| format!("[s{n}]\n"))
            .collect::<String>();
        assert!(convert(&sections, true).is_err());
        assert_eq!(
            convert(
                "; comment\n# comment\n[common]\ntcp_mux\nserver_addr='localhost'\nlog_way=console",
                true
            )
            .unwrap()["serverAddr"],
            "localhost"
        );
        let merged=convert("[common]\nserver_addr=old\n[common]\nserver_addr=new\nmeta_a.b=first\nmeta_second=value",true).unwrap();
        assert_eq!(merged["serverAddr"], "new");
        assert_eq!(merged["metadatas"]["a.b"], "first");
        assert_eq!(merged["metadatas"]["second"], "value");
        assert_eq!(scalar("false", 'b').unwrap(), false);
        assert_eq!(scalar("YES", 'b').unwrap(), true);
        assert!(scalar("invalid", 'n').is_err());
    }
    #[test]
    fn field_tables_preserve_typed_values() {
        // Cover every field mapping with an assertion on its output path/type.
        let common_keys="token authentication_method log_file log_level log_max_days disable_log_color tcp_mux tcp_mux_keepalive_interval heartbeat_timeout tls_cert_file tls_key_file tls_trusted_ca_file udp_packet_size assets_dir pprof_enable quic_keepalive_period quic_max_idle_timeout quic_max_incoming_streams server_addr server_port user protocol pool_count nat_hole_stun_server dns_server login_fail_exit start includes dial_server_timeout dial_server_keepalive connect_server_local_ip http_proxy heartbeat_interval tls_enable disable_custom_tls_first_byte tls_server_name admin_addr admin_port admin_user admin_pwd oidc_client_id oidc_client_secret oidc_audience oidc_scope oidc_token_endpoint_url bind_addr bind_port kcp_bind_port quic_bind_port proxy_bind_addr vhost_http_port vhost_https_port vhost_http_timeout tcpmux_httpconnect_port tcpmux_passthrough dashboard_addr dashboard_port dashboard_user dashboard_pwd dashboard_tls_cert_file dashboard_tls_key_file enable_prometheus detailed_errors_to_client subdomain_host custom_404_page user_conn_timeout max_pool_count tcp_keepalive max_ports_per_client tls_only nat_hole_analysis_data_reserve_hours oidc_issuer oidc_skip_expiry_check oidc_skip_issuer_check";
        for key in common_keys.split_whitespace() {
            for client in [true, false] {
                if let Some((path, kind)) = common_field(key, client) {
                    let value = scalar(
                        match kind {
                            'n' => "1",
                            'b' => "true",
                            'a' => "a,b",
                            _ => "value",
                        },
                        kind,
                    )
                    .unwrap();
                    let mut output = json!({});
                    put(&mut output, path, value.clone());
                    let pointer = format!("/{}", path.replace('.', "/"));
                    assert_eq!(output.pointer(&pointer), Some(&value), "{key}");
                }
            }
        }
        let proxy_keys="type use_encryption use_compression sk server_name server_user bind_addr bind_port protocol keep_tunnel_open max_retries_an_hour min_retry_interval fallback_to fallback_timeout_ms local_ip local_port remote_port custom_domains subdomain locations http_user http_pwd host_header_rewrite route_by_http_user multiplexer allow_users bandwidth_limit bandwidth_limit_mode proxy_protocol_version group group_key health_check_type health_check_timeout_s health_check_max_failed health_check_interval_s health_check_url plugin plugin_local_addr plugin_host_header_rewrite plugin_crt_path plugin_key_path plugin_http_user plugin_http_passwd plugin_user plugin_passwd plugin_local_path plugin_strip_prefix plugin_unix_path";
        for key in proxy_keys.split_whitespace() {
            for visitor in [true, false] {
                if let Some((path, kind)) = proxy_field(key, visitor) {
                    let value = scalar(
                        match kind {
                            'n' => "1",
                            'b' => "true",
                            'a' => "a,b",
                            _ => "value",
                        },
                        kind,
                    )
                    .unwrap();
                    let mut output = Value::Null;
                    put(&mut output, path, value.clone());
                    assert_eq!(
                        output.pointer(&format!("/{}", path.replace('.', "/"))),
                        Some(&value)
                    );
                }
            }
        }
        assert!(common_field("unknown", false).is_none());
        assert!(proxy_field("unknown", true).is_none());
    }
    #[test]
    fn lexical_and_mapping_boundaries_match_legacy_input() {
        for value in ["\"quoted\"", "\"unclosed", "'unclosed", "\"", "'"] {
            assert!(parse(&format!("[common]\nserver_addr={value}")).is_ok());
        }
        assert!(
            convert("[common]\nauthenticate_new_work_conns=true", true).unwrap()["auth"]
                ["additionalScopes"]
                .as_array()
                .unwrap()
                .contains(&json!("NewWorkConns"))
        );
        assert!(convert("[common]\nallow_ports=1", true).is_err());
        assert!(convert("[common]\ndashboard_tls_mode=true", true).is_err());
        assert!(convert("[common]\nlog_way=file", true).is_ok());
        assert!(convert("[common]\ndashboard_tls_mode=false", false).is_ok());
        assert_eq!(scalar("a,,b,", 'a').unwrap(), json!(["a", "b"]));
        let sections = (0..MAX_SECTIONS)
            .map(|n| format!("[s{n}]\n"))
            .collect::<String>();
        assert_eq!(
            parse(&(sections + "[s0]\nkey=value")).unwrap().len(),
            MAX_SECTIONS
        );
        let converted = convert(
            "[common]\n[range:ports]\ntype=tcp\nlocal_port=1-2\nremote_port=3-4",
            true,
        )
        .unwrap();
        assert_eq!(converted["proxies"][0]["type"], "tcp");
        let mut root = json!({"map":{"a":"old"}});
        put_map(&mut root, "map", "b.c", json!("new"));
        assert_eq!(root["map"]["b.c"], "new");
    }
}
