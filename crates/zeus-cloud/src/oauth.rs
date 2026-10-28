//! Native cloud authorization with provider-specific flows and redacted token transport.
//!
//! The caller owns human review and a CLI/bridge loopback receiver. No listener, browser
//! profile, ambient credential discovery or automatic sign-in is created by this module.
use crate::{ProviderKind, SecretString};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use url::Url;
use zeroize::{Zeroize, Zeroizing};

/// Sanitized authorization failures; provider bodies and signed URLs are never displayed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OAuthError {
    /// App registration, redirect or scope is invalid.
    #[error("invalid cloud OAuth registration or redirect")]
    Invalid,
    /// Explicit operation is not supported by this provider.
    #[error("cloud OAuth operation is not supported for this provider")]
    Unsupported,
    /// Human/provider declined consent.
    #[error("cloud OAuth authorization was denied or revoked")]
    Denied,
    /// Flow or provider code expired.
    #[error("cloud OAuth flow expired; start a new sign-in")]
    Expired,
    /// Device authorization is still waiting for the person.
    #[error("cloud OAuth authorization is pending")]
    Pending,
    /// Device polling must reduce its rate.
    #[error("cloud OAuth polling must slow down")]
    SlowDown,
    /// Refresh material is unavailable; a person must sign in again.
    #[error("cloud OAuth requires sign-in again")]
    SignInRequired,
    /// Bounded HTTP response did not match the provider contract.
    #[error("invalid cloud OAuth response")]
    Protocol,
    /// Network failure without provider text or URLs.
    #[error("cloud OAuth transport failed")]
    Transport,
    /// Caller stopped the operation.
    #[error("cloud OAuth operation canceled")]
    Canceled,
}
/// Result of a bounded native OAuth operation.
pub type OAuthResult<T> = Result<T, OAuthError>;

/// Nonsecret, explicitly supplied app registration. Client passwords live only in the keyring.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OAuthRegistration {
    /// Native provider; rclone manages its own authorization.
    pub provider: ProviderKind,
    /// Registered public application identifier.
    pub client_id: String,
    /// Exact permissions the person will review on the provider consent page.
    pub scopes: Vec<String>,
}
impl OAuthRegistration {
    /// Validate against supported provider permissions; never infer an app registration.
    pub fn validate(&self) -> OAuthResult<()> {
        if !plain(&self.client_id, 512) || self.scopes.is_empty() || self.scopes.len() > 8 {
            return Err(OAuthError::Invalid);
        }
        let allowed: &[&str] = match self.provider {
            ProviderKind::GoogleDrive => &[
                "https://www.googleapis.com/auth/drive.file",
                "https://www.googleapis.com/auth/drive",
                "https://www.googleapis.com/auth/drive.readonly",
                "https://www.googleapis.com/auth/drive.metadata.readonly",
            ],
            ProviderKind::YandexDisk => &[
                "cloud_api:disk.read",
                "cloud_api:disk.write",
                "cloud_api:disk.app_folder",
            ],
            ProviderKind::BaiduNetdisk => &["basic", "netdisk"],
            _ => return Err(OAuthError::Unsupported),
        };
        if self.scopes.iter().any(|v| !allowed.contains(&v.as_str()))
            || self
                .scopes
                .iter()
                .enumerate()
                .any(|(i, v)| self.scopes[..i].contains(v))
        {
            return Err(OAuthError::Invalid);
        }
        Ok(())
    }
    /// Least-privilege starting permissions; broader Drive access requires an explicit choice.
    pub fn defaults(provider: ProviderKind, client_id: String) -> OAuthResult<Self> {
        let scopes = match provider {
            ProviderKind::GoogleDrive => vec!["https://www.googleapis.com/auth/drive.file"],
            ProviderKind::YandexDisk => vec!["cloud_api:disk.read", "cloud_api:disk.write"],
            ProviderKind::BaiduNetdisk => vec!["basic", "netdisk"],
            _ => return Err(OAuthError::Unsupported),
        }
        .into_iter()
        .map(str::to_owned)
        .collect();
        let out = Self {
            provider,
            client_id,
            scopes,
        };
        out.validate()?;
        Ok(out)
    }
}

/// Fresh PKCE verifier and state, retained privately until a one-use code exchange.
#[derive(Debug)]
pub struct OAuthProof {
    verifier: SecretString,
    state: SecretString,
}
impl OAuthProof {
    /// Generate independent cryptographically random verifier and state values.
    pub fn generate() -> Self {
        Self {
            verifier: random_secret(),
            state: random_secret(),
        }
    }
    /// State value sent only to the reviewed provider URL and trusted callback client.
    pub fn state(&self) -> &str {
        self.state.expose()
    }
    /// S256 proof, never the verifier.
    pub fn challenge(&self) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.expose().as_bytes()))
    }
    /// Verify state before exchanging a code.
    pub fn matches_state(&self, state: &str) -> bool {
        self.state.expose() == state
    }
}
fn random_secret() -> SecretString {
    let mut bytes = Zeroizing::new([0u8; 32]);
    rand::fill(&mut bytes[..]);
    SecretString::new(URL_SAFE_NO_PAD.encode(bytes.as_slice()))
}
fn plain(v: &str, cap: usize) -> bool {
    !v.is_empty() && v.len() <= cap && !v.chars().any(char::is_control)
}
/// Current wall-clock time for token expiration, without recording ambient environment state.
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

/// Validate the CLI-owned callback before constructing a provider URL.
pub fn validate_redirect(raw: &str) -> OAuthResult<Url> {
    let url = Url::parse(raw).map_err(|_| OAuthError::Invalid)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || url.port() == Some(0)
        || url.path() != "/oauth/callback"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(OAuthError::Invalid);
    }
    Ok(url)
}
/// Build a reviewed Google/Yandex authorization URL with PKCE and state.
pub fn authorization_url(
    reg: &OAuthRegistration,
    redirect: &str,
    proof: &OAuthProof,
    device_id: &str,
) -> OAuthResult<SecretString> {
    reg.validate()?;
    validate_redirect(redirect)?;
    let endpoint = match reg.provider {
        ProviderKind::GoogleDrive => "https://accounts.google.com/o/oauth2/v2/auth",
        ProviderKind::YandexDisk => "https://oauth.yandex.com/authorize",
        _ => return Err(OAuthError::Unsupported),
    };
    let mut url = Url::parse(endpoint).map_err(|_| OAuthError::Invalid)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &reg.client_id)
        .append_pair("redirect_uri", redirect)
        .append_pair("scope", &reg.scopes.join(" "))
        .append_pair("state", proof.state())
        .append_pair("code_challenge", &proof.challenge())
        .append_pair("code_challenge_method", "S256");
    match reg.provider {
        ProviderKind::GoogleDrive => {
            url.query_pairs_mut()
                .append_pair("access_type", "offline")
                .append_pair("prompt", "consent");
        }
        ProviderKind::YandexDisk => {
            if !plain(device_id, 50) || device_id.len() < 6 {
                return Err(OAuthError::Invalid);
            }
            url.query_pairs_mut()
                .append_pair("device_id", device_id)
                .append_pair("device_name", "Zeus desktop");
        }
        _ => {}
    }
    Ok(SecretString::new(url.into()))
}

/// One private keyring object; never a public JSON API result or Debug token disclosure.
#[derive(Debug, Clone)]
pub struct OAuthTokens {
    /// Access bearer passed only to the selected cloud adapter.
    pub access: SecretString,
    /// Optional refresh token, rotated atomically with access material.
    pub refresh: Option<SecretString>,
    /// Optional registered application password, never part of account metadata.
    pub client_secret: Option<SecretString>,
    /// Absolute access-token expiration.
    pub expires_at_ms: u64,
}
impl OAuthTokens {
    /// Encode exclusively for the already-selected private credential store.
    pub fn encode_private(&self) -> OAuthResult<SecretString> {
        let value = PrivateTokenRef {
            access_token: self.access.expose(),
            refresh_token: self.refresh.as_ref().map(SecretString::expose),
            client_secret: self.client_secret.as_ref().map(SecretString::expose),
            expires_at_ms: self.expires_at_ms,
        };
        serde_json::to_string(&value)
            .map(SecretString::new)
            .map_err(|_| OAuthError::Protocol)
    }
    /// Decode the keyring object; plaintext account/config files never supply this object.
    pub fn decode_private(value: &SecretString) -> OAuthResult<Self> {
        let raw: PrivateTokenOwned =
            serde_json::from_str(value.expose()).map_err(|_| OAuthError::Protocol)?;
        if !plain(&raw.access_token, 16384)
            || raw.refresh_token.as_ref().is_some_and(|v| !plain(v, 16384))
            || raw.client_secret.as_ref().is_some_and(|v| !plain(v, 16384))
            || raw.expires_at_ms == 0
        {
            return Err(OAuthError::Protocol);
        }
        Ok(Self {
            access: SecretString::new(raw.access_token.clone()),
            refresh: raw.refresh_token.clone().map(SecretString::new),
            client_secret: raw.client_secret.clone().map(SecretString::new),
            expires_at_ms: raw.expires_at_ms,
        })
    }
    /// Refresh before a near-expiry transfer; the resolver serializes this per account.
    pub fn needs_refresh(&self, now_ms: u64) -> bool {
        self.expires_at_ms <= now_ms.saturating_add(60_000)
    }
}
#[derive(Serialize)]
struct PrivateTokenRef<'a> {
    access_token: &'a str,
    refresh_token: Option<&'a str>,
    client_secret: Option<&'a str>,
    expires_at_ms: u64,
}
#[derive(Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
struct PrivateTokenOwned {
    access_token: String,
    refresh_token: Option<String>,
    client_secret: Option<String>,
    expires_at_ms: u64,
}

/// Device response retained in memory; only a human-only view may reveal its user code/URL.
#[derive(Debug)]
pub struct DeviceAuthorization {
    /// Secret polling code, never sent to the browser or public status.
    pub device_code: SecretString,
    /// Code the person enters on the provider's verification page.
    pub user_code: SecretString,
    /// Official verification page.
    pub verification_uri: SecretString,
    /// Lifetime in milliseconds.
    pub expires_in_ms: u64,
    /// Minimum provider polling interval.
    pub interval_ms: u64,
}

/// One state-verified authorization-code exchange; callback parsing belongs to the CLI.
pub struct CodeExchange<'a> {
    /// App registration whose permissions were reviewed.
    pub registration: &'a OAuthRegistration,
    /// One-use state and verifier retained by the flow owner.
    pub proof: &'a OAuthProof,
    /// Exact bound loopback redirect.
    pub redirect: &'a str,
    /// One-use code captured by the trusted CLI receiver.
    pub code: &'a SecretString,
    /// Optional explicitly supplied application password.
    pub client_secret: Option<SecretString>,
    /// Stable account/device identifier for Yandex token revocation.
    pub device_id: &'a str,
}

/// Bounded OAuth HTTP transport. Production endpoints cannot be supplied by API callers.
pub struct OAuthHttp {
    client: reqwest::Client,
    google_token: Url,
    google_revoke: Url,
    yandex_token: Url,
    yandex_revoke: Url,
    baidu_device: Url,
    baidu_token: Url,
}
impl OAuthHttp {
    /// Use official HTTPS endpoints, without redirects or inherited HTTP proxies.
    pub fn new() -> OAuthResult<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|_| OAuthError::Transport)?,
            google_token: parse("https://oauth2.googleapis.com/token")?,
            google_revoke: parse("https://oauth2.googleapis.com/revoke")?,
            yandex_token: parse("https://oauth.yandex.com/token")?,
            yandex_revoke: parse("https://oauth.yandex.com/revoke_token")?,
            baidu_device: parse("https://openapi.baidu.com/oauth/2.0/device/code")?,
            baidu_token: parse("https://openapi.baidu.com/oauth/2.0/token")?,
        })
    }
    /// Explicit loopback-only endpoints for hermetic cross-crate tests; absent in normal builds.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn loopback_fixture(base: &str) -> OAuthResult<Self> {
        let base = parse(base)?;
        if base.scheme() != "http"
            || base.host_str() != Some("127.0.0.1")
            || base.port().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(OAuthError::Invalid);
        }
        let mut client = Self::new()?;
        let join = |path: &str| base.join(path).map_err(|_| OAuthError::Invalid);
        client.google_token = join("/token")?;
        client.google_revoke = join("/revoke")?;
        client.yandex_token = join("/token")?;
        client.yandex_revoke = join("/revoke")?;
        client.baidu_device = join("/device")?;
        client.baidu_token = join("/token")?;
        Ok(client)
    }
    /// Exchange one state-verified PKCE code. The proof is consumed by the caller's flow record.
    pub async fn exchange(
        &self,
        input: CodeExchange<'_>,
        cancel: &CancellationToken,
    ) -> OAuthResult<OAuthTokens> {
        let CodeExchange {
            registration: reg,
            proof,
            redirect,
            code,
            client_secret,
            device_id,
        } = input;
        reg.validate()?;
        validate_redirect(redirect)?;
        if !plain(code.expose(), 8192) {
            return Err(OAuthError::Invalid);
        }
        let mut fields = vec![
            ("grant_type", "authorization_code"),
            ("client_id", reg.client_id.as_str()),
            ("code", code.expose()),
            ("redirect_uri", redirect),
            ("code_verifier", proof.verifier.expose()),
        ];
        if let Some(secret) = &client_secret {
            fields.push(("client_secret", secret.expose()));
        }
        if reg.provider == ProviderKind::YandexDisk {
            fields.push(("device_id", device_id));
            fields.push(("device_name", "Zeus desktop"));
        }
        let endpoint = match reg.provider {
            ProviderKind::GoogleDrive => &self.google_token,
            ProviderKind::YandexDisk => &self.yandex_token,
            _ => return Err(OAuthError::Unsupported),
        };
        let v = response(self.client.post(endpoint.clone()).form(&fields), cancel).await?;
        tokens(v, reg, None, client_secret)
    }
    /// Request Baidu's documented device-code flow, without opening an embedded browser.
    pub async fn device(
        &self,
        reg: &OAuthRegistration,
        cancel: &CancellationToken,
    ) -> OAuthResult<DeviceAuthorization> {
        reg.validate()?;
        if reg.provider != ProviderKind::BaiduNetdisk {
            return Err(OAuthError::Unsupported);
        }
        let v = response(
            self.client.get(self.baidu_device.clone()).query(&[
                ("response_type", "device_code"),
                ("client_id", &reg.client_id),
                ("scope", &reg.scopes.join(" ")),
            ]),
            cancel,
        )
        .await?;
        let uri = text(&v, "verification_url")?;
        let parsed = parse(&uri)?;
        if parsed.scheme() != "https"
            || parsed.host_str() != Some("openapi.baidu.com")
            || parsed.port_or_known_default() != Some(443)
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(OAuthError::Protocol);
        }
        let expires = number(&v, "expires_in")?;
        let interval = v
            .get("interval")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(5);
        if expires == 0 || expires > 3600 || interval == 0 || interval > 300 {
            return Err(OAuthError::Protocol);
        }
        Ok(DeviceAuthorization {
            device_code: SecretString::new(text(&v, "device_code")?),
            user_code: SecretString::new(text(&v, "user_code")?),
            verification_uri: SecretString::new(uri),
            expires_in_ms: expires * 1000,
            interval_ms: interval * 1000,
        })
    }
    /// Poll Baidu once; caller enforces the provider interval, expiry and slow-down response.
    pub async fn poll_device(
        &self,
        reg: &OAuthRegistration,
        device_code: &SecretString,
        client_secret: SecretString,
        cancel: &CancellationToken,
    ) -> OAuthResult<OAuthTokens> {
        if reg.provider != ProviderKind::BaiduNetdisk {
            return Err(OAuthError::Unsupported);
        }
        reg.validate()?;
        let v = response(
            self.client.get(self.baidu_token.clone()).query(&[
                ("grant_type", "device_token"),
                ("code", device_code.expose()),
                ("client_id", &reg.client_id),
                ("client_secret", client_secret.expose()),
            ]),
            cancel,
        )
        .await?;
        tokens(v, reg, None, Some(client_secret))
    }
    /// Refresh only the selected account's token; a rotated refresh token replaces the old one.
    pub async fn refresh(
        &self,
        reg: &OAuthRegistration,
        old: &OAuthTokens,
        cancel: &CancellationToken,
    ) -> OAuthResult<OAuthTokens> {
        reg.validate()?;
        let refresh = old.refresh.as_ref().ok_or(OAuthError::SignInRequired)?;
        let mut fields = vec![
            ("grant_type", "refresh_token"),
            ("client_id", reg.client_id.as_str()),
            ("refresh_token", refresh.expose()),
        ];
        if let Some(secret) = &old.client_secret {
            fields.push(("client_secret", secret.expose()));
        }
        let request = match reg.provider {
            ProviderKind::GoogleDrive => self.client.post(self.google_token.clone()).form(&fields),
            ProviderKind::YandexDisk => self.client.post(self.yandex_token.clone()).form(&fields),
            ProviderKind::BaiduNetdisk => self.client.get(self.baidu_token.clone()).query(&fields),
            _ => return Err(OAuthError::Unsupported),
        };
        let v = response(request, cancel).await?;
        tokens(v, reg, old.refresh.clone(), old.client_secret.clone())
    }
    /// Remote revocation where officially implemented. Baidu requires user-side access management.
    pub async fn revoke(
        &self,
        reg: &OAuthRegistration,
        value: &OAuthTokens,
        cancel: &CancellationToken,
    ) -> OAuthResult<()> {
        reg.validate()?;
        let request = match reg.provider {
            ProviderKind::GoogleDrive => self.client.post(self.google_revoke.clone()).form(&[(
                "token",
                value.refresh.as_ref().unwrap_or(&value.access).expose(),
            )]),
            ProviderKind::YandexDisk => {
                let secret = value
                    .client_secret
                    .as_ref()
                    .ok_or(OAuthError::Unsupported)?;
                self.client.post(self.yandex_revoke.clone()).form(&[
                    ("access_token", value.access.expose()),
                    ("client_id", &reg.client_id),
                    ("client_secret", secret.expose()),
                ])
            }
            _ => return Err(OAuthError::Unsupported),
        };
        if reg.provider == ProviderKind::YandexDisk {
            let value = response(request, cancel).await?;
            return if value.get("status").and_then(serde_json::Value::as_str) == Some("ok") {
                Ok(())
            } else {
                Err(OAuthError::Protocol)
            };
        }
        let response = tokio::select! {biased; _=cancel.cancelled()=>return Err(OAuthError::Canceled), r=request.send()=>r.map_err(|_|OAuthError::Transport)?};
        if response.status() == reqwest::StatusCode::OK {
            Ok(())
        } else {
            Err(OAuthError::Denied)
        }
    }
}
fn parse(value: &str) -> OAuthResult<Url> {
    Url::parse(value).map_err(|_| OAuthError::Invalid)
}
fn text(v: &serde_json::Value, key: &str) -> OAuthResult<String> {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| plain(s, 16384))
        .map(str::to_owned)
        .ok_or(OAuthError::Protocol)
}
fn number(v: &serde_json::Value, key: &str) -> OAuthResult<u64> {
    v.get(key)
        .and_then(|n| n.as_u64().or_else(|| n.as_str()?.parse().ok()))
        .ok_or(OAuthError::Protocol)
}
fn tokens(
    v: PrivateJson,
    reg: &OAuthRegistration,
    fallback: Option<SecretString>,
    client_secret: Option<SecretString>,
) -> OAuthResult<OAuthTokens> {
    if let Some(scope) = v.get("scope").and_then(serde_json::Value::as_str) {
        let granted: Vec<_> = scope.split_whitespace().collect();
        if reg.scopes.iter().any(|s| !granted.contains(&s.as_str())) {
            return Err(OAuthError::Denied);
        }
    }
    if v.get("token_type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|v| !v.eq_ignore_ascii_case("bearer"))
    {
        return Err(OAuthError::Protocol);
    }
    let ttl = number(&v, "expires_in")?;
    let expires_at_ms = ttl
        .checked_mul(1000)
        .and_then(|ms| unix_ms().checked_add(ms))
        .filter(|ms| *ms <= 253_402_300_799_999)
        .filter(|_| ttl > 0)
        .ok_or(OAuthError::Protocol)?;
    Ok(OAuthTokens {
        access: SecretString::new(text(&v, "access_token")?),
        refresh: v
            .get("refresh_token")
            .filter(|v| !v.is_null())
            .map(|_| text(&v, "refresh_token").map(SecretString::new))
            .transpose()?
            .or(fallback),
        client_secret,
        expires_at_ms,
    })
}
async fn response(
    request: reqwest::RequestBuilder,
    cancel: &CancellationToken,
) -> OAuthResult<PrivateJson> {
    let mut response = tokio::select! {biased;_=cancel.cancelled()=>return Err(OAuthError::Canceled),v=request.send()=>v.map_err(|_|OAuthError::Transport)?};
    let status = response.status();
    if response.content_length().is_some_and(|n| n > 65536) {
        return Err(OAuthError::Protocol);
    }
    let mut body = Zeroizing::new(Vec::new());
    loop {
        let chunk = tokio::select! {biased;_=cancel.cancelled()=>return Err(OAuthError::Canceled),v=response.chunk()=>v.map_err(|_|OAuthError::Transport)?};
        let Some(chunk) = chunk else { break };
        if body.len().saturating_add(chunk.len()) > 65536 {
            return Err(OAuthError::Protocol);
        }
        body.extend_from_slice(&chunk);
    }
    let value = PrivateJson(serde_json::from_slice(&body).map_err(|_| OAuthError::Protocol)?);
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        return Err(match error {
            "authorization_pending" => OAuthError::Pending,
            "slow_down" => OAuthError::SlowDown,
            "expired_token" | "expired_device_code" => OAuthError::Expired,
            "invalid_grant" | "invalid_token" => OAuthError::SignInRequired,
            _ => OAuthError::Denied,
        });
    }
    if !status.is_success() {
        return Err(OAuthError::Denied);
    }
    Ok(value)
}

// Token responses have a narrow owner so provider-supplied strings are zeroized even on failure.
struct PrivateJson(serde_json::Value);
impl std::ops::Deref for PrivateJson {
    type Target = serde_json::Value;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for PrivateJson {
    fn drop(&mut self) {
        fn scrub(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::String(s) => s.zeroize(),
                serde_json::Value::Array(values) => values.iter_mut().for_each(scrub),
                serde_json::Value::Object(values) => values.values_mut().for_each(scrub),
                _ => {}
            }
        }
        scrub(&mut self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::State,
        http::{Request, StatusCode},
        response::IntoResponse,
        routing::any,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    const ACCESS: &str = "ZEUS-CANARY-oauth-access";
    const REFRESH: &str = "ZEUS-CANARY-oauth-refresh";
    #[derive(Clone)]
    struct Fixture {
        mode: u8,
        calls: Arc<AtomicUsize>,
        leaks: Arc<AtomicUsize>,
    }
    async fn handler(
        State(state): State<Fixture>,
        request: Request<Body>,
    ) -> axum::response::Response {
        state.calls.fetch_add(1, Ordering::SeqCst);
        let path = request.uri().path().to_owned();
        if path == "/leak" {
            state.leaks.fetch_add(1, Ordering::SeqCst);
            return StatusCode::OK.into_response();
        }
        if state.mode == 3 {
            return (StatusCode::FOUND, [("location", "/leak")], "{}").into_response();
        }
        if state.mode == 4 {
            return (StatusCode::OK, "x".repeat(65537)).into_response();
        }
        let query = request.uri().query().unwrap_or_default().to_owned();
        let method = request.method().clone();
        let body = to_bytes(request.into_body(), 65536).await.unwrap();
        let values: std::collections::BTreeMap<String, String> =
            url::form_urlencoded::parse(if method == reqwest::Method::GET {
                query.as_bytes()
            } else {
                &body
            })
            .into_owned()
            .collect();
        if state.mode == 5 {
            assert_eq!(values["client_id"], "fixture-app");
            assert_eq!(values["client_secret"], "fixture-app-password");
            assert_eq!(method, reqwest::Method::POST);
            if path == "/revoke" {
                assert_eq!(values["access_token"], ACCESS);
                return axum::Json(serde_json::json!({"status":"ok"})).into_response();
            }
            if values["grant_type"] == "authorization_code" {
                assert_eq!(values["device_id"], "fixture-device");
                assert_eq!(values["device_name"], "Zeus desktop");
                assert_eq!(values["code"], "ZEUS-CANARY-code");
                assert!(!values["code_verifier"].is_empty());
            } else {
                assert_eq!(values["grant_type"], "refresh_token");
                assert_eq!(values["refresh_token"], REFRESH);
            }
            return axum::Json(serde_json::json!({"access_token":ACCESS,"refresh_token":REFRESH,"token_type":"bearer","expires_in":124234123534u64})).into_response();
        }
        if path == "/revoke" {
            assert_eq!(values["token"], REFRESH);
            return (StatusCode::OK, "{}").into_response();
        }
        if state.mode == 2 {
            if path == "/device" {
                assert_eq!(values["response_type"], "device_code");
                return axum::Json(serde_json::json!({"device_code":"ZEUS-CANARY-device","user_code":"TEST-1234","verification_url":"https://openapi.baidu.com/device","expires_in":600,"interval":5})).into_response();
            }
            assert_eq!(values["grant_type"], "device_token");
            assert_eq!(values["code"], "ZEUS-CANARY-device");
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(
                    serde_json::json!({"error":"authorization_pending","error_description":ACCESS}),
                ),
            )
                .into_response();
        }
        match values["grant_type"].as_str() {
            "authorization_code" => {
                assert_eq!(values["code"], "ZEUS-CANARY-code");
                assert_eq!(values["client_id"], "fixture-app");
                assert_eq!(
                    values["redirect_uri"],
                    "http://127.0.0.1:54321/oauth/callback"
                );
                assert_eq!(
                    URL_SAFE_NO_PAD
                        .encode(Sha256::digest(values["code_verifier"].as_bytes()))
                        .len(),
                    43
                );
            }
            "refresh_token" => assert_eq!(values["refresh_token"], REFRESH),
            _ => panic!("unexpected grant"),
        }
        axum::Json(serde_json::json!({"access_token":ACCESS,"refresh_token":"ZEUS-CANARY-rotated","token_type":"bearer","expires_in":3600,"scope":"https://www.googleapis.com/auth/drive.file"})).into_response()
    }
    async fn fixture(mode: u8) -> (OAuthHttp, Fixture, tokio::task::JoinHandle<()>) {
        let state = Fixture {
            mode,
            calls: Arc::new(AtomicUsize::new(0)),
            leaks: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/{*path}", any(handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut http = OAuthHttp::new().unwrap();
        http.google_token = parse(&format!("{base}/token")).unwrap();
        http.google_revoke = parse(&format!("{base}/revoke")).unwrap();
        http.yandex_token = parse(&format!("{base}/token")).unwrap();
        http.yandex_revoke = parse(&format!("{base}/revoke")).unwrap();
        http.baidu_device = parse(&format!("{base}/device")).unwrap();
        http.baidu_token = parse(&format!("{base}/token")).unwrap();
        (http, state, task)
    }
    #[test]
    fn pkce_matches_rfc7636_vector_and_uses_independent_state() {
        let proof = OAuthProof {
            verifier: SecretString::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into()),
            state: SecretString::new("state".into()),
        };
        assert_eq!(
            proof.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(!proof.matches_state("other"));
        let a = OAuthProof::generate();
        let b = OAuthProof::generate();
        assert_ne!(a.state(), b.state());
        assert_ne!(a.verifier.expose(), a.state());
        assert!(!format!("{a:?}").contains(a.verifier.expose()));
    }
    #[test]
    fn permissions_redirect_and_keyring_format_are_explicit() {
        let mut reg =
            OAuthRegistration::defaults(ProviderKind::GoogleDrive, "fixture-app".into()).unwrap();
        assert!(!reg.scopes.iter().any(|v| v.ends_with("/drive")));
        reg.scopes.push("unreviewed:scope".into());
        assert_eq!(reg.validate(), Err(OAuthError::Invalid));
        for u in [
            "https://127.0.0.1:4/oauth/callback",
            "http://localhost:4/oauth/callback",
            "http://127.0.0.1/oauth/callback",
            "http://user@127.0.0.1:4/oauth/callback",
            "http://127.0.0.1:4/other",
        ] {
            assert!(validate_redirect(u).is_err());
        }
        let value = OAuthTokens {
            access: SecretString::new(ACCESS.into()),
            refresh: Some(SecretString::new(REFRESH.into())),
            client_secret: None,
            expires_at_ms: unix_ms() + 3600000,
        };
        assert!(!format!("{value:?}").contains("ZEUS-CANARY"));
        let decoded = OAuthTokens::decode_private(&value.encode_private().unwrap()).unwrap();
        assert_eq!(decoded.refresh.unwrap().expose(), REFRESH);
    }
    #[tokio::test]
    async fn real_loopback_code_exchange_refresh_rotation_and_revoke() {
        let (http, state, task) = fixture(1).await;
        let reg =
            OAuthRegistration::defaults(ProviderKind::GoogleDrive, "fixture-app".into()).unwrap();
        let proof = OAuthProof::generate();
        let cancel = CancellationToken::new();
        let first = http
            .exchange(
                CodeExchange {
                    registration: &reg,
                    proof: &proof,
                    redirect: "http://127.0.0.1:54321/oauth/callback",
                    code: &SecretString::new("ZEUS-CANARY-code".into()),
                    client_secret: None,
                    device_id: "fixture-device",
                },
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(first.access.expose(), ACCESS);
        let old = OAuthTokens {
            access: SecretString::new(ACCESS.into()),
            refresh: Some(SecretString::new(REFRESH.into())),
            client_secret: None,
            expires_at_ms: 1,
        };
        let next = http.refresh(&reg, &old, &cancel).await.unwrap();
        assert_eq!(
            next.refresh.as_ref().unwrap().expose(),
            "ZEUS-CANARY-rotated"
        );
        assert!(next.expires_at_ms > unix_ms());
        http.revoke(&reg, &old, &cancel).await.unwrap();
        assert_eq!(state.calls.load(Ordering::SeqCst), 3);
        task.abort();
    }
    #[tokio::test]
    async fn yandex_pkce_device_binding_refresh_and_confirmed_revoke() {
        let (http, state, task) = fixture(5).await;
        let reg =
            OAuthRegistration::defaults(ProviderKind::YandexDisk, "fixture-app".into()).unwrap();
        let proof = OAuthProof::generate();
        let url = authorization_url(
            &reg,
            "http://127.0.0.1:54321/oauth/callback",
            &proof,
            "fixture-device",
        )
        .unwrap();
        let parsed = Url::parse(url.expose()).unwrap();
        assert!(
            parsed
                .query_pairs()
                .any(|(k, v)| k == "device_id" && v == "fixture-device")
        );
        assert!(
            parsed
                .query_pairs()
                .any(|(k, v)| k == "code_challenge" && v == proof.challenge())
        );
        let cancel = CancellationToken::new();
        let first = http
            .exchange(
                CodeExchange {
                    registration: &reg,
                    proof: &proof,
                    redirect: "http://127.0.0.1:54321/oauth/callback",
                    code: &SecretString::new("ZEUS-CANARY-code".into()),
                    client_secret: Some(SecretString::new("fixture-app-password".into())),
                    device_id: "fixture-device",
                },
                &cancel,
            )
            .await
            .unwrap();
        // Yandex documents long lifetimes; accept safe RFC3339-representable expirations.
        assert!(first.expires_at_ms > unix_ms() + 100 * 366 * 86400 * 1000);
        let next = http.refresh(&reg, &first, &cancel).await.unwrap();
        http.revoke(&reg, &next, &cancel).await.unwrap();
        assert_eq!(state.calls.load(Ordering::SeqCst), 3);
        task.abort();
    }
    #[tokio::test]
    async fn device_flow_preserves_secret_poll_code_and_sanitizes_pending() {
        let (http, _, task) = fixture(2).await;
        let reg =
            OAuthRegistration::defaults(ProviderKind::BaiduNetdisk, "fixture-app".into()).unwrap();
        let cancel = CancellationToken::new();
        let device = http.device(&reg, &cancel).await.unwrap();
        assert_eq!(device.interval_ms, 5000);
        assert!(!format!("{device:?}").contains("ZEUS-CANARY"));
        let error = http
            .poll_device(
                &reg,
                &device.device_code,
                SecretString::new("fixture-app-password".into()),
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(error, OAuthError::Pending);
        assert!(!error.to_string().contains(ACCESS));
        task.abort();
    }
    #[tokio::test]
    async fn token_transport_refuses_redirects_and_oversized_responses() {
        let reg =
            OAuthRegistration::defaults(ProviderKind::GoogleDrive, "fixture-app".into()).unwrap();
        let old = OAuthTokens {
            access: SecretString::new(ACCESS.into()),
            refresh: Some(SecretString::new(REFRESH.into())),
            client_secret: None,
            expires_at_ms: 1,
        };
        for mode in [3, 4] {
            let (http, state, task) = fixture(mode).await;
            assert!(
                http.refresh(&reg, &old, &CancellationToken::new())
                    .await
                    .is_err()
            );
            assert_eq!(state.leaks.load(Ordering::SeqCst), 0);
            task.abort();
        }
    }
    #[tokio::test]
    async fn cancellation_prevents_network_dispatch() {
        let (http, state, task) = fixture(1).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let reg =
            OAuthRegistration::defaults(ProviderKind::GoogleDrive, "fixture-app".into()).unwrap();
        let old = OAuthTokens {
            access: SecretString::new(ACCESS.into()),
            refresh: Some(SecretString::new(REFRESH.into())),
            client_secret: None,
            expires_at_ms: 1,
        };
        assert_eq!(
            http.refresh(&reg, &old, &cancel).await.unwrap_err(),
            OAuthError::Canceled
        );
        assert_eq!(state.calls.load(Ordering::SeqCst), 0);
        task.abort();
    }
}
