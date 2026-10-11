//! 严格模式：复刻 frp 对「某类型代理只允许某些字段」的约束。
//!
//! frp 为每种代理 / 插件类型定义独立结构，严格模式下出现别的类型的字段即报未知字段。
//! 我们用超集结构承载，再用本模块的白名单表还原这条规则。

use serde_json::{Map, Value};

use super::error::ConfigError;

const PROXY_BASE: &[&str] = &[
    "name",
    "type",
    "enabled",
    "annotations",
    "transport",
    "metadatas",
    "loadBalancer",
    "healthCheck",
    "localIP",
    "localPort",
    "plugin",
];
const HTTP_EXTRA: &[&str] = &[
    "customDomains",
    "subdomain",
    "locations",
    "httpUser",
    "httpPassword",
    "hostHeaderRewrite",
    "requestHeaders",
    "responseHeaders",
    "routeByHTTPUser",
];
const HTTPS_EXTRA: &[&str] = &["customDomains", "subdomain"];
const TCPMUX_EXTRA: &[&str] = &[
    "customDomains",
    "subdomain",
    "httpUser",
    "httpPassword",
    "routeByHTTPUser",
    "multiplexer",
];
const SECRET_EXTRA: &[&str] = &["secretKey", "allowUsers"];
const XTCP_EXTRA: &[&str] = &["secretKey", "allowUsers", "natTraversal"];
const PORT_EXTRA: &[&str] = &["remotePort"];

const VISITOR_BASE: &[&str] = &[
    "name",
    "type",
    "enabled",
    "transport",
    "secretKey",
    "serverUser",
    "serverName",
    "bindAddr",
    "bindPort",
    "plugin",
];
const XTCP_VISITOR_EXTRA: &[&str] = &[
    "protocol",
    "keepTunnelOpen",
    "maxRetriesAnHour",
    "minRetryInterval",
    "fallbackTo",
    "fallbackTimeoutMs",
    "natTraversal",
];

/// 已知的代理类型。
pub const PROXY_TYPES: &[&str] = &[
    "tcp", "udp", "http", "https", "tcpmux", "stcp", "xtcp", "sudp",
];
/// 已知的访问者类型。
pub const VISITOR_TYPES: &[&str] = &["stcp", "xtcp", "sudp"];

fn proxy_extra(kind: &str) -> &'static [&'static str] {
    match kind {
        "tcp" | "udp" => PORT_EXTRA,
        "http" => HTTP_EXTRA,
        "https" => HTTPS_EXTRA,
        "tcpmux" => TCPMUX_EXTRA,
        "xtcp" => XTCP_EXTRA,
        // stcp / sudp
        _ => SECRET_EXTRA,
    }
}

fn plugin_keys(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "http2https" | "http2http" => &["type", "localAddr", "hostHeaderRewrite", "requestHeaders"],
        "http_proxy" => &["type", "httpUser", "httpPassword"],
        "https2http" | "https2https" => &[
            "type",
            "localAddr",
            "hostHeaderRewrite",
            "requestHeaders",
            "enableHTTP2",
            "crtPath",
            "keyPath",
        ],
        "socks5" => &["type", "username", "password"],
        "static_file" => &[
            "type",
            "localPath",
            "stripPrefix",
            "httpUser",
            "httpPassword",
        ],
        "unix_domain_socket" => &["type", "unixPath"],
        "tls2raw" => &["type", "localAddr", "crtPath", "keyPath"],
        "virtual_net" => &["type"],
        _ => return None,
    })
}

fn visitor_plugin_keys(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "virtual_net" => Some(&["type", "destinationIP"]),
        _ => None,
    }
}

fn str_field<'a>(obj: &'a Map<String, Value>, key: &str) -> &'a str {
    obj.get(key).and_then(Value::as_str).unwrap_or("")
}

fn extra_keys(obj: &Map<String, Value>, allowed: &[&[&str]], prefix: &str, out: &mut Vec<String>) {
    for key in obj.keys() {
        if !allowed.iter().any(|set| set.contains(&key.as_str())) {
            out.push(format!("{prefix}.{key}"));
        }
    }
}

/// 检查 `plugin` 对象：type 必填且已知，字段须属于该插件类型。
fn check_plugin(
    plugin: &Value,
    table: fn(&str) -> Option<&'static [&'static str]>,
    empty_msg: &str,
    unknown_msg: &str,
    prefix: &str,
    unknown: &mut Vec<String>,
) -> Result<(), String> {
    let Some(obj) = plugin.as_object() else {
        // null 视为未设置，其余类型交给后续的类型化解码报错。
        return Ok(());
    };
    let kind = str_field(obj, "type");
    if kind.is_empty() {
        return Err(empty_msg.to_string());
    }
    let Some(keys) = table(kind) else {
        return Err(format!("{unknown_msg}: {kind}"));
    };
    extra_keys(obj, &[keys], prefix, unknown);
    Ok(())
}

/// 预检客户端配置中的 `proxies` / `visitors`：类型必填且已知，并收集类型不允许的字段。
pub(crate) fn precheck_client(root: &Value, unknown: &mut Vec<String>) -> Result<(), ConfigError> {
    let Some(obj) = root.as_object() else {
        return Ok(());
    };
    if let Some(Value::Array(items)) = obj.get("proxies") {
        for (index, item) in items.iter().enumerate() {
            precheck_proxy(index, item, unknown)?;
        }
    }
    if let Some(Value::Array(items)) = obj.get("visitors") {
        for (index, item) in items.iter().enumerate() {
            precheck_visitor(index, item, unknown)?;
        }
    }
    Ok(())
}

fn precheck_proxy(
    index: usize,
    item: &Value,
    unknown: &mut Vec<String>,
) -> Result<(), ConfigError> {
    let err = |message: String| ConfigError::Proxy { index, message };
    let Some(obj) = item.as_object() else {
        return Err(err("type is required".to_string()));
    };
    let kind = str_field(obj, "type");
    if !PROXY_TYPES.contains(&kind) {
        return Err(err(format!("unknown proxy type: {kind}")));
    }
    let prefix = format!("proxies[{index}]");
    extra_keys(obj, &[PROXY_BASE, proxy_extra(kind)], &prefix, unknown);
    if let Some(plugin) = obj.get("plugin") {
        check_plugin(
            plugin,
            plugin_keys,
            "plugin type is empty",
            "unknown plugin type",
            &format!("{prefix}.plugin"),
            unknown,
        )
        .map_err(|m| err(format!("unmarshal proxy plugin error: {m}")))?;
    }
    Ok(())
}

fn precheck_visitor(
    index: usize,
    item: &Value,
    unknown: &mut Vec<String>,
) -> Result<(), ConfigError> {
    let err = |message: String| ConfigError::Visitor { index, message };
    let Some(obj) = item.as_object() else {
        return Err(err("type is required".to_string()));
    };
    let kind = str_field(obj, "type");
    if !VISITOR_TYPES.contains(&kind) {
        return Err(err(format!("unknown visitor type: {kind}")));
    }
    let prefix = format!("visitors[{index}]");
    let extra: &[&str] = if kind == "xtcp" {
        XTCP_VISITOR_EXTRA
    } else {
        &[]
    };
    extra_keys(obj, &[VISITOR_BASE, extra], &prefix, unknown);
    if let Some(plugin) = obj.get("plugin") {
        check_plugin(
            plugin,
            visitor_plugin_keys,
            "visitor plugin type is empty",
            "unknown visitor plugin type",
            &format!("{prefix}.plugin"),
            unknown,
        )
        .map_err(|m| err(format!("unmarshal visitor plugin error: {m}")))?;
    }
    Ok(())
}

/// 把 serde_ignored 的路径（`proxies.0.foo`）转成 `proxies[0].foo`。
pub(crate) fn pretty_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 4);
    for (i, seg) in path.split('.').enumerate() {
        if !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()) {
            out.push('[');
            out.push_str(seg);
            out.push(']');
        } else {
            if i > 0 {
                out.push('.');
            }
            out.push_str(seg);
        }
    }
    out
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(v: Value) -> (Result<(), ConfigError>, Vec<String>) {
        let mut unknown = Vec::new();
        let r = precheck_client(&v, &mut unknown);
        (r, unknown)
    }

    #[test]
    fn valid_proxies_pass() {
        let (r, u) = run(json!({"proxies":[
            {"name":"a","type":"tcp","remotePort":1,"localPort":2},
            {"name":"b","type":"http","customDomains":["x"]},
            {"name":"c","type":"https","subdomain":"s"},
            {"name":"d","type":"tcpmux","multiplexer":"httpconnect"},
            {"name":"e","type":"stcp","secretKey":"k"},
            {"name":"f","type":"xtcp","natTraversal":{}},
            {"name":"g","type":"sudp","allowUsers":["*"]},
            {"name":"h","type":"udp","plugin":{"type":"socks5","username":"u"}}
        ]}));
        assert!(r.is_ok());
        assert!(u.is_empty(), "{u:?}");
    }

    #[test]
    fn field_of_other_type_is_unknown() {
        let (r, u) = run(json!({"proxies":[
            {"name":"a","type":"tcp","customDomains":["x"]},
            {"name":"b","type":"http","remotePort":1},
            {"name":"c","type":"https","locations":["/"]},
            {"name":"d","type":"stcp","remotePort":1},
            {"name":"e","type":"sudp","natTraversal":{}}
        ]}));
        assert!(r.is_ok());
        assert_eq!(
            u,
            vec![
                "proxies[0].customDomains",
                "proxies[1].remotePort",
                "proxies[2].locations",
                "proxies[3].remotePort",
                "proxies[4].natTraversal"
            ]
        );
    }

    #[test]
    fn proxy_type_errors() {
        let (r, _) = run(json!({"proxies":[{"name":"a"}]}));
        assert_eq!(
            r.unwrap_err().to_string(),
            "decode proxy at index 0: unknown proxy type: "
        );
        let (r, _) = run(json!({"proxies":[{"name":"a","type":"ftp"}]}));
        assert_eq!(
            r.unwrap_err().to_string(),
            "decode proxy at index 0: unknown proxy type: ftp"
        );
        let (r, _) = run(json!({"proxies":[1]}));
        assert_eq!(
            r.unwrap_err().to_string(),
            "decode proxy at index 0: type is required"
        );
    }

    #[test]
    fn plugin_checks() {
        let (r, u) = run(
            json!({"proxies":[{"name":"a","type":"tcp","plugin":{"type":"socks5","localAddr":"x"}}]}),
        );
        assert!(r.is_ok());
        assert_eq!(u, vec!["proxies[0].plugin.localAddr"]);
        let (r, _) = run(json!({"proxies":[{"name":"a","type":"tcp","plugin":{"localAddr":"x"}}]}));
        assert!(r.unwrap_err().to_string().contains("plugin type is empty"));
        let (r, _) = run(json!({"proxies":[{"name":"a","type":"tcp","plugin":{"type":"zzz"}}]}));
        assert!(r
            .unwrap_err()
            .to_string()
            .contains("unknown plugin type: zzz"));
        // null 与非对象的 plugin 交给类型化解码处理。
        let (r, u) = run(
            json!({"proxies":[{"name":"a","type":"tcp","plugin":null},{"name":"b","type":"tcp","plugin":3}]}),
        );
        assert!(r.is_ok() && u.is_empty());
    }

    #[test]
    fn every_plugin_type_is_known() {
        for k in [
            "http2https",
            "http_proxy",
            "https2http",
            "https2https",
            "http2http",
            "socks5",
            "static_file",
            "unix_domain_socket",
            "tls2raw",
            "virtual_net",
        ] {
            assert!(plugin_keys(k).is_some(), "{k}");
        }
        assert!(plugin_keys("nope").is_none());
    }

    #[test]
    fn visitors() {
        let (r, u) = run(json!({"visitors":[
            {"name":"a","type":"stcp","serverName":"s","bindPort":1},
            {"name":"b","type":"xtcp","protocol":"quic","keepTunnelOpen":true},
            {"name":"c","type":"sudp","protocol":"quic"},
            {"name":"d","type":"stcp","plugin":{"type":"virtual_net","destinationIP":"1.1.1.1"}}
        ]}));
        assert!(r.is_ok());
        assert_eq!(u, vec!["visitors[2].protocol"]);

        let (r, _) = run(json!({"visitors":[{"name":"a","type":"tcp"}]}));
        assert_eq!(
            r.unwrap_err().to_string(),
            "decode visitor at index 0: unknown visitor type: tcp"
        );
        let (r, _) = run(json!({"visitors":[null]}));
        assert_eq!(
            r.unwrap_err().to_string(),
            "decode visitor at index 0: type is required"
        );
        let (r, _) = run(json!({"visitors":[{"name":"a","type":"stcp","plugin":{}}]}));
        assert!(r
            .unwrap_err()
            .to_string()
            .contains("visitor plugin type is empty"));
        let (r, _) =
            run(json!({"visitors":[{"name":"a","type":"stcp","plugin":{"type":"socks5"}}]}));
        assert!(r
            .unwrap_err()
            .to_string()
            .contains("unknown visitor plugin type: socks5"));
    }

    #[test]
    fn non_object_root_and_missing_arrays_are_ignored() {
        assert!(run(json!([1, 2])).0.is_ok());
        assert!(run(json!({"proxies": 5, "visitors": "x"})).0.is_ok());
        assert!(run(json!({})).0.is_ok());
    }

    #[test]
    fn pretty_paths() {
        assert_eq!(pretty_path("proxies.0.foo"), "proxies[0].foo");
        assert_eq!(pretty_path("a.b"), "a.b");
        assert_eq!(pretty_path("x"), "x");
        assert_eq!(pretty_path("transport.tls.1.z"), "transport.tls[1].z");
    }

    #[test]
    fn pretty_path_keeps_numeric_looking_names_only_when_digits() {
        assert_eq!(pretty_path("a.1b.2"), "a.1b[2]");
        assert_eq!(pretty_path(".x"), ".x");
    }
}
