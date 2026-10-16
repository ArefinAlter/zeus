use serde::Deserialize;
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};
use zeus_api::{ApiError, ApiResult};

/// One authenticated local peer. Keep its id stable when rotating its token.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    /// Stable, non-secret identity used for task ownership.
    pub id: String,
    /// Dedicated 256-bit bearer token, encoded as 64 lowercase hexadecimal characters.
    pub token: String,
}

/// Bridge options. No non-loopback listener is implemented.
#[derive(Clone)]
pub struct Config {
    /// Listener address; only IPv4 loopback is supported.
    pub bind: SocketAddr,
    /// Dedicated peer tokens. These are not daemon credentials.
    pub peers: Vec<Peer>,
    /// Explicit browser origin allowlist, normally empty.
    pub allowed_origins: BTreeSet<String>,
    /// Durable, private ownership and idempotency index.
    pub index_path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenFile {
    peers: Vec<Peer>,
}

impl Config {
    /// Read a dedicated private JSON token file. Tokens are never printed or accepted in URLs.
    pub fn from_token_file(port: u16, file: &Path, index_path: PathBuf) -> ApiResult<Self> {
        zeus_api::transport::secure_private_file(file)
            .map_err(|_| bad("A2A peer-token file could not be made private"))?;
        let metadata = std::fs::symlink_metadata(file)
            .map_err(|_| bad("A2A peer-token file is unavailable"))?;
        if !metadata.is_file() || metadata.len() > 16_384 {
            return Err(bad("A2A peer-token file must be a small regular file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.mode() & 0o077 != 0 {
                return Err(bad("A2A peer-token file must have mode 0600"));
            }
        }
        let bytes = std::fs::read(file).map_err(|_| bad("A2A peer-token file is unavailable"))?;
        let tokens: TokenFile =
            serde_json::from_slice(&bytes).map_err(|_| bad("Invalid A2A peer-token file"))?;
        let config = Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            peers: tokens.peers,
            allowed_origins: BTreeSet::new(),
            index_path,
        };
        config.validate()?;
        Ok(config)
    }

    /// Validate configuration before opening a listener or contacting the daemon.
    pub fn validate(&self) -> ApiResult<()> {
        if self.bind.port() == 0 {
            return Err(bad("A2A port must be between 1 and 65535"));
        }
        if self.bind.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err(bad("A2A binds only to 127.0.0.1"));
        }
        if self.peers.is_empty() || self.peers.len() > 32 {
            return Err(bad("Configure between 1 and 32 A2A peers"));
        }
        let mut ids = BTreeSet::new();
        let mut tokens = BTreeSet::new();
        for peer in &self.peers {
            if peer.id.is_empty()
                || peer.id.len() > 64
                || !peer
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || peer.token.len() != 64
                || !peer
                    .token
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !ids.insert(&peer.id)
                || !tokens.insert(&peer.token)
            {
                return Err(bad(
                    "A2A peer ids and 64-character tokens must be valid and unique",
                ));
            }
        }
        for origin in &self.allowed_origins {
            let url = url::Url::parse(origin).map_err(|_| bad("Invalid A2A origin allowlist"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.origin().ascii_serialization() != *origin
            {
                return Err(bad("A2A origins must be exact HTTP origins"));
            }
        }
        Ok(())
    }

    pub(crate) fn authenticate(&self, bearer: &str) -> Option<String> {
        // blake3 Hash equality uses constant-time comparison for the full 256-bit digest.
        let presented = blake3::hash(bearer.as_bytes());
        let mut identity = None;
        for peer in &self.peers {
            if blake3::hash(peer.token.as_bytes()) == presented {
                identity = Some(peer.id.clone());
            }
        }
        identity
    }
}
pub(crate) fn bad(message: &str) -> ApiError {
    ApiError::invalid_params(message)
}
