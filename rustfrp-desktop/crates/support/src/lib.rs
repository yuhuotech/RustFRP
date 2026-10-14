//! Portable desktop services. Native windows do not own Agent or Controller state.
#![forbid(unsafe_code)]
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

pub mod config;
pub mod controller;
pub mod diagnostics;
pub mod install;
pub mod lifecycle;
pub mod migration;
pub mod process;
pub mod release;
pub mod runtime;
pub mod startup;
pub mod updates;
pub mod window;

pub fn trusted_origin(url: &reqwest::Url) -> bool {
    url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && ((url.scheme() == "tauri" && url.host_str() == Some("localhost"))
            || (matches!(url.scheme(), "http" | "https")
                && url.host_str() == Some("tauri.localhost")))
}

pub fn external_url(source: &str) -> Result<String, Error> {
    let url = reqwest::Url::parse(source).map_err(Error::invalid_context("链接无效"))?;
    if source.len() > 4096
        || !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::invalid("仅允许不含凭据的 HTTP / HTTPS 链接"));
    }
    Ok(url.to_string())
}
pub mod local;
pub mod settings;

use serde::{Deserialize, Serialize};

/// A structured failure can cross IPC without exposing headers or credentials.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub uncertain: bool,
}

impl Error {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: 0,
            code: "invalid_request".into(),
            message: message.into(),
            uncertain: false,
        }
    }
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            status: 0,
            code: "local_io".into(),
            message: message.into(),
            uncertain: false,
        }
    }
    /// Discard OS/library diagnostics at the IPC boundary and retain a stable, user-facing context.
    pub fn io_context<E>(message: &'static str) -> impl FnOnce(E) -> Self {
        move |_| Self::io(message)
    }
    pub fn invalid_context<E>(message: &'static str) -> impl FnOnce(E) -> Self {
        move |_| Self::invalid(message)
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn packaged_origins_and_external_urls_keep_native_authority_separate() {
        for url in [
            "tauri://localhost/",
            "http://tauri.localhost/index.html",
            "https://tauri.localhost/",
        ] {
            assert!(trusted_origin(&reqwest::Url::parse(url).unwrap()));
        }
        for url in [
            "https://controller.example.com",
            "http://tauri.localhost:8080/",
            "tauri://elsewhere",
            "file:///etc/passwd",
            "https://user@tauri.localhost/",
        ] {
            assert!(!trusted_origin(&reqwest::Url::parse(url).unwrap()));
        }
        assert_eq!(
            external_url("https://rustfrp.com").unwrap(),
            "https://rustfrp.com/"
        );
        for url in [
            "file:///private/file",
            "javascript:alert(1)",
            "https://user:secret@example.com",
            "invalid",
        ] {
            assert!(external_url(url).is_err());
        }
    }
}
