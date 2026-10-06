//! Privacy tiers and the redaction applied to every prompt.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::provider::{Locality, ProviderKind};

/// What may enter a prompt. Cookies, tokens and signed-URL queries are redacted at **every** tier.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyTier {
    /// Default: names, sizes, types, redacted URLs and page structure, never file contents.
    #[default]
    MetadataOnly,
    /// Like `metadata_only`, but only providers that run on this machine are used.
    LocalOnly,
    /// Short excerpts of content are allowed (explicit opt-in per task).
    Content,
}

impl PrivacyTier {
    /// Stable name, identical to the serde form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MetadataOnly => "metadata_only",
            Self::LocalOnly => "local_only",
            Self::Content => "content",
        }
    }

    /// Whether `provider` may receive prompts at this tier.
    pub fn check_provider(self, provider: ProviderKind) -> Result<(), LlmError> {
        match (self, provider.locality()) {
            (Self::LocalOnly, Locality::Remote) => Err(LlmError::PrivacyViolation {
                tier: self.as_str(),
                reason: "provider is not local",
            }),
            _ => Ok(()),
        }
    }
}

const REDACTED: &str = "[REDACTED]";

/// Redact secrets from text that is about to enter a model prompt.
///
/// Removes URL userinfo, query strings and fragments (replaced by `?…`), the value of `Cookie`,
/// `Set-Cookie`, `Authorization` and `Proxy-Authorization` header lines, and `Bearer` tokens. It is
/// deliberately conservative: over-redaction is acceptable, under-redaction is a bug (00 section 8.2).
pub fn redact_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for (index, line) in input.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&redact_line(line));
    }
    out
}

fn redact_line(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    for header in [
        "cookie:",
        "set-cookie:",
        "authorization:",
        "proxy-authorization:",
    ] {
        if let Some(pos) = lower.find(header) {
            let keep = pos + header.len();
            return format!("{} {REDACTED}", &line[..keep]);
        }
    }
    let line = redact_bearer(line);
    redact_urls(&line)
}

fn redact_bearer(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let Some(pos) = lower.find("bearer ") else {
        return line.to_string();
    };
    let start = pos + "bearer ".len();
    let end = line[start..]
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .map_or(line.len(), |n| start + n);
    format!("{}{REDACTED}{}", &line[..start], &line[end..])
}

fn redact_urls(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = find_scheme(rest) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']'))
            .unwrap_or(tail.len());
        out.push_str(&redact_one_url(&tail[..end]));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn find_scheme(text: &str) -> Option<usize> {
    let a = text.find("http://");
    let b = text.find("https://");
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

fn redact_one_url(url: &str) -> String {
    let scheme_end = url.find("://").map_or(0, |p| p + 3);
    let (scheme, rest) = url.split_at(scheme_end);
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    let (path, extra) = tail.split_at(path_end);
    if extra.starts_with('?') {
        format!("{scheme}{authority}{path}?…")
    } else {
        format!("{scheme}{authority}{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_query_fragment_and_userinfo() {
        let text =
            "see https://user:pw@files.example.com/a/b.zip?X-Amz-Signature=abc123&x=1#frag now";
        let out = redact_text(text);
        assert_eq!(out, "see https://files.example.com/a/b.zip?… now");
        assert!(!out.contains("abc123") && !out.contains("pw@"));
    }

    #[test]
    fn redacts_secret_header_lines() {
        let out = redact_text(
            "GET /\nCookie: sid=ZEUS-CANARY-1; a=b\nAuthorization: Bearer ZEUS-CANARY-2",
        );
        assert!(!out.contains("ZEUS-CANARY"));
        assert!(out.contains("Cookie: [REDACTED]"));
    }

    #[test]
    fn redacts_inline_bearer_tokens() {
        let out = redact_text("curl -H \"x: Bearer ZEUS-CANARY-3\" ok");
        assert!(!out.contains("ZEUS-CANARY"));
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(
            redact_text("quarterly report 2026.pdf"),
            "quarterly report 2026.pdf"
        );
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = redact_text("https://a.example/p?token=ZEUS-CANARY-4");
        assert_eq!(redact_text(&once), once);
    }

    #[test]
    fn local_only_rejects_remote_providers() {
        assert!(
            PrivacyTier::LocalOnly
                .check_provider(ProviderKind::OpenrouterOauth)
                .is_err()
        );
        assert!(
            PrivacyTier::LocalOnly
                .check_provider(ProviderKind::LocalOllama)
                .is_ok()
        );
        assert!(
            PrivacyTier::MetadataOnly
                .check_provider(ProviderKind::OpenrouterOauth)
                .is_ok()
        );
    }
}
