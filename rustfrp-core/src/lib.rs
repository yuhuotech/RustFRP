//! RustFRP 核心库：frp 配置兼容层（[`frpcfg`]）及后续的协议与传输。
#![cfg_attr(coverage, feature(coverage_attribute))]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod frpcfg;
