//! PKCE (RFC 7636) helper and the OpenRouter authorisation URL builder.
//!
//! The OpenRouter endpoints below follow OpenRouter's documented one-click flow as known on 2026-10-05 and are
//! to be re-verified when spec 09 is written; they are constants so a change is a one-line edit.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use url::Url;
use zeroize::Zeroizing;

use crate::error::LlmError;

/// OpenRouter authorisation page (user is sent here in the browser).
pub const OPENROUTER_AUTH_URL: &str = "https://openrouter.ai/auth";
/// OpenRouter code-for-key exchange endpoint.
pub const OPENROUTER_EXCHANGE_URL: &str = "https://openrouter.ai/api/v1/auth/keys";

/// Allowed length of a code verifier (RFC 7636 section 4.1).
const VERIFIER_LEN: std::ops::RangeInclusive<usize> = 43..=128;

/// A PKCE code verifier and its `S256` challenge. The verifier is secret until the exchange and is zeroized on
/// drop; it has no `Debug` output.
pub struct PkcePair {
    verifier: Zeroizing<String>,
    challenge: String,
}

impl std::fmt::Debug for PkcePair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkcePair")
            .field("verifier", &"[REDACTED]")
            .field("challenge", &self.challenge)
            .finish()
    }
}

impl PkcePair {
    /// Generate a fresh pair from 32 random bytes (a 43-character verifier).
    pub fn generate() -> Self {
        let mut bytes = Zeroizing::new([0u8; 32]);
        rand::fill(&mut bytes[..]);
        let verifier = Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes.as_slice()));
        let challenge = Self::challenge_for(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    /// Build a pair from a known verifier (used for the RFC test vector and for resuming a flow).
    pub fn from_verifier(verifier: &str) -> Result<Self, LlmError> {
        let valid_chars = verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
        if !VERIFIER_LEN.contains(&verifier.len()) || !valid_chars {
            return Err(LlmError::SecretStore("invalid PKCE verifier".to_string()));
        }
        Ok(Self {
            verifier: Zeroizing::new(verifier.to_string()),
            challenge: Self::challenge_for(verifier),
        })
    }

    /// `BASE64URL-NOPAD(SHA-256(verifier))`.
    pub fn challenge_for(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }

    /// The `S256` challenge to send in the authorisation request.
    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// The secret verifier to send in the code exchange.
    pub fn verifier(&self) -> &str {
        &self.verifier
    }
}

/// Build the URL to open in the user's browser. `callback` must be a loopback URL
/// (`http://127.0.0.1:<port>/...` or `http://localhost:<port>/...`).
pub fn openrouter_auth_url(callback: &Url, challenge: &str) -> Result<Url, LlmError> {
    let loopback = matches!(
        callback.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    );
    if callback.scheme() != "http" || !loopback {
        return Err(LlmError::SecretStore(
            "OAuth callback must be an http loopback URL".to_string(),
        ));
    }
    let mut url =
        Url::parse(OPENROUTER_AUTH_URL).map_err(|e| LlmError::SecretStore(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("callback_url", callback.as_str())
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url)
}

/// Exchange the authorisation `code` for an API key. Not implemented in the skeleton; the daemon agent fills it in
/// together with the loopback listener.
#[cfg(feature = "http")]
pub async fn exchange_code(
    _client: &reqwest::Client,
    _code: &str,
    _pair: &PkcePair,
) -> Result<Zeroizing<String>, LlmError> {
    Err(LlmError::NotImplemented("openrouter code exchange"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b_vector() {
        let pair = PkcePair::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk").unwrap();
        assert_eq!(
            pair.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_valid_and_unique() {
        let a = PkcePair::generate();
        let b = PkcePair::generate();
        assert_eq!(a.verifier().len(), 43);
        assert_ne!(a.verifier(), b.verifier());
        assert!(PkcePair::from_verifier(a.verifier()).is_ok());
        assert_eq!(PkcePair::challenge_for(a.verifier()), a.challenge());
    }

    #[test]
    fn rejects_bad_verifiers() {
        assert!(PkcePair::from_verifier("short").is_err());
        assert!(PkcePair::from_verifier(&"a".repeat(129)).is_err());
        assert!(PkcePair::from_verifier(&format!("{}!", "a".repeat(50))).is_err());
    }

    #[test]
    fn debug_never_prints_the_verifier() {
        let pair = PkcePair::generate();
        let text = format!("{pair:?}");
        assert!(!text.contains(pair.verifier()));
    }

    #[test]
    fn auth_url_requires_loopback_callback() {
        let pair = PkcePair::generate();
        let ok = Url::parse("http://127.0.0.1:43111/callback").unwrap();
        let url = openrouter_auth_url(&ok, pair.challenge()).unwrap();
        assert_eq!(url.host_str(), Some("openrouter.ai"));
        let query: Vec<_> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
        assert_eq!(
            query,
            ["callback_url", "code_challenge", "code_challenge_method"]
        );
        let remote = Url::parse("https://evil.example/callback").unwrap();
        assert!(openrouter_auth_url(&remote, pair.challenge()).is_err());
    }
}
