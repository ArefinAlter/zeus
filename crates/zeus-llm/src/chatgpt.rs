//! Sign in with ChatGPT for open-source local apps: PKCE, OIDC identity, profiles and refresh.
//! Only the documented public OAuth and Responses endpoints are used; other applications' tokens are never read.

use crate::http_provider::{
    HttpDialect, HttpProvider, HttpProviderConfig, Pricing, read_json, unavailable,
};
use crate::pkce::PkcePair;
use crate::secrets::SecretKey;
use crate::{LlmError, LlmProvider, LlmRequest, LlmResponse, ProviderKind, SecretStore};
use async_trait::async_trait;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const INDEX: TableDefinition<&str, &[u8]> = TableDefinition::new("chatgpt_profiles_v1");

/// A validated account registration, safe to display in the trusted account picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// Server-issued registration id; never `dynamic_agent_client`.
    pub client_id: String,
    /// Validated OIDC subject; absent until initial exchange completes.
    pub subject: Option<String>,
    /// Display email from the verified identity token, never used as identity.
    pub email: Option<String>,
    /// Whether the saved grant permits plan inference.
    pub plan_enabled: bool,
}
#[derive(Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    generations: BTreeMap<String, BTreeMap<String, usize>>,
    host_id: String,
    accounts: BTreeMap<String, Account>,
    active: Option<String>,
}
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct Profile {
    client_id: String,
    subject: String,
    email: Option<String>,
    issuer: String,
    access_token: String,
    refresh_token: String,
    id_token: String,
    expires_at: i64,
    #[zeroize(skip)]
    scopes: BTreeSet<String>,
}
impl std::fmt::Debug for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChatgptProfile([REDACTED])")
    }
}
#[derive(Clone)]
struct Endpoints {
    issuer: String,
    authorize: Url,
    token: Url,
    discovery: Url,
    models: Url,
    responses: Url,
}
impl Endpoints {
    fn production() -> Self {
        Self {
            issuer: ISSUER.into(),
            authorize: Url::parse("https://auth.openai.com/api/accounts/authorize")
                .expect("constant URL"),
            token: Url::parse("https://auth.openai.com/api/accounts/oauth/token")
                .expect("constant URL"),
            discovery: Url::parse("https://auth.openai.com/.well-known/openid-configuration")
                .expect("constant URL"),
            models: Url::parse("https://api.openai.com/v1/models").expect("constant URL"),
            responses: Url::parse("https://api.openai.com/v1/responses").expect("constant URL"),
        }
    }
}

/// Single-owner profile metadata; token bundles live exclusively in the supplied secret store.
/// Refreshes are serialized so a rotating refresh token is never concurrently reused by this daemon.
pub struct ChatgptAccounts {
    db: Database,
    secrets: Arc<dyn SecretStore>,
    client: reqwest::Client,
    endpoints: Endpoints,
    gate: Mutex<()>,
}
impl ChatgptAccounts {
    /// Open persistent installation/account metadata without accessing the network or keyring.
    pub fn open(path: &Path, secrets: Arc<dyn SecretStore>) -> Result<Arc<Self>, LlmError> {
        Self::open_at(path, secrets, Endpoints::production())
    }
    fn open_at(
        path: &Path,
        secrets: Arc<dyn SecretStore>,
        endpoints: Endpoints,
    ) -> Result<Arc<Self>, LlmError> {
        if path.is_symlink() {
            return Err(unavailable("unsafe account metadata path"));
        }
        let db = Database::create(path).map_err(|_| unavailable("account metadata unavailable"))?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| unavailable("cannot configure account client"))?;
        let accounts = Arc::new(Self {
            db,
            secrets,
            client,
            endpoints,
            gate: Mutex::new(()),
        });
        let mut tx = accounts
            .db
            .begin_write()
            .map_err(|_| unavailable("account metadata unavailable"))?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| unavailable("account metadata unavailable"))?;
        {
            let mut table = tx
                .open_table(INDEX)
                .map_err(|_| unavailable("account metadata unavailable"))?;
            if table
                .get("index")
                .map_err(|_| unavailable("account metadata unavailable"))?
                .is_none()
            {
                let bytes = serde_json::to_vec(&Index {
                    generations: BTreeMap::new(),
                    host_id: new_host_id(),
                    accounts: BTreeMap::new(),
                    active: None,
                })
                .map_err(|_| LlmError::SchemaViolation)?;
                table
                    .insert("index", bytes.as_slice())
                    .map_err(|_| unavailable("account metadata unavailable"))?;
            }
        }
        tx.commit()
            .map_err(|_| unavailable("account metadata unavailable"))?;
        Ok(accounts)
    }
    fn index(&self) -> Result<Index, LlmError> {
        let tx = self
            .db
            .begin_read()
            .map_err(|_| unavailable("account metadata unavailable"))?;
        let table = tx
            .open_table(INDEX)
            .map_err(|_| unavailable("account metadata unavailable"))?;
        let bytes = table
            .get("index")
            .map_err(|_| unavailable("account metadata unavailable"))?
            .ok_or(LlmError::NotConfigured)?;
        serde_json::from_slice(bytes.value()).map_err(|_| unavailable("invalid account metadata"))
    }
    fn mutate(&self, f: impl FnOnce(&mut Index) -> Result<(), LlmError>) -> Result<(), LlmError> {
        let mut tx = self
            .db
            .begin_write()
            .map_err(|_| unavailable("account metadata unavailable"))?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| unavailable("account metadata unavailable"))?;
        {
            let mut table = tx
                .open_table(INDEX)
                .map_err(|_| unavailable("account metadata unavailable"))?;
            let bytes = table
                .get("index")
                .map_err(|_| unavailable("account metadata unavailable"))?
                .ok_or(LlmError::NotConfigured)?
                .value()
                .to_vec();
            let mut index: Index =
                serde_json::from_slice(&bytes).map_err(|_| LlmError::SchemaViolation)?;
            f(&mut index)?;
            let bytes = serde_json::to_vec(&index).map_err(|_| LlmError::SchemaViolation)?;
            table
                .insert("index", bytes.as_slice())
                .map_err(|_| unavailable("account metadata unavailable"))?;
        }
        tx.commit()
            .map_err(|_| unavailable("account metadata unavailable"))
    }
    fn key(id: &str) -> Result<SecretKey, LlmError> {
        if !valid_client_id(id) {
            return Err(LlmError::AuthFailed);
        }
        Ok(SecretKey {
            provider: ProviderKind::ChatgptOauth,
            purpose: format!("profile/{id}"),
        })
    }
    fn profile(&self, id: &str) -> Result<Profile, LlmError> {
        use base64::Engine;
        let raw = self
            .secrets
            .get(&Self::key(id)?)?
            .ok_or(LlmError::NotConfigured)?;
        let head: BundleHead = serde_json::from_str(&raw).map_err(|_| LlmError::AuthFailed)?;
        if head.chunks == 0
            || head.chunks > 256
            || head.generation.len() != 64
            || !head.generation.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(LlmError::AuthFailed);
        }
        let mut encoded = Zeroizing::new(String::new());
        for index in 0..head.chunks {
            let chunk = self
                .secrets
                .get(&bundle_key(id, &head.generation, index)?)?
                .ok_or(LlmError::AuthFailed)?;
            if chunk.len() > 1024 {
                return Err(LlmError::AuthFailed);
            }
            encoded.push_str(&chunk);
        }
        let bytes = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(encoded.as_bytes())
                .map_err(|_| LlmError::AuthFailed)?,
        );
        let profile: Profile = serde_json::from_slice(&bytes).map_err(|_| LlmError::AuthFailed)?;
        if profile.client_id != id || profile.issuer != self.endpoints.issuer {
            return Err(LlmError::AuthFailed);
        }
        Ok(profile)
    }
    fn save(&self, profile: &Profile, select: bool) -> Result<(), LlmError> {
        use base64::Engine;
        if self
            .index()?
            .accounts
            .get(&profile.client_id)
            .and_then(|a| a.subject.as_deref())
            .is_some_and(|s| s != profile.subject)
        {
            return Err(LlmError::AuthFailed);
        }
        let bytes =
            Zeroizing::new(serde_json::to_vec(profile).map_err(|_| LlmError::SchemaViolation)?);
        if bytes.len() > 128 * 1024 {
            return Err(LlmError::SchemaViolation);
        }
        let encoded =
            Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(bytes.as_slice()));
        let generation = hex::encode(rand::random::<[u8; 32]>());
        let chunks = encoded.len().div_ceil(1024);
        // A non-secret cleanup intent precedes bounded keyring entries. One atomic head swap
        // commits the token generation without exceeding platform credential-size limits.
        self.mutate(|index| {
            index
                .generations
                .entry(profile.client_id.clone())
                .or_default()
                .insert(generation.clone(), chunks);
            Ok(())
        })?;
        for (index, chunk) in encoded.as_bytes().chunks(1024).enumerate() {
            self.secrets.set(
                &bundle_key(&profile.client_id, &generation, index)?,
                std::str::from_utf8(chunk).map_err(|_| LlmError::SchemaViolation)?,
            )?;
        }
        let head = serde_json::to_string(&BundleHead {
            generation: generation.clone(),
            chunks,
        })
        .map_err(|_| LlmError::SchemaViolation)?;
        self.secrets.set(&Self::key(&profile.client_id)?, &head)?;
        self.mutate(|index| {
            index.accounts.insert(
                profile.client_id.clone(),
                Account {
                    client_id: profile.client_id.clone(),
                    subject: Some(profile.subject.clone()),
                    email: profile.email.clone(),
                    plan_enabled: profile.scopes.contains("chatgpt.tokens.use.direct")
                        && profile.scopes.contains("resource.invoke"),
                },
            );
            if select {
                index.active = Some(profile.client_id.clone())
            }
            Ok(())
        })?;
        let old = self
            .index()?
            .generations
            .get(&profile.client_id)
            .cloned()
            .unwrap_or_default();
        for (prior, count) in old {
            if prior == generation {
                continue;
            }
            let mut removed = true;
            for index in 0..count {
                if self
                    .secrets
                    .delete(&bundle_key(&profile.client_id, &prior, index)?)
                    .is_err()
                {
                    removed = false
                }
            }
            if removed {
                self.mutate(|index| {
                    if let Some(gens) = index.generations.get_mut(&profile.client_id) {
                        gens.remove(&prior);
                    }
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
    /// List verified and pending registrations, without credential material.
    pub fn accounts(&self) -> Result<Vec<Account>, LlmError> {
        Ok(self.index()?.accounts.into_values().collect())
    }
    /// The selected registration, if any.
    pub fn active(&self) -> Result<Option<String>, LlmError> {
        Ok(self.index()?.active)
    }
    /// Select an existing identity only when its saved token grant permits inference.
    pub async fn select(&self, id: &str) -> Result<(), LlmError> {
        let _guard = self.gate.lock().await;
        let profile = self.profile(id)?;
        require_plan(&profile)?;
        self.mutate(|i| {
            if !i.accounts.contains_key(id) {
                return Err(LlmError::NotConfigured);
            }
            i.active = Some(id.into());
            Ok(())
        })
    }
    /// Start a new registration or reauthorize an existing registration; returns a browser URL and listener.
    pub async fn begin(self: &Arc<Self>, account: Option<&str>) -> Result<ChatgptLogin, LlmError> {
        let index = self.index()?;
        let selected = account
            .map(|id| {
                index
                    .accounts
                    .get(id)
                    .cloned()
                    .ok_or(LlmError::NotConfigured)
            })
            .transpose()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| unavailable("cannot bind sign-in callback"))?;
        let port = listener
            .local_addr()
            .map_err(|_| unavailable("cannot bind sign-in callback"))?
            .port();
        let callback = Url::parse(&format!("http://127.0.0.1:{port}/auth/callback"))
            .map_err(|_| LlmError::SchemaViolation)?;
        let pair = PkcePair::generate();
        let state = Zeroizing::new(PkcePair::generate().verifier().to_owned());
        let nonce = Zeroizing::new(PkcePair::generate().verifier().to_owned());
        let mut authorize = self.endpoints.authorize.clone();
        let mut q = authorize.query_pairs_mut();
        q.append_pair(
            "client_id",
            selected
                .as_ref()
                .map_or("dynamic_agent_client", |a| a.client_id.as_str()),
        )
        .append_pair("ext_agent_host_id", &index.host_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", callback.as_str())
        .append_pair(
            "scope",
            "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
        )
        .append_pair("resource", RESOURCE)
        .append_pair("state", &state)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", pair.challenge());
        if selected.is_none() {
            q.append_pair("agent_name_hint", "Zeus");
        }
        drop(q);
        Ok(ChatgptLogin {
            accounts: self.clone(),
            listener,
            pair,
            state,
            nonce,
            callback,
            authorize,
            selected,
        })
    }
    async fn discovery(&self) -> Result<Value, LlmError> {
        let response = self
            .client
            .get(self.endpoints.discovery.clone())
            .send()
            .await
            .map_err(|_| unavailable("identity discovery failed"))?;
        let value = read_json(response, 256 * 1024).await?;
        if value["issuer"].as_str() != Some(&self.endpoints.issuer) {
            return Err(LlmError::AuthFailed);
        }
        Ok(value)
    }
    fn issuer_url(&self, value: &Value) -> Result<Url, LlmError> {
        let url = Url::parse(value.as_str().ok_or(LlmError::AuthFailed)?)
            .map_err(|_| LlmError::AuthFailed)?;
        // Metadata cannot redirect credential-bearing requests or JWKS retrieval to a different authority.
        if url.origin() != self.endpoints.discovery.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.query().is_some()
        {
            return Err(LlmError::AuthFailed);
        }
        Ok(url)
    }
    async fn identity(
        &self,
        token: &str,
        client_id: &str,
        nonce: Option<&str>,
        subject: Option<&str>,
    ) -> Result<Claims, LlmError> {
        let discovery = self.discovery().await?;
        let jwks = self.issuer_url(&discovery["jwks_uri"])?;
        let response = self
            .client
            .get(jwks)
            .send()
            .await
            .map_err(|_| unavailable("identity keys unavailable"))?;
        let keys: jsonwebtoken::jwk::JwkSet =
            serde_json::from_value(read_json(response, 256 * 1024).await?)
                .map_err(|_| LlmError::AuthFailed)?;
        validate_identity(
            token,
            &keys,
            &self.endpoints.issuer,
            client_id,
            nonce,
            subject,
        )
    }
    async fn tokens(&self, parameters: &[(&str, &str)]) -> Result<Value, LlmError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(parameters.iter().copied())
            .finish();
        let response = self
            .client
            .post(self.endpoints.token.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|_| unavailable("sign-in exchange failed"))?;
        read_json(response, 64 * 1024).await
    }
    async fn access_token(&self, id: &str) -> Result<Zeroizing<String>, LlmError> {
        let _guard = self.gate.lock().await;
        let mut profile = self.profile(id)?;
        require_plan(&profile)?;
        if profile.expires_at <= time::OffsetDateTime::now_utc().unix_timestamp() + 60 {
            if profile.refresh_token.is_empty() {
                return Err(LlmError::AuthFailed);
            }
            let value = self
                .tokens(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", id),
                    ("refresh_token", &profile.refresh_token),
                    ("resource", RESOURCE),
                ])
                .await?;
            if let Some(id_token) = value["id_token"].as_str() {
                let identity = self
                    .identity(id_token, id, None, Some(&profile.subject))
                    .await?;
                profile.id_token = id_token.into();
                profile.email = identity.email;
            }
            update_tokens(&mut profile, &value, false)?;
            require_plan(&profile)?;
            self.save(&profile, false)?;
        }
        Ok(Zeroizing::new(profile.access_token.clone()))
    }
    /// Fetch the account-specific model picker only on explicit user request.
    pub async fn models(&self, id: &str) -> Result<Vec<ModelChoice>, LlmError> {
        let token = self.access_token(id).await?;
        let response = self
            .client
            .get(self.endpoints.models.clone())
            .bearer_auth(token.as_str())
            .send()
            .await
            .map_err(|_| unavailable("model catalogue unavailable"))?;
        let value = read_json(response, 1024 * 1024).await?;
        let mut models = Vec::new();
        for model in value["models"]
            .as_array()
            .ok_or(LlmError::SchemaViolation)?
        {
            if model["visibility"] == "list" {
                let slug = model["slug"].as_str().ok_or(LlmError::SchemaViolation)?;
                let name = model["display_name"]
                    .as_str()
                    .ok_or(LlmError::SchemaViolation)?;
                if slug.len() > 200 || name.len() > 300 {
                    return Err(LlmError::SchemaViolation);
                }
                models.push(ModelChoice {
                    slug: slug.into(),
                    display_name: name.into(),
                });
            }
        }
        Ok(models)
    }
    /// Stop using a registration and attempt remote refresh-token revocation. Returns whether remote revocation was confirmed.
    pub async fn sign_out(&self, id: &str) -> Result<bool, LlmError> {
        let _guard = self.gate.lock().await;
        let profile = match self.profile(id) {
            Ok(profile) => Some(profile),
            Err(LlmError::NotConfigured | LlmError::AuthFailed) => None,
            Err(error) => return Err(error),
        };
        let revoked = async {
            let profile = profile.as_ref().ok_or(LlmError::NotConfigured)?;
            let discovery = self.discovery().await?;
            let endpoint = self.issuer_url(&discovery["revocation_endpoint"])?;
            let body = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs([
                    ("token", profile.refresh_token.as_str()),
                    ("token_type_hint", "refresh_token"),
                    ("client_id", id),
                ])
                .finish();
            self.client
                .post(endpoint)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body)
                .send()
                .await
                .map(|r| r.status() == 200)
                .map_err(|_| unavailable("revocation unavailable"))
        }
        .await
        .unwrap_or(false);
        self.secrets.delete(&Self::key(id)?)?;
        // Disable the account as soon as its committed token head is removed. If a platform
        // deletion fails later, the retained generation intents make a subsequent sign-out retryable.
        self.mutate(|index| {
            if index.active.as_deref() == Some(id) {
                index.active = None;
            }
            if let Some(account) = index.accounts.get_mut(id) {
                account.plan_enabled = false;
            }
            Ok(())
        })?;
        let generations = self
            .index()?
            .generations
            .get(id)
            .cloned()
            .unwrap_or_default();
        for (generation, chunks) in generations {
            for index in 0..chunks {
                self.secrets.delete(&bundle_key(id, &generation, index)?)?;
            }
        }
        self.mutate(|index| {
            index.generations.remove(id);
            if index.active.as_deref() == Some(id) {
                index.active = None;
            }
            if let Some(account) = index.accounts.get_mut(id) {
                account.plan_enabled = false;
            }
            Ok(())
        })?;
        Ok(revoked)
    }
}
/// One account's visible model entry, returned in server order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelChoice {
    /// Identifier sent to the Responses endpoint.
    pub slug: String,
    /// Human-readable name.
    pub display_name: String,
}

/// Single-use browser sign-in; dropping it cancels the listener without altering the active account.
pub struct ChatgptLogin {
    accounts: Arc<ChatgptAccounts>,
    listener: tokio::net::TcpListener,
    pair: PkcePair,
    state: Zeroizing<String>,
    nonce: Zeroizing<String>,
    callback: Url,
    authorize: Url,
    selected: Option<Account>,
}
impl std::fmt::Debug for ChatgptLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChatgptLogin([REDACTED])")
    }
}
impl ChatgptLogin {
    /// Open this in the user's browser only after an explicit sign-in action. Do not log it.
    pub fn authorization_url(&self) -> &Url {
        &self.authorize
    }
    /// Finish within five minutes; save the issued registration before exchange, validate identity before switching accounts.
    pub async fn finish(self) -> Result<Account, LlmError> {
        tokio::time::timeout(Duration::from_secs(300), self.finish_inner())
            .await
            .map_err(|_| unavailable("sign-in expired"))?
    }
    async fn finish_inner(self) -> Result<Account, LlmError> {
        loop {
            let (mut socket, peer) = self
                .listener
                .accept()
                .await
                .map_err(|_| unavailable("sign-in listener failed"))?;
            if !peer.ip().is_loopback() {
                continue;
            }
            let mut bytes = Zeroizing::new(Vec::new());
            let read = tokio::time::timeout(Duration::from_secs(3), async {
                let mut buf = [0; 1024];
                loop {
                    let n = socket.read(&mut buf).await?;
                    if n == 0 || bytes.len() + n > 8192 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.windows(4).any(|p| p == b"\r\n\r\n") {
                        break;
                    }
                }
                Ok::<(), std::io::Error>(())
            })
            .await;
            if !matches!(read, Ok(Ok(()))) {
                continue;
            }
            let params = callback_parameters(
                std::str::from_utf8(&bytes).unwrap_or(""),
                &self.callback,
                &self.state,
            );
            let Some(params) = params else {
                let _=socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                continue;
            };
            if params.contains_key("error") {
                return Err(LlmError::AuthFailed);
            }
            let code = params
                .get("code")
                .filter(|c| !c.is_empty() && c.len() <= 4096)
                .ok_or(LlmError::AuthFailed)?;
            let id = match &self.selected {
                Some(selected) => {
                    if params
                        .get("client_id")
                        .is_some_and(|id| id != &selected.client_id)
                    {
                        return Err(LlmError::AuthFailed);
                    }
                    selected.client_id.clone()
                }
                None => params
                    .get("client_id")
                    .filter(|id| valid_client_id(id))
                    .cloned()
                    .ok_or(LlmError::AuthFailed)?,
            };
            let _guard = self.accounts.gate.lock().await;
            self.accounts.mutate(|index| {
                index.accounts.entry(id.clone()).or_insert(Account {
                    client_id: id.clone(),
                    subject: None,
                    email: None,
                    plan_enabled: false,
                });
                Ok(())
            })?;
            let value = self
                .accounts
                .tokens(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", &id),
                    ("code", code),
                    ("code_verifier", self.pair.verifier()),
                    ("redirect_uri", self.callback.as_str()),
                    ("resource", RESOURCE),
                ])
                .await?;
            let token = value["id_token"].as_str().ok_or(LlmError::AuthFailed)?;
            let identity = self
                .accounts
                .identity(
                    token,
                    &id,
                    Some(&self.nonce),
                    self.selected.as_ref().and_then(|s| s.subject.as_deref()),
                )
                .await?;
            let mut profile = Profile {
                client_id: id.clone(),
                subject: identity.sub,
                email: identity.email,
                issuer: identity.iss,
                access_token: String::new(),
                refresh_token: String::new(),
                id_token: token.into(),
                expires_at: 0,
                scopes: BTreeSet::new(),
            };
            update_tokens(&mut profile, &value, true)?;
            self.accounts.save(&profile, true)?;
            let account = self
                .accounts
                .index()?
                .accounts
                .get(&id)
                .cloned()
                .ok_or(LlmError::AuthFailed)?;
            let body = if account.plan_enabled {
                "Zeus is connected. You can close this tab."
            } else {
                "Your account is connected. ChatGPT plan use was not granted."
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            return Ok(account);
        }
    }
}
fn callback_parameters(
    request: &str,
    callback: &Url,
    state: &str,
) -> Option<BTreeMap<String, String>> {
    if !request.contains("\r\n\r\n") {
        return None;
    }
    let mut lines = request.split("\r\n");
    let parts: Vec<_> = lines.next()?.split(' ').collect();
    if parts.len() != 3
        || parts[0] != "GET"
        || parts[2] != "HTTP/1.1"
        || !parts[1].starts_with('/')
        || parts[1].starts_with("//")
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
    }
    if host? != format!("127.0.0.1:{}", callback.port()?) {
        return None;
    }
    let url = callback.join(parts[1]).ok()?;
    if url.origin() != callback.origin()
        || url.path() != callback.path()
        || url.fragment().is_some()
    {
        return None;
    }
    let mut params = BTreeMap::new();
    for (k, v) in url.query_pairs() {
        if params.insert(k.into_owned(), v.into_owned()).is_some() {
            return None;
        }
    }
    if params.get("state").map(String::as_str) != Some(state) {
        return None;
    }
    Some(params)
}
fn valid_client_id(id: &str) -> bool {
    id.starts_with("oaiapp_")
        && id.len() <= 200
        && id.len() > 7
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn new_host_id() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let h = hex::encode(bytes);
    format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}
fn require_plan(profile: &Profile) -> Result<(), LlmError> {
    if profile.scopes.contains("chatgpt.tokens.use.direct")
        && profile.scopes.contains("resource.invoke")
    {
        Ok(())
    } else {
        Err(LlmError::DisabledByPolicy("chatgpt_plan_not_granted"))
    }
}
fn update_tokens(profile: &mut Profile, value: &Value, initial: bool) -> Result<(), LlmError> {
    if value["token_type"]
        .as_str()
        .is_none_or(|v| !v.eq_ignore_ascii_case("bearer"))
    {
        return Err(LlmError::AuthFailed);
    }
    let access = value["access_token"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 32768)
        .ok_or(LlmError::AuthFailed)?;
    let expiry = value["expires_in"]
        .as_u64()
        .filter(|v| *v > 0 && *v <= 86400 * 30)
        .ok_or(LlmError::AuthFailed)?;
    profile.access_token = access.into();
    profile.expires_at = time::OffsetDateTime::now_utc()
        .unix_timestamp()
        .checked_add(expiry as i64)
        .ok_or(LlmError::AuthFailed)?;
    if let Some(refresh) = value["refresh_token"].as_str() {
        if refresh.is_empty() || refresh.len() > 32768 {
            return Err(LlmError::AuthFailed);
        }
        profile.refresh_token = refresh.into()
    } else if initial {
        return Err(LlmError::AuthFailed);
    }
    if let Some(scopes) = value["scope"].as_str() {
        profile.scopes = scopes.split_ascii_whitespace().map(str::to_owned).collect()
    } else if initial {
        return Err(LlmError::AuthFailed);
    }
    Ok(())
}
#[derive(Clone, Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: Value,
    exp: u64,
    iat: u64,
    nonce: Option<String>,
    azp: Option<String>,
    email: Option<String>,
}
fn validate_identity(
    token: &str,
    keys: &jsonwebtoken::jwk::JwkSet,
    issuer: &str,
    client_id: &str,
    nonce: Option<&str>,
    subject: Option<&str>,
) -> Result<Claims, LlmError> {
    use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
    if token.len() > 32768 || keys.keys.len() > 64 {
        return Err(LlmError::AuthFailed);
    }
    let header = decode_header(token).map_err(|_| LlmError::AuthFailed)?;
    if !matches!(
        header.alg,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::ES256
            | Algorithm::ES384
            | Algorithm::EdDSA
    ) {
        return Err(LlmError::AuthFailed);
    }
    let kid = header.kid.ok_or(LlmError::AuthFailed)?;
    let key = keys.find(&kid).ok_or(LlmError::AuthFailed)?;
    if key
        .common
        .public_key_use
        .as_ref()
        .is_some_and(|v| *v != jsonwebtoken::jwk::PublicKeyUse::Signature)
    {
        return Err(LlmError::AuthFailed);
    }
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["iss", "sub", "aud", "exp", "iat"]);
    validation.validate_nbf = true;
    validation.leeway = 60;
    let claims = decode::<Claims>(
        token,
        &DecodingKey::from_jwk(key).map_err(|_| LlmError::AuthFailed)?,
        &validation,
    )
    .map_err(|_| LlmError::AuthFailed)?
    .claims;
    let now = time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64;
    if claims.sub.is_empty()
        || claims.exp <= now
        || claims.iat > now + 60
        || claims.azp.as_deref().is_some_and(|v| v != client_id)
        || claims
            .aud
            .as_array()
            .is_some_and(|v| v.len() > 1 && claims.azp.as_deref() != Some(client_id))
        || nonce.is_some_and(|n| claims.nonce.as_deref() != Some(n))
        || subject.is_some_and(|s| claims.sub != s)
    {
        return Err(LlmError::AuthFailed);
    }
    Ok(claims)
}

/// Responses adapter for one verified ChatGPT registration; requires explicit model/pricing selection.
pub struct ChatgptProvider {
    accounts: Arc<ChatgptAccounts>,
    client_id: String,
    http: HttpProvider,
}
impl ChatgptProvider {
    /// Configure without calling the network. Model discovery is a separate explicit user action.
    pub fn new(
        accounts: Arc<ChatgptAccounts>,
        client_id: String,
        model: String,
        pricing: Pricing,
        timeout_seconds: u64,
    ) -> Result<Self, LlmError> {
        if !valid_client_id(&client_id) {
            return Err(LlmError::NotConfigured);
        }
        let config = HttpProviderConfig {
            kind: ProviderKind::ApiKeyOpenaiCompatible,
            endpoint: accounts.endpoints.responses.clone(),
            dialect: HttpDialect::OpenAiResponses,
            model,
            pricing: Some(pricing),
            timeout_seconds,
        };
        let http = HttpProvider::new(config, accounts.secrets.clone())?;
        Ok(Self {
            accounts,
            client_id,
            http,
        })
    }
}
#[async_trait]
impl LlmProvider for ChatgptProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::ChatgptOauth
    }
    fn is_configured(&self) -> bool {
        self.accounts
            .profile(&self.client_id)
            .is_ok_and(|p| require_plan(&p).is_ok())
    }
    fn cost_ceiling(&self, request: &LlmRequest) -> Result<u64, LlmError> {
        self.http.quote(request)
    }
    async fn complete(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
        request.privacy.check_provider(self.kind())?;
        let token = self.accounts.access_token(&self.client_id).await?;
        let mut response = self
            .http
            .complete_authorized(request, Some(token), true)
            .await?;
        response.provider = self.kind();
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[derive(Default)]
    struct FaultSecrets {
        inner: crate::MemorySecretStore,
        fail_head: std::sync::atomic::AtomicBool,
        fail_delete: std::sync::atomic::AtomicBool,
    }
    impl SecretStore for FaultSecrets {
        fn get(&self, key: &SecretKey) -> Result<Option<Zeroizing<String>>, LlmError> {
            self.inner.get(key)
        }
        fn set(&self, key: &SecretKey, value: &str) -> Result<(), LlmError> {
            assert!(
                value.len() <= 1024,
                "credential entry exceeds portable bound"
            );
            if key.purpose == "profile/oaiapp_test" && self.fail_head.swap(false, Ordering::SeqCst)
            {
                return Err(LlmError::SecretStore("injected head failure".into()));
            }
            self.inner.set(key, value)
        }
        fn delete(&self, key: &SecretKey) -> Result<(), LlmError> {
            if key.purpose != "profile/oaiapp_test"
                && self.fail_delete.swap(false, Ordering::SeqCst)
            {
                return Err(LlmError::SecretStore("injected delete failure".into()));
            }
            self.inner.delete(key)
        }
    }
    #[tokio::test]
    async fn credential_head_commit_preserves_prior_generation_and_cleanup_is_retryable() {
        let root = tempfile::tempdir().unwrap();
        let secrets = Arc::new(FaultSecrets::default());
        let accounts =
            ChatgptAccounts::open(&root.path().join("profiles"), secrets.clone()).unwrap();
        let mut profile = Profile {
            client_id: "oaiapp_test".into(),
            subject: "subject".into(),
            email: None,
            issuer: ISSUER.into(),
            access_token: "prior-access".into(),
            refresh_token: "prior-refresh".repeat(1000),
            id_token: "identity".into(),
            expires_at: i64::MAX,
            scopes: ["chatgpt.tokens.use.direct".into(), "resource.invoke".into()]
                .into_iter()
                .collect(),
        };
        accounts.save(&profile, true).unwrap();
        secrets.fail_head.store(true, Ordering::SeqCst);
        profile.access_token = "next-access".into();
        assert!(accounts.save(&profile, false).is_err());
        assert_eq!(
            accounts.profile("oaiapp_test").unwrap().access_token,
            "prior-access"
        );
        accounts.save(&profile, false).unwrap();
        assert_eq!(
            accounts.profile("oaiapp_test").unwrap().access_token,
            "next-access"
        );
        assert_eq!(
            accounts.index().unwrap().generations["oaiapp_test"].len(),
            1
        );
        // Simulate interruption after removing the committed head; do not call any network.
        secrets
            .delete(&ChatgptAccounts::key("oaiapp_test").unwrap())
            .unwrap();
        secrets.fail_delete.store(true, Ordering::SeqCst);
        assert!(accounts.sign_out("oaiapp_test").await.is_err());
        assert_eq!(accounts.active().unwrap(), None);
        assert!(!accounts.sign_out("oaiapp_test").await.unwrap());
        assert!(
            !accounts
                .index()
                .unwrap()
                .generations
                .contains_key("oaiapp_test")
        );
    }
    fn token(key: &SigningKey, issuer: &str, audience: &str, nonce: &str) -> String {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let header = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({"alg":"EdDSA","kid":"test-only"})).unwrap());
        let claims=URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"iss":issuer,"sub":"account-subject","aud":audience,"exp":now+3600,"iat":now,"nonce":nonce,"email":"person@example.test"})).unwrap());
        let input = format!("{header}.{claims}");
        format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()).to_bytes())
        )
    }
    fn keys(key: &SigningKey) -> Value {
        json!({"keys":[{"kty":"OKP","crv":"Ed25519","use":"sig","alg":"EdDSA","kid":"test-only","x":URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes())}]})
    }
    #[test]
    fn signed_identity_requires_issuer_client_nonce_and_original_subject() {
        let key = SigningKey::from_bytes(&[17; 32]);
        let keys: jsonwebtoken::jwk::JwkSet = serde_json::from_value(keys(&key)).unwrap();
        let token = token(&key, ISSUER, "oaiapp_example", "nonce");
        assert!(
            validate_identity(
                &token,
                &keys,
                ISSUER,
                "oaiapp_example",
                Some("nonce"),
                Some("account-subject")
            )
            .is_ok()
        );
        for (issuer, client, nonce, subject) in [
            (
                "https://evil.test",
                "oaiapp_example",
                "nonce",
                "account-subject",
            ),
            (ISSUER, "oaiapp_other", "nonce", "account-subject"),
            (ISSUER, "oaiapp_example", "wrong", "account-subject"),
            (ISSUER, "oaiapp_example", "nonce", "other"),
        ] {
            assert!(
                validate_identity(&token, &keys, issuer, client, Some(nonce), Some(subject))
                    .is_err()
            )
        }
        let mut forged = token.clone().into_bytes();
        let index = forged.len() - 3;
        forged[index] = if forged[index] == b'A' { b'B' } else { b'A' };
        assert!(
            validate_identity(
                std::str::from_utf8(&forged).unwrap(),
                &keys,
                ISSUER,
                "oaiapp_example",
                Some("nonce"),
                None
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn dynamic_login_refresh_stream_and_revocation_are_bound_to_one_profile() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let endpoint = |path: &str| Url::parse(&format!("{base}{path}")).unwrap();
        let endpoints = Endpoints {
            issuer: base.clone(),
            authorize: endpoint("/authorize"),
            token: endpoint("/token"),
            discovery: endpoint("/discovery"),
            models: endpoint("/models"),
            responses: endpoint("/responses"),
        };
        let root = tempfile::tempdir().unwrap();
        let secrets = Arc::new(crate::MemorySecretStore::default());
        let accounts =
            ChatgptAccounts::open_at(&root.path().join("profiles"), secrets.clone(), endpoints)
                .unwrap();
        let login = accounts.begin(None).await.unwrap();
        let query: BTreeMap<String, String> = login
            .authorization_url()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(query["client_id"], "dynamic_agent_client");
        assert_eq!(query["agent_name_hint"], "Zeus");
        assert!(query["ext_agent_host_id"].starts_with("urn:uuid:"));
        let expected_nonce = query["nonce"].clone();
        let expected_challenge = query["code_challenge"].clone();
        let expected_callback = query["redirect_uri"].clone();
        let refreshes = Arc::new(AtomicUsize::new(0));
        let count = refreshes.clone();
        let server_base = base.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                let (path, body) = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        panic!("unexpected closed request")
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..end]);
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|n| n.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + len {
                            break (
                                head.lines()
                                    .next()
                                    .unwrap()
                                    .split(' ')
                                    .nth(1)
                                    .unwrap()
                                    .to_owned(),
                                bytes[end + 4..].to_vec(),
                            );
                        }
                    }
                };
                let key = SigningKey::from_bytes(&[18; 32]);
                let (content_type,response)=match path.as_str(){
                    "/discovery"=>("application/json",json!({"issuer":server_base,"jwks_uri":format!("{server_base}/keys"),"revocation_endpoint":format!("{server_base}/revoke")}).to_string()),
                    "/keys"=>("application/json",keys(&key).to_string()),
                    "/token"=>{
                        let form:BTreeMap<String,String>=url::form_urlencoded::parse(&body).map(|(k,v)|(k.into_owned(),v.into_owned())).collect();assert_eq!(form["client_id"],"oaiapp_test");assert_eq!(form["resource"],RESOURCE);
                        let response=if form["grant_type"]=="authorization_code" {
                            assert_eq!(form["redirect_uri"],expected_callback);assert_eq!(PkcePair::challenge_for(&form["code_verifier"]),expected_challenge);
                            json!({"token_type":"Bearer","access_token":"ZEUS-CANARY-access1","refresh_token":"ZEUS-CANARY-refresh1","id_token":token(&key,&server_base,"oaiapp_test",&expected_nonce),"expires_in":3600,"scope":"openid resource.invoke chatgpt.tokens.use.direct offline_access"})
                        } else {assert_eq!(form["refresh_token"],"ZEUS-CANARY-refresh1");count.fetch_add(1,Ordering::SeqCst);json!({"token_type":"Bearer","access_token":"ZEUS-CANARY-access2","refresh_token":"ZEUS-CANARY-refresh2","expires_in":3600})};("application/json",response.to_string())
                    },
                    "/models"=>("application/json",json!({"models":[{"slug":"chosen-model","display_name":"Chosen Model","visibility":"list"},{"slug":"hidden","display_name":"Hidden","visibility":"hidden"}]}).to_string()),
                    "/responses"=>{
                        let request:Value=serde_json::from_slice(&body).unwrap();assert_eq!(request["store"],false);assert_eq!(request["stream"],true);assert_eq!(request["model"],"chosen-model");
                        let response=json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"safe.txt"}]}],"usage":{"input_tokens":3,"output_tokens":2}}});
                        ("text/event-stream",format!("data: {{\"type\":\"response.output_text.delta\",\"delta\":\"safe.txt\"}}\n\ndata: {response}\n\n"))
                    },
                    "/revoke"=>{let form:BTreeMap<String,String>=url::form_urlencoded::parse(&body).map(|(k,v)|(k.into_owned(),v.into_owned())).collect();assert_eq!(form["token"],"ZEUS-CANARY-refresh2");("text/plain",String::new())},
                    _=>panic!("unexpected test endpoint"),
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
            }
        });
        let complete = tokio::spawn(login.finish());
        let mut callback = Url::parse(&query["redirect_uri"]).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("state", &query["state"])
            .append_pair("code", "new-code")
            .append_pair("client_id", "oaiapp_test");
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(client.get(callback).send().await.unwrap().status(), 200);
        let account = complete.await.unwrap().unwrap();
        assert!(account.plan_enabled);
        assert_eq!(accounts.active().unwrap().as_deref(), Some("oaiapp_test"));
        let raw_index = serde_json::to_string(&accounts.index().unwrap()).unwrap();
        assert!(!raw_index.contains("ZEUS-CANARY"));
        let mut profile = accounts.profile("oaiapp_test").unwrap();
        profile.expires_at = 0;
        accounts.save(&profile, false).unwrap();
        let (a, b) = tokio::join!(
            accounts.access_token("oaiapp_test"),
            accounts.access_token("oaiapp_test")
        );
        assert_eq!(a.unwrap().as_str(), "ZEUS-CANARY-access2");
        assert_eq!(b.unwrap().as_str(), "ZEUS-CANARY-access2");
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(accounts.models("oaiapp_test").await.unwrap().len(), 1);
        let provider = ChatgptProvider::new(
            accounts.clone(),
            "oaiapp_test".into(),
            "chosen-model".into(),
            Pricing {
                input_micro_usd_per_million: 1,
                output_micro_usd_per_million: 1,
                max_input_tokens: 4096,
            },
            10,
        )
        .unwrap();
        let response = provider
            .complete(LlmRequest {
                task: crate::TaskKind::SuggestName,
                messages: vec![crate::Message {
                    role: crate::Role::User,
                    content: "report".into(),
                }],
                response_schema: None,
                max_output_tokens: 32,
                privacy: crate::PrivacyTier::MetadataOnly,
            })
            .await
            .unwrap();
        assert_eq!(response.provider, ProviderKind::ChatgptOauth);
        assert_eq!(response.content, crate::LlmContent::Text("safe.txt".into()));
        assert!(accounts.sign_out("oaiapp_test").await.unwrap());
        assert!(accounts.profile("oaiapp_test").is_err());
        assert!(accounts.active().unwrap().is_none());
        server.abort();
    }
    #[tokio::test]
    async fn metadata_cannot_send_keys_or_credentials_to_a_foreign_domain() {
        let temp = tempfile::tempdir().unwrap();
        let accounts = ChatgptAccounts::open(
            &temp.path().join("accounts"),
            Arc::new(crate::MemorySecretStore::default()),
        )
        .unwrap();
        assert!(
            accounts
                .issuer_url(&json!("https://evil.example/keys"))
                .is_err()
        );
        assert!(
            accounts
                .issuer_url(&json!("http://auth.openai.com/keys"))
                .is_err()
        );
        assert!(
            accounts
                .issuer_url(&json!("https://user:secret@auth.openai.com/keys"))
                .is_err()
        );
        let login = accounts.begin(None).await.unwrap();
        let query: BTreeMap<String, String> = login
            .authorization_url()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let host_before = query["ext_agent_host_id"].clone();
        drop(login);
        drop(accounts);
        let reopened = ChatgptAccounts::open(
            &temp.path().join("accounts"),
            Arc::new(crate::MemorySecretStore::default()),
        )
        .unwrap();
        assert_eq!(reopened.index().unwrap().host_id, host_before);
    }
}

#[derive(Serialize, Deserialize)]
struct BundleHead {
    generation: String,
    chunks: usize,
}
fn bundle_key(id: &str, generation: &str, index: usize) -> Result<SecretKey, LlmError> {
    let mut key = ChatgptAccounts::key(id)?;
    key.purpose.push_str(&format!("/{generation}/{index}"));
    Ok(key)
}
