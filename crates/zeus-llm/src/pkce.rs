//! PKCE (RFC 7636) helper and the OpenRouter authorisation URL builder.
//!
//! Endpoints and the S256 exchange were verified against the provider documentation on 2026-10-16.
//! The loopback listener requires a random state and callback path, with a bounded lifetime.

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
    if callback.scheme() != "http"
        || !loopback
        || callback.port().is_none()
        || !callback.username().is_empty()
        || callback.password().is_some()
        || callback.query().is_some()
        || callback.fragment().is_some()
    {
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

/// Exchange an authorization code for an API key. A dedicated no-redirect client protects the verifier;
/// the legacy client argument is retained for API compatibility and is not used.
#[cfg(feature = "http")]
pub async fn exchange_code(
    _client: &reqwest::Client,
    code: &str,
    pair: &PkcePair,
) -> Result<Zeroizing<String>, LlmError> {
    exchange_code_at(
        &Url::parse(OPENROUTER_EXCHANGE_URL).map_err(|_| LlmError::NotConfigured)?,
        code,
        pair,
    )
    .await
}

#[cfg(feature = "http")]
pub(crate) async fn exchange_code_at(
    endpoint: &Url,
    code: &str,
    pair: &PkcePair,
) -> Result<Zeroizing<String>, LlmError> {
    crate::http_provider::validate_endpoint(endpoint)?;
    if code.is_empty() || code.len() > 4096 {
        return Err(LlmError::AuthFailed);
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .no_proxy()
        .build()
        .map_err(|_| LlmError::ProviderUnavailable("cannot configure OAuth exchange".into()))?;
    let request = serde_json::json!({"code":code,"code_verifier":pair.verifier(),"code_challenge_method":"S256"});
    let response = client
        .post(endpoint.clone())
        .json(&request)
        .send()
        .await
        .map_err(|_| LlmError::ProviderUnavailable("OAuth exchange failed".into()))?;
    let mut value = crate::http_provider::read_json(response, 16384).await?;
    match value["key"].take() {
        serde_json::Value::String(key) if !key.is_empty() && key.len() <= 8192 => {
            Ok(Zeroizing::new(key))
        }
        _ => Err(LlmError::AuthFailed),
    }
}

/// A single-use OpenRouter login. Drop closes its listener; no browser opens without the caller's action.
#[cfg(feature = "http")]
pub struct OpenRouterLogin {
    listener: tokio::net::TcpListener,
    pair: PkcePair,
    state: Zeroizing<String>,
    callback: Url,
    authorize: Url,
}
#[cfg(feature = "http")]
impl std::fmt::Debug for OpenRouterLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenRouterLogin([REDACTED])")
    }
}
#[cfg(feature = "http")]
impl OpenRouterLogin {
    /// Bind an OS-selected IPv4 loopback port and construct the S256 authorization URL.
    pub async fn bind() -> Result<Self, LlmError> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| LlmError::ProviderUnavailable("cannot bind OAuth callback".into()))?;
        let port = listener
            .local_addr()
            .map_err(|_| LlmError::ProviderUnavailable("cannot read OAuth callback".into()))?
            .port();
        let pair = PkcePair::generate();
        let state = Zeroizing::new(PkcePair::generate().verifier().to_owned());
        let callback = Url::parse(&format!(
            "http://127.0.0.1:{port}/oauth/{}",
            PkcePair::generate().challenge()
        ))
        .map_err(|_| LlmError::NotConfigured)?;
        let mut authorize = openrouter_auth_url(&callback, pair.challenge())?;
        authorize.query_pairs_mut().append_pair("state", &state);
        Ok(Self {
            listener,
            pair,
            state,
            callback,
            authorize,
        })
    }
    /// Open this URL in a trusted human browser. It contains public PKCE challenge and state, not credentials.
    pub fn authorization_url(&self) -> &Url {
        &self.authorize
    }
    /// Wait up to five minutes, verify the callback, exchange once, then save the API key in `store`.
    pub async fn finish(self, store: &dyn crate::secrets::SecretStore) -> Result<(), LlmError> {
        let endpoint = Url::parse(OPENROUTER_EXCHANGE_URL).map_err(|_| LlmError::NotConfigured)?;
        self.finish_at(store, &endpoint, std::time::Duration::from_secs(300))
            .await
    }
    pub(crate) async fn finish_at(
        self,
        store: &dyn crate::secrets::SecretStore,
        endpoint: &Url,
        timeout: std::time::Duration,
    ) -> Result<(), LlmError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        tokio::time::timeout(timeout,async {
            loop {
                let (mut socket,peer)=self.listener.accept().await.map_err(|_|LlmError::ProviderUnavailable("OAuth listener failed".into()))?;
                if !peer.ip().is_loopback(){continue}
                let mut bytes=Zeroizing::new(Vec::new());
                let read=tokio::time::timeout(std::time::Duration::from_secs(3),async {
                    let mut chunk=[0u8;1024];loop {
                        let count=socket.read(&mut chunk).await?;
                        if count==0 || bytes.len()+count>8192{break}
                        bytes.extend_from_slice(&chunk[..count]);if bytes.windows(4).any(|v|v==b"\r\n\r\n"){break}
                    }Ok::<(),std::io::Error>(())
                }).await;
                if !matches!(read,Ok(Ok(()))){continue}
                let request=std::str::from_utf8(&bytes).unwrap_or("");
                let code=callback_code(request,&self.callback,&self.state);
                let Some(code)=code else {
                    let _=socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    continue;
                };
                let key=exchange_code_at(endpoint,&code,&self.pair).await?;
                store.set(&crate::secrets::SecretKey{provider:crate::ProviderKind::OpenrouterOauth,purpose:"api_key".into()},&key)?;
                let body="Zeus is connected. You can close this tab.";
                let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
                let _=socket.write_all(response.as_bytes()).await;return Ok(())
            }
        }).await.map_err(|_|LlmError::ProviderUnavailable("OAuth login expired".into()))?
    }
}
#[cfg(feature = "http")]
fn callback_code(request: &str, callback: &Url, state: &str) -> Option<Zeroizing<String>> {
    if !request.contains("\r\n\r\n") {
        return None;
    }
    let mut lines = request.split("\r\n");
    let mut first = lines.next()?.split(' ');
    if first.next()? != "GET" {
        return None;
    }
    let target = first.next()?;
    if first.next()? != "HTTP/1.1"
        || first.next().is_some()
        || !target.starts_with('/')
        || target.starts_with("//")
    {
        return None;
    }
    let mut host = None;
    for line in lines.take_while(|v| !v.is_empty()) {
        let (key, value) = line.split_once(':')?;
        if key.eq_ignore_ascii_case("host") && host.replace(value.trim()).is_some() {
            return None;
        }
        if key.eq_ignore_ascii_case("origin") || key.eq_ignore_ascii_case("transfer-encoding") {
            return None;
        }
        if key.eq_ignore_ascii_case("content-length") && value.trim() != "0" {
            return None;
        }
    }
    if host? != format!("127.0.0.1:{}", callback.port()?) {
        return None;
    }
    let url = callback.join(target).ok()?;
    if url.path() != callback.path()
        || url.origin() != callback.origin()
        || url.fragment().is_some()
    {
        return None;
    }
    let mut code = None;
    let mut received_state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "state" => {
                if received_state.replace(value.into_owned()).is_some() {
                    return None;
                }
            }
            "code" if code.replace(Zeroizing::new(value.into_owned())).is_some() => {
                return None;
            }
            _ => {}
        }
    }
    if received_state.as_deref() != Some(state) {
        return None;
    }
    code.filter(|c| !c.is_empty() && c.len() <= 4096)
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

#[cfg(all(test, feature = "http"))]
mod runtime_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn loopback_requires_state_and_exchanges_s256_before_storing_key() {
        let login = OpenRouterLogin::bind().await.unwrap();
        let authorize = login.authorization_url().clone();
        let params: std::collections::BTreeMap<_, _> = authorize
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let mut callback = Url::parse(&params["callback_url"]).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Url::parse(&format!(
            "http://{}/exchange",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let challenge = params["code_challenge"].clone();
        let exchange = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..end]);
                    let len = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        let body: serde_json::Value =
                            serde_json::from_slice(&bytes[end + 4..]).unwrap();
                        assert_eq!(body["code"], "authorization-code");
                        assert_eq!(body["code_challenge_method"], "S256");
                        assert_eq!(
                            PkcePair::challenge_for(body["code_verifier"].as_str().unwrap()),
                            challenge
                        );
                        break;
                    }
                }
            }
            let body = r#"{"key":"ZEUS-CANARY-oauth-key"}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let store = std::sync::Arc::new(crate::MemorySecretStore::default());
        let saved = store.clone();
        let finish = tokio::spawn(async move {
            login
                .finish_at(saved.as_ref(), &endpoint, std::time::Duration::from_secs(5))
                .await
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        callback
            .query_pairs_mut()
            .append_pair("state", "wrong")
            .append_pair("code", "authorization-code");
        assert_eq!(
            client.get(callback.clone()).send().await.unwrap().status(),
            400
        );
        callback.set_query(None);
        callback
            .query_pairs_mut()
            .append_pair("state", &params["state"])
            .append_pair("code", "authorization-code");
        let response = client.get(callback).send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert!(!response.text().await.unwrap().contains("ZEUS-CANARY"));
        finish.await.unwrap().unwrap();
        exchange.await.unwrap();
        use crate::SecretStore;
        assert_eq!(
            store
                .get(&crate::secrets::SecretKey {
                    provider: crate::ProviderKind::OpenrouterOauth,
                    purpose: "api_key".into()
                })
                .unwrap()
                .unwrap()
                .as_str(),
            "ZEUS-CANARY-oauth-key"
        );
    }
    #[test]
    fn rejects_duplicate_state_host_injection_and_callback_credentials() {
        let callback = Url::parse("http://127.0.0.1:54321/callback").unwrap();
        for request in [
            "GET /callback?code=x&state=s&state=s HTTP/1.1\r\nHost: 127.0.0.1:54321\r\n\r\n",
            "GET /callback?code=x&state=s HTTP/1.1\r\nHost: evil.example\r\n\r\n",
            "GET //evil.example/callback?code=x&state=s HTTP/1.1\r\nHost: 127.0.0.1:54321\r\n\r\n",
        ] {
            assert!(callback_code(request, &callback, "s").is_none())
        }
        assert!(
            openrouter_auth_url(
                &Url::parse("http://user:password@127.0.0.1:123/callback").unwrap(),
                "challenge"
            )
            .is_err()
        );
    }
}
