//! 兼容性报告：把「frp 里有、RustFRP 当前不支持或有意忽略」的配置逐条列出，绝不静默吞掉。

use super::types::*;

/// 问题的性质。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// 暂未实现，配置的这部分不会生效。
    Unsupported,
    /// 有意忽略：RustFRP 的实现方式不需要它，行为等价。
    Ignored,
    /// 非严格模式下出现的未知字段（严格模式下是错误）。
    UnknownField,
}

/// 一条兼容性问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// 性质。
    pub severity: Severity,
    /// 字段路径，如 `transport.protocol`。
    pub path: String,
    /// 说明与替代方案。
    pub message: String,
}

/// 兼容性报告。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompatReport {
    /// 全部问题。
    pub issues: Vec<Issue>,
}

impl CompatReport {
    pub(crate) fn add(
        &mut self,
        severity: Severity,
        path: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.issues.push(Issue {
            severity,
            path: path.into(),
            message: message.into(),
        });
    }

    /// 是否存在「不会生效」的配置。
    pub fn has_unsupported(&self) -> bool {
        self.issues
            .iter()
            .any(|i| i.severity == Severity::Unsupported)
    }

    /// 取某类问题。
    pub fn of(&self, severity: Severity) -> impl Iterator<Item = &Issue> {
        self.issues.iter().filter(move |i| i.severity == severity)
    }
}

const WIRE_NOTE: &str =
    "RustFRP uses its own wire protocol; this frp-internal option has no effect";

/// 检查（已补全默认值的）客户端配置，并检查全部代理 / 访问者。
pub(crate) fn check_client(
    c: &ClientConfig,
    proxies: &[Proxy],
    visitors: &[Visitor],
    r: &mut CompatReport,
) {
    let proto = c.transport.protocol.as_str();
    if proto != "tcp" {
        r.add(
            Severity::Unsupported,
            "transport.protocol",
            format!("\"{proto}\" transport is not implemented yet; only \"tcp\" is available"),
        );
    }
    if c.transport.wire_protocol == "v2" {
        r.add(Severity::Ignored, "transport.wireProtocol", WIRE_NOTE);
    }
    if c.transport.pool_count > 1 {
        r.add(
            Severity::Ignored,
            "transport.poolCount",
            "streams are opened on demand; no connection pool is needed",
        );
    }
    if c.transport.tcp_mux == Some(false) {
        r.add(
            Severity::Ignored,
            "transport.tcpMux",
            "RustFRP always multiplexes streams over one connection",
        );
    }
    if !c.virtual_net.address.is_empty() {
        r.add(
            Severity::Unsupported,
            "virtualNet",
            "virtual network (alpha in frp) is not implemented",
        );
    }
    if !c.store.path.is_empty() {
        r.add(
            Severity::Unsupported,
            "store.path",
            "frp's built-in store is not implemented; use the RustFRP panel instead",
        );
    }
    if c.web_server.pprof_enable {
        r.add(
            Severity::Ignored,
            "webServer.pprofEnable",
            "pprof is a Go runtime feature and is not available",
        );
    }
    for (i, p) in proxies.iter().enumerate() {
        if p.kind == "xtcp" {
            r.add(
                Severity::Unsupported,
                format!("proxies[{i}]"),
                "xtcp (P2P hole punching) is planned for V1.1",
            );
        }
        if p.plugin.as_ref().is_some_and(|x| x.kind == "virtual_net") {
            r.add(
                Severity::Unsupported,
                format!("proxies[{i}].plugin"),
                "virtual_net plugin is not implemented",
            );
        }
    }
    for (i, v) in visitors.iter().enumerate() {
        if v.kind == "xtcp" {
            r.add(
                Severity::Unsupported,
                format!("visitors[{i}]"),
                "xtcp (P2P hole punching) is planned for V1.1",
            );
        }
        if v.plugin.as_ref().is_some_and(|x| x.kind == "virtual_net") {
            r.add(
                Severity::Unsupported,
                format!("visitors[{i}].plugin"),
                "virtual_net plugin is not implemented",
            );
        }
    }
}

/// 检查（已补全默认值的）服务端配置。
pub(crate) fn check_server(s: &ServerConfig, r: &mut CompatReport) {
    if s.kcp_bind_port > 0 {
        r.add(
            Severity::Unsupported,
            "kcpBindPort",
            "KCP transport is not implemented",
        );
    }
    if s.quic_bind_port > 0 {
        r.add(
            Severity::Unsupported,
            "quicBindPort",
            "QUIC transport is planned for V1.1",
        );
    }
    if s.ssh_tunnel_gateway.bind_port > 0 {
        r.add(
            Severity::Unsupported,
            "sshTunnelGateway",
            "SSH tunnel gateway is not implemented",
        );
    }
    if !s.http_plugins.is_empty() {
        r.add(
            Severity::Unsupported,
            "httpPlugins",
            "server HTTP plugins are not implemented",
        );
    }
    if s.tcpmux_http_connect_port > 0 {
        r.add(
            Severity::Unsupported,
            "tcpmuxHTTPConnectPort",
            "tcpmux is planned for a later phase",
        );
    }
    if s.enable_prometheus {
        r.add(
            Severity::Unsupported,
            "enablePrometheus",
            "Prometheus metrics are an optional feature planned for a later phase",
        );
    }
    if s.transport.tcp_mux == Some(false) {
        r.add(
            Severity::Ignored,
            "transport.tcpMux",
            "RustFRP always multiplexes streams over one connection",
        );
    }
    if s.transport.max_pool_count != 5 {
        r.add(
            Severity::Ignored,
            "transport.maxPoolCount",
            "streams are opened on demand; no connection pool is needed",
        );
    }
    if s.web_server.pprof_enable {
        r.add(
            Severity::Ignored,
            "webServer.pprofEnable",
            "pprof is a Go runtime feature and is not available",
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    fn client(f: impl FnOnce(&mut ClientConfig)) -> CompatReport {
        let mut c = ClientConfig::default();
        c.complete(&crate::frpcfg::env::MapEnv::default());
        f(&mut c);
        let mut r = CompatReport::default();
        check_client(&c, &[], &[], &mut r);
        r
    }

    fn paths(r: &CompatReport, s: Severity) -> Vec<String> {
        r.of(s).map(|i| i.path.clone()).collect()
    }

    #[test]
    fn clean_client_has_no_issues() {
        let r = client(|_| {});
        assert!(r.issues.is_empty() && !r.has_unsupported());
    }

    #[test]
    fn client_issues() {
        let r = client(|c| {
            c.transport.protocol = "kcp".into();
            c.transport.wire_protocol = "v2".into();
            c.transport.pool_count = 5;
            c.transport.tcp_mux = Some(false);
            c.virtual_net.address = "100.64.0.1/24".into();
            c.store.path = "./s".into();
            c.web_server.pprof_enable = true;
        });
        assert_eq!(
            paths(&r, Severity::Unsupported),
            vec!["transport.protocol", "virtualNet", "store.path"]
        );
        assert_eq!(
            paths(&r, Severity::Ignored),
            vec![
                "transport.wireProtocol",
                "transport.poolCount",
                "transport.tcpMux",
                "webServer.pprofEnable"
            ]
        );
        assert!(r.has_unsupported());
        assert_eq!(r.of(Severity::UnknownField).count(), 0);
    }

    #[test]
    fn proxy_and_visitor_issues() {
        let mut r = CompatReport::default();
        let mut c = ClientConfig::default();
        c.complete(&crate::frpcfg::env::MapEnv::default());
        let vn = Some(Plugin {
            kind: "virtual_net".into(),
            ..Default::default()
        });
        let proxies = vec![
            Proxy {
                kind: "xtcp".into(),
                ..Default::default()
            },
            Proxy {
                kind: "tcp".into(),
                plugin: vn.clone(),
                ..Default::default()
            },
            Proxy {
                kind: "tcp".into(),
                plugin: Some(Plugin {
                    kind: "socks5".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ];
        let visitors = vec![
            Visitor {
                kind: "xtcp".into(),
                ..Default::default()
            },
            Visitor {
                kind: "stcp".into(),
                plugin: vn,
                ..Default::default()
            },
            Visitor {
                kind: "stcp".into(),
                ..Default::default()
            },
        ];
        check_client(&c, &proxies, &visitors, &mut r);
        assert_eq!(
            paths(&r, Severity::Unsupported),
            vec![
                "proxies[0]",
                "proxies[1].plugin",
                "visitors[0]",
                "visitors[1].plugin"
            ]
        );
    }

    #[test]
    fn server_issues() {
        let mut s = ServerConfig::default();
        s.complete();
        let mut r = CompatReport::default();
        check_server(&s, &mut r);
        assert!(r.issues.is_empty());

        s.kcp_bind_port = 1;
        s.quic_bind_port = 2;
        s.ssh_tunnel_gateway.bind_port = 2200;
        s.http_plugins.push(Default::default());
        s.tcpmux_http_connect_port = 3;
        s.enable_prometheus = true;
        s.transport.tcp_mux = Some(false);
        s.transport.max_pool_count = 9;
        s.web_server.pprof_enable = true;
        check_server(&s, &mut r);
        assert_eq!(
            paths(&r, Severity::Unsupported),
            vec![
                "kcpBindPort",
                "quicBindPort",
                "sshTunnelGateway",
                "httpPlugins",
                "tcpmuxHTTPConnectPort",
                "enablePrometheus"
            ]
        );
        assert_eq!(
            paths(&r, Severity::Ignored),
            vec![
                "transport.tcpMux",
                "transport.maxPoolCount",
                "webServer.pprofEnable"
            ]
        );
    }
}
