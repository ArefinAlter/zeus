//! Reviewed, finite media acquisition. Manifests and segments remain in Rust;
//! the webview receives opaque identities and redacted facts only.
//!
//! This module does not fetch browser credentials, decrypt protected media,
//! record live streams or execute page scripts. Optional stream-copy muxing
//! uses an operator-pinned local FFmpeg helper over verified completed files.

pub mod dash;
pub mod hls;
pub mod http;
pub mod manifest;
mod mux;
mod runner;
pub mod service;
pub mod transfer;

use zeus_app::failure::Failure;

fn invalid(message: &str) -> Failure {
    Failure::new("media.invalid_manifest", message)
}

fn unsupported(message: &str) -> Failure {
    Failure::new("media.unsupported", message)
}
