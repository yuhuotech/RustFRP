//! RustFRP 核心库：frp 配置兼容层（[`frpcfg`]）与隧道基础设施（线协议、多路复用、TLS、转发）。
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

pub mod auth;
pub mod error;
pub mod frpcfg;
pub mod mux;
pub mod pipe;
pub mod tls;
pub mod wire;
