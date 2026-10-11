//! 默认值补全（对应 frp 各配置的 `Complete()`）。语义与 frp 一致：零值 / 缺省 = 使用默认值。

use super::env::Env;
use super::types::*;

fn or_str(s: &mut String, default: &str) {
    if s.is_empty() {
        *s = default.to_string();
    }
}
fn or_i64(v: &mut i64, default: i64) {
    if *v == 0 {
        *v = default;
    }
}
fn or_bool(v: &mut Option<bool>, default: bool) {
    if v.is_none() {
        *v = Some(default);
    }
}

impl LogConfig {
    pub(crate) fn complete(&mut self) {
        or_str(&mut self.to, "console");
        or_str(&mut self.level, "info");
        or_i64(&mut self.max_days, 3);
    }
}

impl QuicOptions {
    pub(crate) fn complete(&mut self) {
        or_i64(&mut self.keepalive_period, 10);
        or_i64(&mut self.max_idle_timeout, 30);
        or_i64(&mut self.max_incoming_streams, 100_000);
    }
}

impl ClientTransport {
    pub(crate) fn complete(&mut self, env: &dyn Env) {
        or_str(&mut self.protocol, "tcp");
        or_str(&mut self.wire_protocol, "v1");
        or_i64(&mut self.dial_server_timeout, 10);
        or_i64(&mut self.dial_server_keepalive, 7200);
        if self.proxy_url.is_empty() {
            self.proxy_url = env.get("http_proxy").unwrap_or_default();
        }
        or_i64(&mut self.pool_count, 1);
        or_bool(&mut self.tcp_mux, true);
        or_i64(&mut self.tcp_mux_keepalive_interval, 30);
        if self.tcp_mux == Some(true) {
            // 开启多路复用后应用层心跳多余，由 mux 自身保活。
            or_i64(&mut self.heartbeat_interval, -1);
            or_i64(&mut self.heartbeat_timeout, -1);
        } else {
            or_i64(&mut self.heartbeat_interval, 30);
            or_i64(&mut self.heartbeat_timeout, 90);
        }
        self.quic.complete();
        or_bool(&mut self.tls.enable, true);
        or_bool(&mut self.tls.disable_custom_tls_first_byte, true);
    }
}

impl ServerTransport {
    pub(crate) fn complete(&mut self) {
        or_bool(&mut self.tcp_mux, true);
        or_i64(&mut self.tcp_mux_keepalive_interval, 30);
        or_i64(&mut self.tcp_keepalive, 7200);
        or_i64(&mut self.max_pool_count, 5);
        if self.tcp_mux == Some(true) {
            or_i64(&mut self.heartbeat_timeout, -1);
        } else {
            or_i64(&mut self.heartbeat_timeout, 90);
        }
        self.quic.complete();
        if !self.tls.trusted_ca_file.is_empty() {
            self.tls.force = true;
        }
    }
}

impl ClientConfig {
    /// 补全默认值（`env` 用于读取 `http_proxy`）。
    pub fn complete(&mut self, env: &dyn Env) {
        or_str(&mut self.server_addr, "0.0.0.0");
        or_i64(&mut self.server_port, 7000);
        or_bool(&mut self.login_fail_exit, true);
        or_str(&mut self.nat_hole_stun_server, "stun.easyvoip.com:3478");
        or_str(&mut self.auth.method, "token");
        self.log.complete();
        self.transport.complete(env);
        or_str(&mut self.web_server.addr, "127.0.0.1");
        or_i64(&mut self.udp_packet_size, 1500);
    }
}

impl ServerConfig {
    /// 补全默认值。
    pub fn complete(&mut self) {
        or_str(&mut self.auth.method, "token");
        self.log.complete();
        self.transport.complete();
        or_str(&mut self.web_server.addr, "127.0.0.1");
        or_str(
            &mut self.ssh_tunnel_gateway.auto_gen_private_key_path,
            "./.autogen_ssh_key",
        );
        or_str(&mut self.bind_addr, "0.0.0.0");
        or_i64(&mut self.bind_port, 7000);
        if self.proxy_bind_addr.is_empty() {
            self.proxy_bind_addr = self.bind_addr.clone();
        }
        or_i64(&mut self.vhost_http_timeout, 60);
        or_bool(&mut self.detailed_errors_to_client, true);
        or_i64(&mut self.user_conn_timeout, 10);
        or_i64(&mut self.udp_packet_size, 1500);
        or_i64(&mut self.nathole_analysis_data_reserve_hours, 7 * 24);
    }
}

impl Plugin {
    pub(crate) fn complete(&mut self) {
        if self.kind == "https2http" || self.kind == "https2https" {
            or_bool(&mut self.enable_http2, true);
        }
    }
}

impl Proxy {
    /// 补全默认值。
    pub fn complete(&mut self) {
        or_str(&mut self.local_ip, "127.0.0.1");
        or_str(&mut self.transport.bandwidth_limit_mode, "client");
        if let Some(p) = self.plugin.as_mut() {
            p.complete();
        }
    }

    /// 是否启用（缺省 = 启用）。
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

impl Visitor {
    /// 补全默认值。
    pub fn complete(&mut self) {
        or_str(&mut self.bind_addr, "127.0.0.1");
        if self.kind == "xtcp" {
            or_str(&mut self.protocol, "quic");
            or_i64(&mut self.max_retries_an_hour, 8);
            or_i64(&mut self.min_retry_interval, 90);
            or_i64(&mut self.fallback_timeout_ms, 1000);
        }
    }

    /// 是否启用（缺省 = 启用）。
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use crate::frpcfg::env::MapEnv;

    fn env_with_proxy() -> MapEnv {
        let mut m = std::collections::BTreeMap::new();
        m.insert("http_proxy".to_string(), "http://p:1".to_string());
        MapEnv(m)
    }

    #[test]
    fn client_defaults() {
        let mut c = ClientConfig::default();
        c.complete(&MapEnv::default());
        assert_eq!((c.server_addr.as_str(), c.server_port), ("0.0.0.0", 7000));
        assert_eq!(c.login_fail_exit, Some(true));
        assert_eq!(c.auth.method, "token");
        assert_eq!(
            (c.log.to.as_str(), c.log.level.as_str(), c.log.max_days),
            ("console", "info", 3)
        );
        assert_eq!(c.web_server.addr, "127.0.0.1");
        assert_eq!(c.udp_packet_size, 1500);
        assert_eq!(c.nat_hole_stun_server, "stun.easyvoip.com:3478");
        let t = &c.transport;
        assert_eq!(
            (t.protocol.as_str(), t.wire_protocol.as_str()),
            ("tcp", "v1")
        );
        assert_eq!(
            (t.dial_server_timeout, t.dial_server_keepalive, t.pool_count),
            (10, 7200, 1)
        );
        assert_eq!((t.tcp_mux, t.tcp_mux_keepalive_interval), (Some(true), 30));
        assert_eq!((t.heartbeat_interval, t.heartbeat_timeout), (-1, -1));
        assert_eq!(t.tls.enable, Some(true));
        assert_eq!(t.tls.disable_custom_tls_first_byte, Some(true));
        assert_eq!(
            (
                t.quic.keepalive_period,
                t.quic.max_idle_timeout,
                t.quic.max_incoming_streams
            ),
            (10, 30, 100_000)
        );
        assert!(t.proxy_url.is_empty());
    }

    #[test]
    fn client_explicit_values_are_kept() {
        let mut c = ClientConfig {
            server_port: 9,
            login_fail_exit: Some(false),
            ..Default::default()
        };
        c.transport.tcp_mux = Some(false);
        c.transport.proxy_url = "socks5://x".into();
        c.complete(&env_with_proxy());
        assert_eq!((c.server_port, c.login_fail_exit), (9, Some(false)));
        assert_eq!(
            (
                c.transport.heartbeat_interval,
                c.transport.heartbeat_timeout
            ),
            (30, 90)
        );
        assert_eq!(c.transport.proxy_url, "socks5://x");
    }

    #[test]
    fn proxy_url_from_environment() {
        let mut c = ClientConfig::default();
        c.complete(&env_with_proxy());
        assert_eq!(c.transport.proxy_url, "http://p:1");
    }

    #[test]
    fn server_defaults() {
        let mut s = ServerConfig::default();
        s.complete();
        assert_eq!(
            (
                s.bind_addr.as_str(),
                s.bind_port,
                s.proxy_bind_addr.as_str()
            ),
            ("0.0.0.0", 7000, "0.0.0.0")
        );
        assert_eq!(
            (s.vhost_http_timeout, s.user_conn_timeout, s.udp_packet_size),
            (60, 10, 1500)
        );
        assert_eq!(s.nathole_analysis_data_reserve_hours, 168);
        assert_eq!(s.detailed_errors_to_client, Some(true));
        assert_eq!(
            s.ssh_tunnel_gateway.auto_gen_private_key_path,
            "./.autogen_ssh_key"
        );
        assert_eq!(s.web_server.addr, "127.0.0.1");
        let t = &s.transport;
        assert_eq!(
            (
                t.tcp_mux,
                t.tcp_keepalive,
                t.max_pool_count,
                t.heartbeat_timeout
            ),
            (Some(true), 7200, 5, -1)
        );
        assert!(!t.tls.force);
    }

    #[test]
    fn server_explicit_values() {
        let mut s = ServerConfig {
            bind_addr: "1.2.3.4".into(),
            proxy_bind_addr: "5.6.7.8".into(),
            ..Default::default()
        };
        s.transport.tcp_mux = Some(false);
        s.transport.tls.trusted_ca_file = "ca.pem".into();
        s.complete();
        assert_eq!(s.proxy_bind_addr, "5.6.7.8");
        assert_eq!(s.transport.heartbeat_timeout, 90);
        assert!(s.transport.tls.force);
    }

    #[test]
    fn proxy_defaults_and_enabled() {
        let mut p = Proxy {
            plugin: Some(Plugin {
                kind: "https2http".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        p.complete();
        assert_eq!(p.local_ip, "127.0.0.1");
        assert_eq!(p.transport.bandwidth_limit_mode, "client");
        assert_eq!(p.plugin.as_ref().unwrap().enable_http2, Some(true));
        assert!(p.is_enabled());
        p.enabled = Some(false);
        assert!(!p.is_enabled());
        let mut q = Proxy {
            local_ip: "10.0.0.1".into(),
            ..Default::default()
        };
        q.complete();
        assert_eq!(q.local_ip, "10.0.0.1");
        assert!(q.plugin.is_none());
    }

    #[test]
    fn plugin_default_only_for_https_plugins() {
        for (kind, expect) in [
            ("https2https", Some(true)),
            ("socks5", None),
            ("http2https", None),
        ] {
            let mut p = Plugin {
                kind: kind.into(),
                ..Default::default()
            };
            p.complete();
            assert_eq!(p.enable_http2, expect, "{kind}");
        }
        let mut p = Plugin {
            kind: "https2http".into(),
            enable_http2: Some(false),
            ..Default::default()
        };
        p.complete();
        assert_eq!(p.enable_http2, Some(false));
    }

    #[test]
    fn visitor_defaults() {
        let mut v = Visitor {
            kind: "xtcp".into(),
            ..Default::default()
        };
        v.complete();
        assert_eq!(v.bind_addr, "127.0.0.1");
        assert_eq!(
            (
                v.protocol.as_str(),
                v.max_retries_an_hour,
                v.min_retry_interval,
                v.fallback_timeout_ms
            ),
            ("quic", 8, 90, 1000)
        );
        let mut s = Visitor {
            kind: "stcp".into(),
            ..Default::default()
        };
        s.complete();
        assert!(s.protocol.is_empty() && s.is_enabled());
        s.enabled = Some(false);
        assert!(!s.is_enabled());
    }
}
