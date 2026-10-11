//! HTTP 虚拟主机的逐请求能力（集成测试实例）。场景本体见 `scenarios/http_features.rs`，
//! 它同时被库内单元测试包含，使 `proxy_rt` 的两份编译实例都被覆盖。
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

include!("scenarios/http_features.rs");
