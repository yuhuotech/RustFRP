//! RustFRP Controller：管理 API、SQLite 存储与内置 Edge。
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

pub mod api;
pub mod app;
pub mod auth;
pub mod cli;
pub mod config;
pub mod db;
pub mod hub;
pub mod maintenance;
pub mod models;
pub mod nodes;
pub mod panel;
pub mod process;
pub mod store;

pub mod traffic;

pub mod api_tokens;

pub mod config_transfer;
