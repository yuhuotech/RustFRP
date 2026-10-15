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

pub mod bridge;
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

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn trusted_origins_and_external_urls_reject_credentials_and_remote_authority() {
        for allowed in [
            reqwest::Url::parse("tauri://localhost/index.html").unwrap(),
            reqwest::Url::parse("http://tauri.localhost/index.html").unwrap(),
            reqwest::Url::parse("https://tauri.localhost/index.html").unwrap(),
        ] {
            assert!(trusted_origin(&allowed));
        }
        for denied in [
            reqwest::Url::parse("tauri://example.com/index.html").unwrap(),
            reqwest::Url::parse("http://tauri.localhost:1234/index.html").unwrap(),
            reqwest::Url::parse("https://user:password@tauri.localhost/index.html").unwrap(),
            reqwest::Url::parse("https://:password@tauri.localhost/index.html").unwrap(),
        ] {
            assert!(!trusted_origin(&denied));
        }
        assert_eq!(
            external_url("https://example.com/path").unwrap(),
            "https://example.com/path"
        );
        assert!(external_url("http://example.com/path").is_ok());
        let long = format!("https://example.com/{}", "x".repeat(4096));
        for denied in [
            "file:///etc/passwd",
            "mailto:user@example.com",
            "https://user@example.com",
            "https://:password@example.com",
            "http://",
            long.as_str(),
        ] {
            assert!(external_url(denied).is_err());
        }
    }
}

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
