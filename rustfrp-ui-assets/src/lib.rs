//! Shared, optionally embedded UI resources. Responses borrow static bytes.
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

pub struct Asset {
    pub path: &'static str,
    pub bytes: &'static [u8],
    pub gzip: &'static [u8],
    pub brotli: &'static [u8],
    pub hash: &'static str,
}
include!(concat!(env!("OUT_DIR"), "/assets.rs"));

pub fn lookup(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.path == path)
}

/// Explicit encodings override wildcard quality; identity is valid by default.
pub fn quality(header: &str, encoding: &str) -> f32 {
    let mut wildcard = None;
    for item in header.split(',') {
        let mut parts = item.trim().split(';');
        let name = parts.next().unwrap_or("").trim();
        let mut quality = 1.0;
        for part in parts {
            if let Some(value) = part.trim().strip_prefix("q=") {
                quality = value
                    .parse::<f32>()
                    .ok()
                    .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
                    .unwrap_or(0.0);
            }
        }
        if name == encoding {
            return quality;
        }
        if name == "*" {
            wildcard = Some(quality);
        }
    }
    wildcard.unwrap_or(if encoding == "identity" { 1.0 } else { 0.0 })
}

impl Asset {
    pub fn response_bytes(&self, accept: &str) -> Option<(&'static [u8], Option<&'static str>)> {
        let br = quality(accept, "br");
        let gzip = quality(accept, "gzip");
        if br > 0.0 && br >= gzip && !self.brotli.is_empty() {
            return Some((self.brotli, Some("br")));
        }
        if gzip > 0.0 && !self.gzip.is_empty() {
            return Some((self.gzip, Some("gzip")));
        }
        (quality(accept, "identity") > 0.0).then_some((self.bytes, None))
    }
}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    static SAMPLE: Asset = Asset {
        path: "sample.js",
        bytes: b"raw",
        gzip: b"gzip",
        brotli: b"br",
        hash: "sample",
    };
    #[test]
    fn encoding_negotiation_respects_explicit_rejections_and_wildcards() {
        assert_eq!(
            SAMPLE.response_bytes("gzip, br"),
            Some((&b"br"[..], Some("br")))
        );
        assert_eq!(
            SAMPLE.response_bytes("br;q=0,gzip;q=0.8"),
            Some((&b"gzip"[..], Some("gzip")))
        );
        assert_eq!(SAMPLE.response_bytes(""), Some((&b"raw"[..], None)));
        assert_eq!(SAMPLE.response_bytes("*;q=0"), None);
        assert_eq!(quality("*;q=0,identity;q=1", "identity"), 1.0);
        assert_eq!(quality("gzip;q=NaN", "gzip"), 0.0);
        assert_eq!(quality("gzip;q=2", "gzip"), 0.0);
        assert_eq!(quality("br;q=0.2,gzip;q=0.8", "br"), 0.2);
    }
    #[test]
    fn uncompressed_assets_work_without_precompressed_variants() {
        let asset = Asset {
            gzip: &[],
            brotli: &[],
            ..SAMPLE
        };
        assert_eq!(asset.response_bytes("gzip,br"), Some((&b"raw"[..], None)));
        assert!(lookup("missing.js").is_none());
        #[cfg(feature = "embedded")]
        {
            assert!(lookup("index.html").is_some());
            assert!(lookup("ui-manifest.json").is_some());
        }
    }
}
