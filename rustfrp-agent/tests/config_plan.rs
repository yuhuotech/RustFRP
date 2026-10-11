//! frp 配置计划边界的公共调用路径。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use rustfrp_agent::plan;
use rustfrp_core::frpcfg::{
    load_client, ClientConfig, CompatReport, LoadOptions, LoadedClient, MapEnv, MemFs, Visitor,
};

#[test]
fn disabled_visitors_are_not_added_to_the_runtime_plan() {
    let fs = MemFs::new().with(
        "frpc.toml",
        "serverAddr='127.0.0.1'\n[[visitors]]\nname='disabled'\ntype='stcp'\nenabled=false\nserverName='private'\nsecretKey='k'\nbindPort=0\n",
    );
    let env = MapEnv::default();
    let loaded = load_client(
        "frpc.toml",
        &LoadOptions {
            strict: true,
            fs: &fs,
            env: &env,
        },
    )
    .unwrap();
    let plan = plan::client_plan(&loaded, &fs).unwrap();
    assert!(plan.cfg.visitors.is_empty());
    assert!(plan.notes.iter().all(|note| !note.contains("disabled")));
}

#[test]
fn runtime_plan_defensively_skips_a_disabled_loaded_visitor() {
    let fs = MemFs::new();
    let visitor = Visitor {
        name: "disabled".into(),
        kind: "stcp".into(),
        enabled: Some(false),
        server_name: "private".into(),
        secret_key: "key".into(),
        ..Default::default()
    };
    let loaded = LoadedClient {
        source: ClientConfig::default(),
        common: ClientConfig::default(),
        proxies: Vec::new(),
        visitors: vec![visitor],
        report: CompatReport::default(),
    };
    let plan = plan::client_plan(&loaded, &fs).unwrap();
    assert!(plan.cfg.visitors.is_empty());
}
