//! 属性测试：解析器与模板引擎面对任意输入都不得 panic，且导出 → 重载保持不变。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use proptest::prelude::*;
use rustfrp_core::frpcfg::template::{render, Values};
use rustfrp_core::frpcfg::*;
use std::collections::BTreeMap;

fn opts<'a>(fs: &'a MemFs, env: &'a MapEnv) -> LoadOptions<'a> {
    LoadOptions {
        strict: false,
        fs,
        env,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn template_never_panics(src in ".{0,200}") {
        let envs = BTreeMap::new();
        let _ = render(&src, &Values { envs: &envs });
    }

    #[test]
    fn template_with_braces_never_panics(src in r"[{}a-z$.:= \-\n\x22]{0,80}") {
        let envs = BTreeMap::new();
        let _ = render(&src, &Values { envs: &envs });
    }

    #[test]
    fn client_loader_never_panics(src in ".{0,300}") {
        let (fs, env) = (MemFs::new(), MapEnv::default());
        let _ = load_client_str(&src, Format::Auto, &opts(&fs, &env));
        let _ = load_server_str(&src, Format::Auto, &opts(&fs, &env));
    }

    #[test]
    fn export_reload_roundtrip(
        addr in "[a-z0-9.]{1,20}",
        port in 1i64..65535,
        name in "[a-z][a-z0-9-]{0,15}",
        local in 1i64..65535,
        remote in 1i64..65535,
    ) {
        let (fs, env) = (MemFs::new(), MapEnv::default());
        let cfg = ClientConfig {
            server_addr: addr,
            server_port: port,
            proxies: vec![Proxy { name, kind: "tcp".into(), local_port: local, remote_port: remote, ..Default::default() }],
            ..Default::default()
        };
        for (f, hint) in [(ExportFormat::Toml, Format::Toml), (ExportFormat::Yaml, Format::Auto), (ExportFormat::Json, Format::Auto)] {
            let text = export(&cfg, f).unwrap();
            let loaded = load_client_str(&text, hint, &LoadOptions { strict: true, fs: &fs, env: &env }).unwrap();
            prop_assert_eq!(&loaded.source, &cfg);
        }
    }
}
