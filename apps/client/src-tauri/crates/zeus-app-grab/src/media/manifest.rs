//! Private manifest models and bounded parsing shared by HLS and DASH.

use serde::Serialize;
use sha2::{Digest, Sha256};
use url::Url;
use zeus_app::failure::{Failure, Result};

/// Largest manifest accepted, before parsing.
pub const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
/// Largest finite segment list accepted in one reviewed track.
pub const MAX_SEGMENTS: usize = 4096;
/// Largest representation catalogue accepted.
pub const MAX_FORMATS: usize = 64;
/// Maximum combined expanded resource-address bytes in a parsed catalogue.
pub const MAX_EXPANDED_URL_BYTES: usize = 16 * 1024 * 1024;

/// A byte interval on a media resource, inclusive at its start and exclusive at its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRange {
    /// First byte.
    pub start: u64,
    /// First byte outside the interval.
    pub end: u64,
}

/// One independently retrieved resource. This type intentionally has no serializer:
/// signed query strings must not enter frontend state, logs, or checkpoints.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Private resource URL.
    pub url: Url,
    /// Optional byte interval.
    pub range: Option<MediaRange>,
}

/// One finite track that can be assembled without a codec conversion.
#[derive(Debug, Clone)]
pub struct Track {
    /// Untrusted, bounded descriptive label, never a path.
    pub label: String,
    /// HLS transport stream, fragmented MP4, or one direct file.
    pub container: String,
    /// Optional MIME type from the manifest.
    pub mime_type: Option<String>,
    /// Optional declared bitrate.
    pub bandwidth: Option<u64>,
    /// Optional width.
    pub width: Option<u32>,
    /// Optional height.
    pub height: Option<u32>,
    /// Ordered initialization/media resources.
    pub segments: Vec<Segment>,
    /// Duration of the selected finite presentation, when declared.
    pub duration_ms: Option<u64>,
    /// Accurate limitations visible before the person starts.
    pub notices: Vec<String>,
}

impl Track {
    /// Stable identity of the exact expanded resources and their order. Full URLs
    /// contribute to the digest, but only the digest is persisted.
    pub fn fingerprint(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"zeus-media-track-v1\0");
        hash.update(self.container.as_bytes());
        hash.update([0]);
        for segment in &self.segments {
            let url = segment.url.as_str().as_bytes();
            hash.update((url.len() as u64).to_le_bytes());
            hash.update(url);
            match &segment.range {
                Some(range) => {
                    hash.update([1]);
                    hash.update(range.start.to_le_bytes());
                    hash.update(range.end.to_le_bytes());
                }
                None => hash.update([0]),
            }
        }
        hex::encode(hash.finalize())
    }

    /// A projection containing no resource addresses or signed query strings.
    pub fn view(&self, format_id: String) -> FormatView {
        FormatView {
            format_id,
            label: self.label.clone(),
            container: self.container.clone(),
            mime_type: self.mime_type.clone(),
            bandwidth: self.bandwidth,
            width: self.width,
            height: self.height,
            segment_count: Some(self.segments.len()),
            duration_ms: self.duration_ms,
            notices: self.notices.clone(),
        }
    }
}

/// Safe media format facts shown in the desktop review.
#[derive(Debug, Clone, Serialize)]
pub struct FormatView {
    /// Opaque format identity in the private inspection cache.
    pub format_id: String,
    /// Untrusted descriptive text.
    pub label: String,
    /// Intended output container.
    pub container: String,
    /// Declared media MIME type.
    pub mime_type: Option<String>,
    /// Declared bitrate, not a measured download rate.
    pub bandwidth: Option<u64>,
    /// Declared width.
    pub width: Option<u32>,
    /// Declared height.
    pub height: Option<u32>,
    /// Number of resources; unknown until an HLS variant is selected.
    pub segment_count: Option<usize>,
    /// Declared duration.
    pub duration_ms: Option<u64>,
    /// Limits affecting the resulting track.
    pub notices: Vec<String>,
}

/// Resolve a manifest reference without credentials or unsafe schemes.
pub fn resolve(base: &Url, reference: &str) -> Result<Url> {
    let mut url = base
        .join(reference)
        .map_err(|_| super::invalid("A media resource address is invalid."))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Failure::new(
            "media.invalid_url",
            "Media resources require an HTTP or HTTPS address without an embedded login.",
        ));
    }
    url.set_fragment(None);
    if url.as_str().len() > 16 * 1024 {
        return Err(super::invalid(
            "A media resource address exceeds the length limit.",
        ));
    }
    Ok(url)
}

/// Parse one explicit byte interval (`start-end`, inclusive in the manifest).
pub fn range(value: &str) -> Result<MediaRange> {
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| super::invalid("A media byte range is invalid."))?;
    let start: u64 = start
        .parse()
        .map_err(|_| super::invalid("A media byte range is invalid."))?;
    let end = end
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_add(1))
        .filter(|end| *end > start)
        .ok_or_else(|| super::invalid("A media byte range is invalid."))?;
    Ok(MediaRange { start, end })
}
