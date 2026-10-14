//! Static UI: borrowed embedded assets or bounded file streaming. No renderer
//! or whole-file heap cache is linked into the Controller.

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use std::path::Path;
use std::time::UNIX_EPOCH;
use tokio_util::io::ReaderStream;

pub fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

pub fn relative_path(path: &str) -> Option<String> {
    if path.len() > 2048 {
        return None;
    }
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        return Some("index.html".into());
    }
    rel.split('/')
        .all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && !segment.contains('\\')
                && !segment.contains('\0')
                && !segment.contains('%')
        })
        .then(|| rel.to_string())
}

fn cache(path: &str) -> &'static str {
    if matches!(path, "index.html" | "ui-manifest.json") {
        "no-cache"
    } else if path.rsplit('/').next().is_some_and(|name| {
        name.split('-').next_back().is_some_and(|suffix| {
            suffix.split('.').next().is_some_and(|hash| {
                hash.len() >= 8 && hash.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
        })
    }) {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    }
}

fn unchanged(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|header| header.to_str().ok())
        .is_some_and(|values| {
            values
                .split(',')
                .any(|value| value.trim() == etag || value.trim() == "*")
        })
}

fn response(
    path: &str,
    length: u64,
    encoding: Option<&str>,
    etag: &str,
    incoming: &HeaderMap,
    body: Body,
) -> Response {
    let mut result = if unchanged(incoming, etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        body.into_response()
    };
    let headers = result.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache(path)));
    headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    if !unchanged(incoming, etag) {
        if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
            headers.insert(header::CONTENT_LENGTH, value);
        }
    }
    if let Some(encoding) = encoding {
        if let Ok(value) = HeaderValue::from_str(encoding) {
            headers.insert(header::CONTENT_ENCODING, value);
        }
    }
    result
}

pub async fn serve(
    directory: Option<&Path>,
    path: &str,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    if !matches!(*method, Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if path.starts_with("/api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(mut rel) = relative_path(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let accept = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if let Some(directory) = directory {
        let Ok(root) = tokio::fs::canonicalize(directory).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let mut candidate = tokio::fs::canonicalize(root.join(&rel)).await.ok();
        if candidate.is_none() && !rel.contains('.') {
            rel = "index.html".into();
            candidate = tokio::fs::canonicalize(root.join(&rel)).await.ok();
        }
        let Some(raw) = candidate.filter(|path| path.starts_with(&root)) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let mut selected = raw.clone();
        let mut encoding = None;
        let br = rustfrp_ui_assets::quality(accept, "br");
        let gzip = rustfrp_ui_assets::quality(accept, "gzip");
        let encodings = if br >= gzip {
            [("br", br), ("gzip", gzip)]
        } else {
            [("gzip", gzip), ("br", br)]
        };
        for (kind, quality) in encodings {
            if quality <= 0.0 {
                continue;
            }
            let suffix = if kind == "gzip" { "gz" } else { "br" };
            if let Ok(sidecar) = tokio::fs::canonicalize(root.join(format!("{rel}.{suffix}"))).await
            {
                if sidecar.starts_with(&root) {
                    selected = sidecar;
                    encoding = Some(kind);
                    break;
                }
            }
        }
        if encoding.is_none() && rustfrp_ui_assets::quality(accept, "identity") <= 0.0 {
            return StatusCode::NOT_ACCEPTABLE.into_response();
        }
        let Ok(file) = tokio::fs::File::open(selected).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let Ok(metadata) = file.metadata().await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if !metadata.is_file() {
            return StatusCode::NOT_FOUND.into_response();
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |time| time.as_nanos());
        let etag = format!(
            "W/\"{}-{modified}-{}\"",
            metadata.len(),
            encoding.unwrap_or("identity")
        );
        let body = if *method == Method::HEAD {
            Body::empty()
        } else {
            Body::from_stream(ReaderStream::with_capacity(file, 16 * 1024))
        };
        return response(&rel, metadata.len(), encoding, &etag, headers, body);
    }
    let asset = rustfrp_ui_assets::lookup(&rel).or_else(|| {
        if !rel.contains('.') {
            rustfrp_ui_assets::lookup("index.html")
        } else {
            None
        }
    });
    let Some(asset) = asset else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((bytes, encoding)) = asset.response_bytes(accept) else {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    };
    let etag = format!("\"{}-{}\"", asset.hash, encoding.unwrap_or("identity"));
    let body = if *method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(Bytes::from_static(bytes))
    };
    response(
        asset.path,
        bytes.len() as u64,
        encoding,
        &etag,
        headers,
        body,
    )
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    #[test]
    fn paths_and_cache_do_not_alias_missing_scripts_or_api_resources() {
        assert_eq!(relative_path("/"), Some("index.html".into()));
        for value in [
            "/../secret",
            "/a//b",
            "/a/./b",
            "/a\\b",
            "/%2e%2e/secret",
            "/\0",
        ] {
            assert!(relative_path(value).is_none());
        }
        assert_eq!(
            cache("main-ABCDEFGH.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(cache("logo.svg"), "public, max-age=3600");
        assert_eq!(cache("index.html"), "no-cache");
    }
    #[tokio::test]
    async fn external_files_stream_with_encoding_etags_and_head_semantics() {
        let root =
            std::env::temp_dir().join(format!("rustfrp-panel-{}", rustfrp_core::auth::random_id()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(root.join("index.html"), b"<html>panel</html>")
            .await
            .unwrap();
        tokio::fs::write(root.join("main-ABCDEFGH.js"), b"raw-js")
            .await
            .unwrap();
        tokio::fs::write(root.join("main-ABCDEFGH.js.gz"), b"compressed")
            .await
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        let result = serve(Some(&root), "/main-ABCDEFGH.js", &Method::GET, &headers).await;
        assert_eq!(result.status(), StatusCode::OK);
        assert_eq!(result.headers()[header::CONTENT_ENCODING], "gzip");
        let etag = result.headers()[header::ETAG].clone();
        assert_eq!(
            result.into_body().collect().await.unwrap().to_bytes(),
            &b"compressed"[..]
        );
        headers.insert(header::IF_NONE_MATCH, etag);
        assert_eq!(
            serve(Some(&root), "/main-ABCDEFGH.js", &Method::GET, &headers)
                .await
                .status(),
            StatusCode::NOT_MODIFIED
        );
        headers.clear();
        let head = serve(Some(&root), "/", &Method::HEAD, &headers).await;
        assert_eq!(head.headers()[header::CONTENT_LENGTH], "18");
        assert!(head
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty());
        assert_eq!(
            serve(Some(&root), "/client/details", &Method::GET, &headers)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            serve(Some(&root), "/missing.js", &Method::GET, &headers)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            serve(Some(&root), "/api/missing", &Method::GET, &headers)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            serve(Some(&root), "/", &Method::POST, &headers)
                .await
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
