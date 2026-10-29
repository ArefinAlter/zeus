//! Human-terminal cloud sign-in. OAuth URLs, codes and secrets never reach agent methods.
use super::cloud::{CloudAccountView, ProviderConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub use zeus_cloud::oauth::OAuthRegistration;
use zeus_types::{SecretString, Timestamp};

/// Human-only provider URL/code projection; Debug always withholds its contents.
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct CloudOAuthHumanText(SecretString);
impl CloudOAuthHumanText {
    /// Construct only when returning a human-gated authorization instruction.
    pub fn new(value: String) -> Self {
        Self(SecretString::new(value))
    }
    /// Display only on the verified controlling terminal.
    pub fn expose(&self) -> &str {
        self.0.expose()
    }
}
impl Serialize for CloudOAuthHumanText {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.expose())
    }
}
impl std::fmt::Debug for CloudOAuthHumanText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudOAuthHumanText([REDACTED])")
    }
}
/// Begin sign-in for an explicitly reviewed destination and registered application.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudOAuthBeginParams {
    /// Existing/new human-selected alias.
    pub account_id: String,
    /// Untrusted display label.
    pub label: String,
    /// Exact destination, independent of provider OAuth permissions.
    pub config: ProviderConfig,
    /// Reviewed transfer ceiling.
    pub max_bytes_per_delivery: u64,
    /// Current revision when replacing an account.
    pub expected_revision: Option<u64>,
    /// Explicit provider application and requested scopes.
    pub registration: OAuthRegistration,
    /// Registered app password entered only through hidden terminal input.
    #[serde(default)]
    pub client_secret: Option<SecretString>,
    /// CLI-owned exact loopback redirect; absent for Baidu device authorization.
    pub redirect_uri: Option<String>,
    /// Same-intent retry binding for this bounded in-memory flow.
    pub idempotency_key: String,
}
impl Serialize for CloudOAuthBeginParams {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            account_id: &'a str,
            label: &'a str,
            config: &'a ProviderConfig,
            max_bytes_per_delivery: u64,
            expected_revision: Option<u64>,
            registration: &'a OAuthRegistration,
            client_secret: Option<&'a str>,
            redirect_uri: Option<&'a str>,
            idempotency_key: &'a str,
        }
        Wire {
            account_id: &self.account_id,
            label: &self.label,
            config: &self.config,
            max_bytes_per_delivery: self.max_bytes_per_delivery,
            expected_revision: self.expected_revision,
            registration: &self.registration,
            client_secret: self.client_secret.as_ref().map(SecretString::expose),
            redirect_uri: self.redirect_uri.as_deref(),
            idempotency_key: &self.idempotency_key,
        }
        .serialize(serializer)
    }
}
/// Authorization lifecycle, without access or refresh tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudOAuthState {
    /// Waiting for external browser/device consent.
    Waiting,
    /// New credential was safely stored and destination configured.
    Connected,
    /// Access token expired; supported refresh material is available for the same account.
    RefreshRequired,
    /// Explicit cancellation.
    Canceled,
    /// Flow deadline passed; start a new sign-in.
    Expired,
    /// Provider denied or exchange failed; no credential material is returned.
    Failed,
    /// A previously configured account needs renewed authentication.
    SignInRequired,
    /// Local account authority was revoked.
    Revoked,
    /// Account uses an explicitly entered token or external rclone configuration.
    Manual,
}
/// Nonsecret status projection; only the begin result contains human instructions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudOAuthStatus {
    /// Flow identity when inspecting a sign-in attempt.
    pub flow_id: Option<String>,
    /// Destination account alias.
    pub account_id: String,
    /// Current lifecycle state.
    pub state: CloudOAuthState,
    /// Flow expiration (never token contents).
    pub expires_at: Option<Timestamp>,
    /// Current token expiration when known.
    pub token_expires_at: Option<Timestamp>,
    /// Supported refresh material is present and neither locally revoked nor uncertain.
    pub refresh_available: bool,
    /// An earlier requested keyring deletion still needs cleanup.
    pub local_cleanup_pending: bool,
    /// Latest nonsecret account scope after a completed connection.
    pub account: Option<CloudAccountView>,
}
/// Human-only initial browser/device instructions, never an agent resource.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudOAuthBeginResult {
    /// Durable destination identity plus bounded in-memory flow state.
    pub status: CloudOAuthStatus,
    /// Browser authorization URL or official device verification page.
    pub authorization_url: CloudOAuthHumanText,
    /// Device code for the person, never the private polling credential.
    pub user_code: Option<CloudOAuthHumanText>,
    /// Minimum device polling interval; zero for loopback code exchange.
    pub poll_interval_ms: u64,
}
/// Submit the one-use loopback code/state or perform one bounded device poll.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudOAuthFinishParams {
    /// Flow started by this human credential.
    pub flow_id: String,
    /// Browser callback code; absent for device polling.
    pub code: Option<SecretString>,
    /// Exact browser callback state; absent for device polling.
    pub state: Option<SecretString>,
}
impl Serialize for CloudOAuthFinishParams {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            flow_id: &'a str,
            code: Option<&'a str>,
            state: Option<&'a str>,
        }
        Wire {
            flow_id: &self.flow_id,
            code: self.code.as_ref().map(SecretString::expose),
            state: self.state.as_ref().map(SecretString::expose),
        }
        .serialize(serializer)
    }
}
/// Inspect exactly one flow or account through the human-only lifecycle surface.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudOAuthStatusParams {
    /// Started flow, mutually exclusive with account_id.
    pub flow_id: Option<String>,
    /// Existing account, mutually exclusive with flow_id.
    pub account_id: Option<String>,
}
/// Cancel a flow; no provider account credentials are changed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudOAuthCancelParams {
    /// Started flow.
    pub flow_id: String,
}
/// Human-requested refresh; regular transfers also refresh near-expiry credentials.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudOAuthRefreshParams {
    /// Registered OAuth account.
    pub account_id: String,
    /// Current destination revision.
    pub expected_revision: u64,
    /// Same-intent refresh retry binding; a saved successful refresh is not dispatched twice.
    pub idempotency_key: String,
}
/// Disable local account authority and optionally revoke access at a supported provider.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudOAuthDisconnectParams {
    /// Account to disconnect.
    pub account_id: String,
    /// Current revision the person reviewed.
    pub expected_revision: u64,
    /// Explicit remote revocation; Google revokes this app/project's scopes, not just one folder.
    pub remote: bool,
    /// Durable local revocation retry binding.
    pub idempotency_key: String,
}
/// Accurate local and remote disconnect outcomes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudOAuthDisconnectResult {
    /// Locally revoked account, independent of provider response.
    pub account: CloudAccountView,
    /// true only after confirmed provider revocation, false after failure/unsupported, null if not requested.
    pub remote_revoked: Option<bool>,
    /// Local authority is disabled even when private keyring deletion needs another attempt.
    pub local_cleanup_pending: bool,
    /// Stable redacted reason for unavailable or uncertain remote revocation.
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oauth_methods_remain_denied_even_if_an_agent_has_configuration_permission() {
        let identity = crate::ClientIdentity {
            credential_id: crate::auth::CredentialId::generate(),
            kind: crate::ClientKind::Agent,
            role: crate::ClientRole::Mcp,
            permissions: crate::ClientKind::Human.default_permissions(),
            client_name: "fixture".into(),
        };
        for method in [
            "begin",
            "finish",
            "status",
            "cancel",
            "refresh",
            "disconnect",
        ] {
            let name = format!("cloud.oauth.{method}");
            let policy = crate::policy::lookup(&name).unwrap();
            assert!(policy.human_only);
            assert!(
                crate::policy::authorize(Some(&identity), policy, &serde_json::json!({})).is_err()
            );
        }
    }
    #[test]
    fn human_instruction_and_input_debug_never_exposes_flow_material() {
        let value = CloudOAuthHumanText::new(
            "https://accounts.google.com/auth?state=ZEUS-CANARY-state".into(),
        );
        assert!(!format!("{value:?}").contains("ZEUS-CANARY"));
        assert!(
            serde_json::to_string(&value)
                .unwrap()
                .contains("ZEUS-CANARY")
        );
        let p = CloudOAuthFinishParams {
            flow_id: "cof_fixture".into(),
            code: Some(SecretString::new("ZEUS-CANARY-code".into())),
            state: Some(SecretString::new("ZEUS-CANARY-state".into())),
        };
        assert!(!format!("{p:?}").contains("ZEUS-CANARY"));
    }
}
