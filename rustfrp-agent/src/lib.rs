//! RustFRP Agent：同一二进制的两种角色——客户端（frpc 兼容）与边缘节点（frps 兼容）。
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

pub mod admin;
pub mod app;
pub mod cli;
pub mod client;
pub mod edge;
pub mod health;
pub mod local_control;
pub mod log;
pub mod managed;
pub mod plan;
pub mod plugin;
pub mod proxy_rt;
pub mod service;
pub mod visitor;

pub mod metrics;
pub mod node;

pub mod log_ring;

pub mod convert;
