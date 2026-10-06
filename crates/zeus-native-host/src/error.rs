//! Host errors and their mapping to the `nm.*` codes of spec 02 section 14.

use thiserror::Error;
use zeus_types::{ErrorEnvelope, codes};

/// Failures of the native host. Messages never contain frame bodies or secrets.
#[derive(Debug, Error)]
pub enum HostError {
    /// The launch arguments do not identify an extension.
    #[error("cannot determine which extension launched the host: {0}")]
    Launch(&'static str),
    /// The launching extension is not in the host's allow-list.
    #[error("the launching extension is not allowed")]
    OriginNotAllowed,
    /// A frame announced a zero length.
    #[error("zero-length frame")]
    ZeroLength,
    /// A frame announced a length above the limit for its direction.
    #[error("frame of {length} bytes exceeds the {limit} byte limit")]
    FrameTooLarge {
        /// Announced length.
        length: u64,
        /// The limit.
        limit: usize,
    },
    /// The payload is not valid UTF-8 JSON.
    #[error("frame payload is not valid JSON")]
    InvalidJson,
    /// The stream ended inside a frame.
    #[error("stream ended inside a frame")]
    Truncated,
    /// I/O failure.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// Not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// Installation failure.
    #[error("install: {0}")]
    Install(String),
}

impl HostError {
    /// The envelope to report to the daemon or log.
    pub fn to_envelope(&self) -> ErrorEnvelope {
        let code = match self {
            Self::Launch(_) | Self::OriginNotAllowed => codes::NM_ORIGIN_NOT_ALLOWED,
            Self::ZeroLength => codes::NM_MALFORMED_FRAME,
            Self::FrameTooLarge { .. } => codes::NM_FRAME_TOO_LARGE,
            Self::InvalidJson => codes::NM_INVALID_JSON,
            Self::Truncated => codes::NM_MALFORMED_FRAME,
            Self::Io(_) | Self::Install(_) => codes::NM_INTERNAL,
            Self::NotImplemented(_) => codes::NM_INTERNAL,
        };
        ErrorEnvelope::from_code(code, self.to_string())
    }
}
