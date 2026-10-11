#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! 用 frp 官方完整示例配置验证兼容层（语料见 rustfrp-protocol/frp-compat/upstream）。
//! 这些示例是 frp 自己的「全字段」参考配置，应当能在严格模式下加载、通过校验并无损导出。

use rustfrp_core::frpcfg::*;

const CLIENT: &str =
    include_str!("../../rustfrp-protocol/frp-compat/upstream/frpc_full_example.toml");
const SERVER: &str =
    include_str!("../../rustfrp-protocol/frp-compat/upstream/frps_full_example.toml");

fn opts<'a>(fs: &'a MemFs, env: &'a MapEnv) -> LoadOptions<'a> {
    LoadOptions {
        strict: true,
        fs,
        env,
    }
}

fn paths(r: &CompatReport, s: Severity) -> Vec<&str> {
    r.of(s).map(|i| i.path.as_str()).collect()
}

#[test]
fn client_full_example_loads_validates_and_reports() {
    let (fs, env) = (MemFs::new(), MapEnv::default());
    let l = load_client_str(CLIENT, Format::Toml, &opts(&fs, &env)).unwrap();
    assert_eq!((l.proxies.len(), l.visitors.len()), (18, 3));
    assert_eq!(l.common.server_port, 7000);
    assert_eq!(l.common.user, "your_name");

    let warnings = validate::validate_client_all(&l.common, &l.proxies, &l.visitors, &[]).unwrap();
    assert!(warnings.is_empty());

    // 未实现的只有 xtcp 与 virtual_net；poolCount 属于「有意忽略」。
    assert_eq!(
        paths(&l.report, Severity::Unsupported),
        vec![
            "proxies[16]",
            "proxies[17].plugin",
            "visitors[1]",
            "visitors[2].plugin"
        ]
    );
    assert_eq!(
        paths(&l.report, Severity::Ignored),
        vec!["transport.poolCount"]
    );
    assert_eq!(l.report.of(Severity::UnknownField).count(), 0);
}

#[test]
fn server_full_example_loads_validates_and_reports() {
    let (fs, env) = (MemFs::new(), MapEnv::default());
    let l = load_server_str(SERVER, Format::Toml, &opts(&fs, &env)).unwrap();
    validate::validate_server(&l.config, &[]).unwrap();
    assert_eq!(l.config.bind_port, 7000);
    assert_eq!(
        paths(&l.report, Severity::Unsupported),
        vec!["kcpBindPort", "httpPlugins", "enablePrometheus"]
    );
}

#[test]
fn full_examples_survive_export_and_reload_in_every_format() {
    let (fs, env) = (MemFs::new(), MapEnv::default());
    let client = load_client_str(CLIENT, Format::Toml, &opts(&fs, &env)).unwrap();
    let server = load_server_str(SERVER, Format::Toml, &opts(&fs, &env)).unwrap();
    for (fmt, hint) in [
        (ExportFormat::Toml, Format::Toml),
        (ExportFormat::Yaml, Format::Auto),
        (ExportFormat::Json, Format::Auto),
    ] {
        let text = export(&client.source, fmt).unwrap();
        let again = load_client_str(&text, hint, &opts(&fs, &env)).unwrap();
        assert_eq!(again.source, client.source, "client {fmt:?}");

        let text = export(&server.source, fmt).unwrap();
        let again = load_server_str(&text, hint, &opts(&fs, &env)).unwrap();
        assert_eq!(again.source, server.source, "server {fmt:?}");
    }
}

#[test]
fn legacy_official_examples_convert_and_reload_in_strict_mode() {
    let (fs, env) = (MemFs::new(), MapEnv::default());
    let client = ini::convert(
        include_str!("../../rustfrp-protocol/frp-compat/upstream/frpc_legacy_full.ini"),
        true,
    )
    .unwrap();
    let loaded = load_client_str(&client.to_string(), Format::Auto, &opts(&fs, &env)).unwrap();
    validate::validate_client_all(&loaded.common, &loaded.proxies, &loaded.visitors, &[]).unwrap();
    assert_eq!(loaded.common.user, "your_name");
    assert!(loaded
        .proxies
        .iter()
        .any(|p| p.name == "tcp_port_16" && p.local_port == 6028));
    assert_eq!(loaded.visitors.len(), 2);
    let server = ini::convert(
        include_str!("../../rustfrp-protocol/frp-compat/upstream/frps_legacy_full.ini"),
        false,
    )
    .unwrap();
    let loaded = load_server_str(&server.to_string(), Format::Auto, &opts(&fs, &env)).unwrap();
    validate::validate_server(&loaded.config, &[]).unwrap();
    assert_eq!(loaded.source.http_plugins.len(), 2);
    assert_eq!(loaded.source.allow_ports.len(), 4);
}
