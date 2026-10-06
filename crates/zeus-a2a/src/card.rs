//! The A2A Agent Card and the Zeus card.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

/// Where the card is served, relative to the endpoint origin (A2A discovery convention; confirm in spec 10).
pub const WELL_KNOWN_PATH: &str = "/.well-known/agent-card.json";

/// A2A protocol version this crate targets.
pub const A2A_PROTOCOL_VERSION: &str = "1.0";

/// One way to reach the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    /// Endpoint URL.
    pub url: String,
    /// `JSONRPC`, `GRPC` or `HTTP+JSON`.
    pub protocol_binding: String,
    /// A2A protocol version spoken at this endpoint.
    pub protocol_version: String,
    /// Optional tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

/// The party that runs the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentProvider {
    /// Provider URL.
    pub url: String,
    /// Organisation name.
    pub organization: String,
}

/// Optional capabilities.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// Server-sent streaming of task updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    /// Push notifications to a webhook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_notifications: Option<bool>,
    /// An authenticated, richer card is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_agent_card: Option<bool>,
}

/// One thing the agent can do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What it does.
    pub description: String,
    /// Search tags.
    pub tags: Vec<String>,
    /// Example prompts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// Accepted media types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    /// Produced media types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
}

/// A security scheme (externally tagged, matching the protobuf JSON mapping of the `oneof`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SecurityScheme {
    /// HTTP authentication, for example `Bearer`.
    HttpAuthSecurityScheme {
        /// Scheme name from the IANA HTTP authentication scheme registry.
        scheme: String,
        /// Hint about the token format.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer_format: Option<String>,
        /// Description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Mutual TLS, required for any non-loopback exposure.
    MutualTlsSecurityScheme {
        /// Description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

/// The Agent Card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    /// Agent name.
    pub name: String,
    /// What the agent is.
    pub description: String,
    /// Endpoints.
    pub supported_interfaces: Vec<AgentInterface>,
    /// Who runs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<AgentProvider>,
    /// Agent version.
    pub version: String,
    /// Documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
    /// Capabilities.
    pub capabilities: AgentCapabilities,
    /// Named security schemes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub security_schemes: BTreeMap<String, SecurityScheme>,
    /// Which schemes a caller must satisfy (opaque until spec 10 fixes the shape).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_requirements: Vec<serde_json::Value>,
    /// Default accepted media types.
    pub default_input_modes: Vec<String>,
    /// Default produced media types.
    pub default_output_modes: Vec<String>,
    /// Skills.
    pub skills: Vec<AgentSkill>,
}

fn skill(id: &str, name: &str, description: &str, examples: &[&str]) -> AgentSkill {
    AgentSkill {
        id: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        tags: vec!["download".to_string(), "files".to_string()],
        examples: examples.iter().map(|e| (*e).to_string()).collect(),
        input_modes: vec!["application/json".to_string(), "text/plain".to_string()],
        output_modes: vec!["application/json".to_string()],
    }
}

/// The card Zeus publishes. `endpoint` is the A2A JSON-RPC URL of the bridge process. The card never contains
/// local paths, user names or session information; security scheme is a bearer token, and a card for a
/// non-loopback endpoint must also declare mutual TLS (enforced by [`zeus_agent_card`] callers in the bridge).
pub fn zeus_agent_card(endpoint: &Url, version: &str) -> AgentCard {
    let mut security_schemes = BTreeMap::new();
    security_schemes.insert(
        "zeusBearer".to_string(),
        SecurityScheme::HttpAuthSecurityScheme {
            scheme: "Bearer".to_string(),
            bearer_format: Some("opaque".to_string()),
            description: Some("Per-client token issued by the local Zeus daemon".to_string()),
        },
    );
    AgentCard {
        name: "Zeus".to_string(),
        description: "Session-aware download and file layer: fetches, resumes, verifies and files downloads on the \
                      user's machine. A person approves anything that needs their login."
            .to_string(),
        supported_interfaces: vec![AgentInterface {
            url: endpoint.to_string(),
            protocol_binding: "JSONRPC".to_string(),
            protocol_version: A2A_PROTOCOL_VERSION.to_string(),
            tenant: None,
        }],
        provider: None,
        version: version.to_string(),
        documentation_url: None,
        capabilities: AgentCapabilities {
            streaming: Some(true),
            push_notifications: Some(false),
            extended_agent_card: Some(false),
        },
        security_schemes,
        security_requirements: vec![serde_json::json!({ "schemes": { "zeusBearer": { "list": [] } } })],
        default_input_modes: vec!["application/json".to_string(), "text/plain".to_string()],
        default_output_modes: vec!["application/json".to_string()],
        skills: vec![
            skill("fetch_file", "Fetch a file", "Download a URL to the user's machine with resume and verification.", &["Download https://example.org/data.zip and verify it"]),
            skill("rescue_download", "Rescue a failed download", "Finish a download the browser dropped, using the user's own session if they approve.", &["Resume the failed download of report.pdf"]),
            skill("organize_files", "Organize files", "Plan (dry run first) renames and moves inside the user's download roots.", &["Show how you would sort my Downloads by project"]),
            skill("convert_files", "Convert files", "Convert documents with sandboxed converters (later phase).", &["Convert these DOCX files to PDF"]),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_has_the_four_skills_and_a_bearer_scheme() {
        let card = zeus_agent_card(&Url::parse("http://127.0.0.1:4750/a2a").unwrap(), "0.1.0");
        let ids: Vec<_> = card.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "fetch_file",
                "rescue_download",
                "organize_files",
                "convert_files"
            ]
        );
        assert!(card.security_schemes.contains_key("zeusBearer"));
        assert_eq!(card.capabilities.push_notifications, Some(false));
    }

    #[test]
    fn card_serialises_in_camel_case_and_round_trips() {
        let card = zeus_agent_card(&Url::parse("http://127.0.0.1:4750/a2a").unwrap(), "0.1.0");
        let value = serde_json::to_value(&card).unwrap();
        assert!(value.get("supportedInterfaces").is_some());
        assert!(value["capabilities"].get("pushNotifications").is_some());
        assert!(
            value["securitySchemes"]["zeusBearer"]
                .get("httpAuthSecurityScheme")
                .is_some()
        );
        let back: AgentCard = serde_json::from_value(value).unwrap();
        assert_eq!(back, card);
    }

    #[test]
    fn card_leaks_no_local_information() {
        let text = serde_json::to_string(&zeus_agent_card(
            &Url::parse("http://127.0.0.1:4750/a2a").unwrap(),
            "0.1.0",
        ))
        .unwrap();
        for needle in ["C:\\", "/home/", "/Users/", "cookie", "ZEUS-CANARY"] {
            assert!(!text.contains(needle), "{needle}");
        }
    }
}
