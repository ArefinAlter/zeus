//! Explicit team enrollment and metadata calls. Provider/OIDC tokens never enter the webview.
#[path = "../../../../../../shared/team_policy.rs"]
pub mod policy;
use crate::{
    failure::{Failure, Result},
    home::AppHome,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

fn denied() -> Failure {
    Failure::new(
        "team.policy_held",
        "Team policy is missing, expired, untrusted, or restricts this operation. Refresh the policy or contact your team administrator.",
    )
}
fn invalid() -> Failure {
    Failure::invalid("Use the supported team metadata fields and limits.")
}
fn unavailable() -> Failure {
    Failure::new(
        "team.unavailable",
        "The team request could not be completed. Sign in again if your session expired.",
    )
}
fn secret(v: &Value, key: &str) -> Result<Zeroizing<String>> {
    let text = v.get(key).and_then(Value::as_str).ok_or_else(unavailable)?;
    if text.len() != 43
        || !text
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(unavailable());
    }
    Ok(Zeroizing::new(text.into()))
}
struct Flow {
    uri: String,
    verifier: Zeroizing<String>,
    device: Zeroizing<String>,
    expires: i64,
    next_poll: i64,
}
#[derive(Default)]
struct Session {
    token: Option<Zeroizing<String>>,
    flow: Option<Flow>,
}
/// Native team connection with an in-memory session and immutable administrator trust.
pub struct Team {
    guard: policy::Guard,
    session: Mutex<Session>,
}
impl Team {
    /// Load trust under this installation's shared daemon state. No network request is made on startup.
    pub fn new(home: &AppHome) -> Self {
        Self {
            guard: policy::Guard::new(&home.state_dir(), "desktop"),
            session: Mutex::new(Session::default()),
        }
    }
    /// Additional restriction before extensions can produce native download effects.
    pub fn guard_route(&self, route: &str, body: &Value) -> Result<()> {
        let feature = match route {
            "POST /api/v1/links/preview" | "POST /api/v1/links/enqueue" => Some("downloads"),
            "POST /svc/v1/sitegrab/preview"
            | "POST /native/v1/media/inspect"
            | "POST /native/v1/media/select"
            | "POST /native/v1/media/start"
            | "POST /native/v1/media/recover" => Some("downloads"),
            _ => None,
        };
        if let Some(f) = feature {
            self.guard.admit(f).map_err(|_| denied())?
        }
        let _ = body;
        Ok(())
    }
    fn status(&self, session: &Session) -> Value {
        let pin = self.guard.pin().ok();
        let policy = self.guard.current().ok();
        json!({"configured":self.guard.configured(),"trusted":pin.is_some(),"connected":session.token.is_some(),"origin":pin.map(|p|&p.origin),"tenant_id":pin.map(|p|&p.tenant_id),"policy":policy,"policy_held":self.guard.configured()&&policy.is_none()})
    }
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
        headers: &[(&str, String)],
    ) -> Result<Value> {
        let pin = self.guard.pin().map_err(|_| denied())?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| unavailable())?;
        let mut req = client.request(method, format!("{}{}", pin.origin, path));
        if let Some(token) = token {
            req = req.bearer_auth(token)
        }
        if let Some(body) = body {
            req = req.json(body)
        }
        for (name, value) in headers {
            req = req.header(*name, value)
        }
        let mut response = req.send().await.map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(match response.status().as_u16() {
                401 => Failure::new("team.sign_in_required", "Sign in to your team again."),
                403 => Failure::new(
                    "team.forbidden",
                    "Your team role or policy does not allow this operation.",
                ),
                409 | 412 => Failure::new(
                    "team.conflict",
                    "Team metadata changed. Pull the latest revision before trying again.",
                ),
                429 => Failure::new(
                    "team.rate_limited",
                    "Wait before making another team request.",
                ),
                _ => unavailable(),
            });
        }
        if response.content_length().is_some_and(|n| n > 65536) {
            return Err(unavailable());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if bytes.len() + chunk.len() > 65536 {
                return Err(unavailable());
            }
            bytes.extend_from_slice(&chunk)
        }
        serde_json::from_slice(&bytes).map_err(|_| unavailable())
    }
    async fn refresh(&self, token: &str) -> Result<Value> {
        let pin = self.guard.pin().map_err(|_| denied())?;
        let identity = self
            .request(reqwest::Method::GET, "/v1/session", Some(token), None, &[])
            .await?;
        if identity["tenant_id"] != pin.tenant_id
            || !matches!(
                identity["role"].as_str(),
                Some("reader" | "writer" | "admin")
            )
        {
            return Err(denied());
        }
        time::OffsetDateTime::parse(
            identity["expires_at"].as_str().ok_or_else(unavailable)?,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| unavailable())?;
        let e = self
            .request(
                reqwest::Method::GET,
                &format!("/v1/teams/{}/policy", pin.tenant_id),
                Some(token),
                None,
                &[],
            )
            .await?;
        let envelope = serde_json::from_value(e).map_err(|_| unavailable())?;
        let policy = self.guard.accept(&envelope).map_err(|_| denied())?;
        Ok(json!({"policy":policy,"role":identity["role"],"expires_at":identity["expires_at"]}))
    }
    /// Closed native-only routes; no arbitrary URL, bearer or remote approval route is exposed.
    pub async fn dispatch(&self, route: &str, body: &Value) -> Option<Result<Value>> {
        let op = route.strip_prefix("POST /native/v1/team/")?;
        Some(self.operation(op, body).await)
    }
    async fn operation(&self, op: &str, body: &Value) -> Result<Value> {
        let mut s = self.session.lock().await;
        if op == "status" {
            return Ok(self.status(&s));
        }
        let pin = self.guard.pin().map_err(|_| denied())?;
        if op == "connect" {
            fields(body, &["alias"])?;
            let alias = body["alias"].as_str().ok_or_else(invalid)?;
            label(alias)?;
            if s.flow.is_some() {
                return Err(Failure::new(
                    "team.enrollment_active",
                    "Finish or cancel the current sign-in first.",
                ));
            }
            let verifier = Zeroizing::new(URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()));
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            let value = self
                .request(
                    reqwest::Method::POST,
                    "/auth/device",
                    None,
                    Some(&json!({"alias":alias,"code_challenge":challenge})),
                    &[],
                )
                .await?;
            let device = secret(&value, "device_code")?;
            let code = value["user_code"].as_str().ok_or_else(unavailable)?;
            if code.len() != 11
                || !code.bytes().enumerate().all(|(i, c)| {
                    if i == 5 {
                        c == b'-'
                    } else {
                        c.is_ascii_hexdigit() && !c.is_ascii_lowercase()
                    }
                })
            {
                return Err(unavailable());
            }
            let uri = format!("{}/auth/authorize?user_code={code}", pin.origin);
            if value["verification_uri"] != uri
                || value["expires_in_ms"] != 600000
                || value["poll_interval_ms"] != 1000
            {
                return Err(unavailable());
            }
            let now = policy::now_ms();
            s.flow = Some(Flow {
                uri: uri.clone(),
                verifier,
                device,
                expires: now + 600000,
                next_poll: now + 1000,
            });
            return Ok(
                json!({"user_code":code,"verification_uri":uri,"expires_in_ms":600000,"poll_interval_ms":1000}),
            );
        }
        if op == "open" {
            fields(body, &[])?;
            let flow = s
                .flow
                .as_ref()
                .filter(|f| f.expires > policy::now_ms())
                .ok_or_else(unavailable)?;
            #[cfg(windows)]
            let program = "explorer.exe";
            #[cfg(target_os = "macos")]
            let program = "open";
            #[cfg(all(not(windows), not(target_os = "macos")))]
            let program = "xdg-open";
            std::process::Command::new(program)
                .arg(&flow.uri)
                .spawn()
                .map_err(|_| {
                    Failure::new(
                        "team.browser_unavailable",
                        "Open the displayed sign-in address in your browser.",
                    )
                })?;
            return Ok(json!({"opened":true}));
        }
        if matches!(op, "poll" | "cancel") {
            fields(body, &[])?;
            let flow = s.flow.as_ref().ok_or_else(unavailable)?;
            if policy::now_ms() >= flow.expires {
                s.flow = None;
                return Err(Failure::new(
                    "team.enrollment_expired",
                    "Start team sign-in again.",
                ));
            }
            if op == "poll" && policy::now_ms() < flow.next_poll {
                return Ok(json!({"state":"waiting","poll_after_ms":1000}));
            }
            let value = self
                .request(
                    reqwest::Method::POST,
                    &format!("/auth/device/{op}"),
                    None,
                    Some(&json!({"device_code":&*flow.device,"code_verifier":&*flow.verifier})),
                    &[],
                )
                .await?;
            if op == "cancel" {
                s.flow = None;
                return Ok(json!({"canceled":true}));
            }
            if value["state"] == "waiting" {
                if let Some(flow) = s.flow.as_mut() {
                    flow.next_poll = policy::now_ms() + 1000
                }
                return Ok(json!({"state":"waiting","poll_after_ms":1000}));
            }
            s.flow = None;
            if value["state"] != "connected" || value["tenant_id"] != pin.tenant_id {
                return Err(denied());
            }
            let token = secret(&value, "access_token")?;
            let current = self.refresh(&token).await?;
            s.token = Some(token);
            return Ok(json!({"state":"connected","session":current}));
        }
        let token = s
            .token
            .as_ref()
            .ok_or_else(|| Failure::new("team.sign_in_required", "Sign in to your team first."))?;
        if op == "disconnect" {
            fields(body, &[])?;
            let result = self
                .request(
                    reqwest::Method::POST,
                    "/v1/session/revoke",
                    Some(token),
                    None,
                    &[],
                )
                .await;
            s.token = None;
            s.flow = None;
            return result.map(|_| json!({"disconnected":true}));
        }
        if op == "refresh" {
            fields(body, &[])?;
            return self.refresh(token).await;
        }
        if op == "rotate" {
            fields(body, &[])?;
            let value = self
                .request(
                    reqwest::Method::POST,
                    "/v1/session/rotate",
                    Some(token),
                    None,
                    &[],
                )
                .await?;
            let next = secret(&value, "access_token")?;
            s.token = Some(next);
            return self
                .refresh(s.token.as_ref().ok_or_else(unavailable)?)
                .await;
        }
        let prefix = format!("/v1/teams/{}", pin.tenant_id);
        if op == "changes" {
            self.guard.admit("metadata_sync").map_err(|_| denied())?;
            fields(body, &["after"])?;
            let after = integer(&body["after"], 0, 9_007_199_254_740_991)?;
            let value = self
                .request(
                    reqwest::Method::GET,
                    &format!("{prefix}/changes?after={after}&limit=100"),
                    Some(token),
                    None,
                    &[],
                )
                .await?;
            // Typed allowlist: never forward arbitrary service fields or credentials to the page.
            let records = value["records"].as_array().ok_or_else(unavailable)?;
            if records.len() > 100 {
                return Err(unavailable());
            }
            let mut safe = Vec::new();
            for row in records {
                let kind = row["kind"].as_str().ok_or_else(unavailable)?;
                let id = row["record_id"].as_str().ok_or_else(unavailable)?;
                if !policy::valid_id(id) || !KINDS.contains(&kind) {
                    return Err(unavailable());
                }
                let revision = integer(&row["revision"], 1, 9_007_199_254_740_991)?;
                let sequence = integer(&row["sequence"], 1, 9_007_199_254_740_991)?;
                let tombstone = row["tombstone"].as_bool().ok_or_else(unavailable)?;
                if tombstone {
                    if !row["value"].is_null() {
                        return Err(unavailable());
                    }
                } else {
                    metadata(kind, &row["value"])?
                }
                safe.push(json!({"kind":kind,"record_id":id,"revision":revision,"sequence":sequence,"tombstone":tombstone,"value":row["value"]}));
            }
            return Ok(
                json!({"records":safe,"more":value["more"].as_bool().ok_or_else(unavailable)?,"snapshot_sequence":integer(&value["snapshot_sequence"],0,9_007_199_254_740_991)?}),
            );
        }
        if matches!(op, "put" | "delete") {
            self.guard.admit("metadata_sync").map_err(|_| denied())?;
            fields(
                body,
                &["kind", "record_id", "revision", "value", "idempotency_key"],
            )?;
            let kind = body["kind"].as_str().ok_or_else(invalid)?;
            let id = body["record_id"].as_str().ok_or_else(invalid)?;
            let key = body["idempotency_key"].as_str().ok_or_else(invalid)?;
            if !KINDS.contains(&kind) || !policy::valid_id(id) || !policy::valid_id(key) {
                return Err(invalid());
            }
            let revision = integer(&body["revision"], 0, 9_007_199_254_740_991)?;
            if op == "put" {
                metadata(kind, &body["value"])?
            } else if !body["value"].is_null() {
                return Err(invalid());
            }
            let value = self
                .request(
                    if op == "put" {
                        reqwest::Method::PUT
                    } else {
                        reqwest::Method::DELETE
                    },
                    &format!("{prefix}/metadata/{kind}/{id}"),
                    Some(token),
                    if op == "put" {
                        Some(&body["value"])
                    } else {
                        None
                    },
                    &[
                        ("if-match", format!("\"{revision}\"")),
                        ("idempotency-key", key.into()),
                    ],
                )
                .await?;
            return Ok(
                json!({"revision":integer(&value["revision"],1,9_007_199_254_740_991)?,"sequence":integer(&value["sequence"],1,9_007_199_254_740_991)?}),
            );
        }
        if op == "provenance" {
            self.guard.admit("provenance").map_err(|_| denied())?;
            fields(body, &["artifact", "idempotency_key"])?;
            let artifact = &body["artifact"];
            fields(artifact, &["sha256", "size_bytes", "source_kind"])?;
            let hash = artifact["sha256"].as_str().ok_or_else(invalid)?;
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                || !matches!(
                    artifact["source_kind"].as_str(),
                    Some("http" | "cloud" | "torrent" | "artifact" | "unknown")
                )
            {
                return Err(invalid());
            }
            integer(&artifact["size_bytes"], 0, 9_007_199_254_740_991)?;
            let key = body["idempotency_key"].as_str().ok_or_else(invalid)?;
            if !policy::valid_id(key) {
                return Err(invalid());
            }
            let value = self
                .request(
                    reqwest::Method::POST,
                    &format!("{prefix}/provenance"),
                    Some(token),
                    Some(artifact),
                    &[("idempotency-key", key.into())],
                )
                .await?;
            let envelope: policy::Envelope =
                serde_json::from_value(value).map_err(|_| unavailable())?;
            let verified =
                policy::verify(pin, &envelope, policy::PROVENANCE_TYPE).map_err(|_| denied())?;
            if verified["assertion_kind"] != "client_reported" || verified["artifact"] != *artifact
            {
                return Err(denied());
            }
            // Public signed assertion only, with a bounded closed shape and no remote arbitrary fields.
            fields(
                &verified,
                &[
                    "schema_version",
                    "issuer",
                    "tenant_id",
                    "assertion_kind",
                    "actor_id",
                    "observed_at",
                    "artifact",
                ],
            )?;
            let actor = verified["actor_id"].as_str().ok_or_else(unavailable)?;
            if actor.len() != 64 || !actor.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err(unavailable());
            }
            let at = verified["observed_at"].as_str().ok_or_else(unavailable)?;
            time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
                .map_err(|_| unavailable())?;
            return Ok(json!({"verified":true,"assertion":verified,"envelope":envelope}));
        }
        Err(Failure::invalid("Unknown team operation."))
    }
}
const KINDS: &[&str] = &["tags", "queue_presets", "workspace_labels"];
fn fields(v: &Value, keys: &[&str]) -> Result<()> {
    let o = v.as_object().ok_or_else(invalid)?;
    if o.len() != keys.len() || keys.iter().any(|k| !o.contains_key(*k)) {
        return Err(invalid());
    }
    Ok(())
}
fn integer(v: &Value, min: u64, max: u64) -> Result<u64> {
    let n = v.as_u64().ok_or_else(invalid)?;
    if n < min || n > max {
        return Err(invalid());
    }
    Ok(n)
}
fn label(s: &str) -> Result<()> {
    if s.is_empty()
        || s.chars().count() > 128
        || !s.chars().next().is_some_and(char::is_alphanumeric)
        || !s
            .chars()
            .all(|c| c.is_alphanumeric() || " ._()-".contains(c))
    {
        return Err(invalid());
    }
    let lower = s.to_lowercase();
    if [
        "sk-",
        "aiza",
        "akia",
        "bearer",
        "password",
        "secret",
        "token",
        "zeus-canary-",
    ]
    .iter()
    .any(|p| lower.contains(p))
    {
        return Err(invalid());
    }
    Ok(())
}
fn metadata(kind: &str, v: &Value) -> Result<()> {
    match kind {
        "tags" => {
            fields(v, &["label", "color"])?;
            label(v["label"].as_str().ok_or_else(invalid)?)?;
            let c = v["color"].as_str().ok_or_else(invalid)?;
            if c.len() != 7
                || !c.starts_with('#')
                || !c[1..]
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(invalid());
            }
        }
        "queue_presets" => {
            fields(
                v,
                &["label", "parallel", "bytes_per_second", "schedule_utc"],
            )?;
            label(v["label"].as_str().ok_or_else(invalid)?)?;
            integer(&v["parallel"], 1, 64)?;
            integer(&v["bytes_per_second"], 0, 1024u64.pow(4))?;
            fields(&v["schedule_utc"], &["start_minute", "end_minute"])?;
            integer(&v["schedule_utc"]["start_minute"], 0, 1439)?;
            integer(&v["schedule_utc"]["end_minute"], 0, 1439)?;
        }
        "workspace_labels" => {
            fields(v, &["label", "tags"])?;
            label(v["label"].as_str().ok_or_else(invalid)?)?;
            let tags = v["tags"].as_array().ok_or_else(invalid)?;
            let mut unique = std::collections::BTreeSet::new();
            if tags.len() > 32
                || tags.iter().any(|t| {
                    !t.as_str()
                        .is_some_and(|s| policy::valid_id(s) && unique.insert(s))
                })
            {
                return Err(invalid());
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_allowlist_rejects_credentials_urls_paths_and_unbounded_values() {
        assert!(!policy::valid_id("ZEUS-CANARY-record"));
        assert!(!policy::valid_id("sk-abcdefghijklmnop"));
        assert!(metadata("tags", &json!({"label":"Research","color":"#123abc"})).is_ok());
        for v in [
            json!({"label":"Research","color":"#123abc","url":"https://example.com/"}),
            json!({"label":"ZEUS-CANARY-session","color":"#123abc"}),
            json!({"label":"Research","color":"🦀123"}),
        ] {
            assert!(metadata("tags", &v).is_err());
        }
        assert!(
            metadata(
                "workspace_labels",
                &json!({"label":"Work","tags":["same","same"]})
            )
            .is_err()
        );
        assert!(metadata("queue_presets",&json!({"label":"Night","parallel":65,"bytes_per_second":0,"schedule_utc":{"start_minute":0,"end_minute":1439}})).is_err());
    }
    #[tokio::test]
    async fn unconfigured_team_does_not_network_or_change_local_authority() {
        let dir = tempfile::tempdir().unwrap();
        let team = Team::new(&AppHome::at(dir.path().to_path_buf()));
        let state = team
            .dispatch("POST /native/v1/team/status", &json!({}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state["configured"], false);
        assert!(
            team.dispatch("POST /native/v1/team/connect", &json!({"alias":"Fixture"}))
                .await
                .unwrap()
                .is_err()
        );
        assert!(team.dispatch("POST /unknown", &json!({})).await.is_none());
        assert!(
            team.guard_route("POST /api/v1/links/enqueue", &json!({}))
                .is_ok()
        );
    }
}
