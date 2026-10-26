//! The agent's configuration: the provider ladder, consent, caps, prices and the offline switch.
//!
//! Stored in `app/agent/config.json` (no keys: those are in the key store). Rust is the authority for what a provider
//! may be sent: a provider without `consent` is never called, whatever the page asks, and `offline` stops every call.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::provider::{ErrorKind, Kind, ProviderError, Target, check_base_url};
use zeus_app::engine_config::write_atomic;
use zeus_app::failure::{Failure, Result};
use zeus_app::home::AppHome;

/// How tools are offered to a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolsMode {
    /// In the provider's own function-calling format.
    #[default]
    Native,
    /// Described in the prompt; the model answers with a JSON object (for models without tool calling).
    Json,
}

/// One entry of the ladder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCfg {
    /// A stable id (`p-anthropic`).
    pub id: String,
    /// The name shown.
    pub name: String,
    /// Which provider.
    pub kind: Kind,
    /// Whether the ladder may use it.
    #[serde(default)]
    pub enabled: bool,
    /// The base URL (empty: the public default of the kind).
    #[serde(default)]
    pub base_url: String,
    /// The model id the person typed.
    #[serde(default)]
    pub model: String,
    /// The person allowed Zeus to send threads to this provider.
    #[serde(default)]
    pub consent: bool,
    /// Monthly cap in US dollars; zero means no call.
    #[serde(default = "default_cap")]
    pub monthly_cap_usd: f64,
    /// Native tool calling or the JSON protocol.
    #[serde(default)]
    pub tools_mode: ToolsMode,
    /// Price per million input tokens (USD); empty uses the default rate.
    #[serde(default)]
    pub price_in_per_mtok: Option<f64>,
    /// Price per million output tokens (USD); empty uses the default rate.
    #[serde(default)]
    pub price_out_per_mtok: Option<f64>,
}

fn default_cap() -> f64 {
    10.0
}

impl ProviderCfg {
    /// The effective base URL.
    pub fn base(&self) -> String {
        if self.base_url.trim().is_empty() {
            self.kind.default_base_url().to_owned()
        } else {
            self.base_url.trim().to_owned()
        }
    }

    /// The target of a call, after checking the URL.
    pub fn target(&self) -> std::result::Result<Target, ProviderError> {
        let base = self.base();
        if base.is_empty() {
            return Err(ProviderError::new(ErrorKind::BadRequest, "This provider has no base URL yet."));
        }
        check_base_url(&base)?;
        Ok(Target { kind: self.kind, base_url: base, json_tools: self.tools_mode == ToolsMode::Json })
    }

    /// Whether the endpoint is on this computer or this network (no data leaves it).
    pub fn is_local(&self) -> bool {
        url::Url::parse(&self.base()).is_ok_and(|u| crate::provider::plain_http_allowed(&u))
    }
}

/// The whole configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// The ladder, in the order it is tried.
    pub providers: Vec<ProviderCfg>,
    /// The global offline switch: no model is called, local ones included.
    #[serde(default)]
    pub offline: bool,
    /// Daily cap over every provider, in US dollars.
    #[serde(default = "default_daily")]
    pub daily_cap_usd: f64,
    /// Cap per thread, in US dollars.
    #[serde(default = "default_thread")]
    pub thread_cap_usd: f64,
    /// Rate used for a provider without its own prices: US dollars per million input tokens.
    #[serde(default = "default_in")]
    pub default_price_in_per_mtok: f64,
    /// Same for output tokens.
    #[serde(default = "default_out")]
    pub default_price_out_per_mtok: f64,
    /// The output limit of one model call.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_daily() -> f64 {
    5.0
}
fn default_thread() -> f64 {
    2.0
}
fn default_in() -> f64 {
    3.0
}
fn default_out() -> f64 {
    15.0
}
fn default_max_tokens() -> u32 {
    2048
}

impl Default for AgentConfig {
    fn default() -> Self {
        let p = |id: &str, kind: Kind| ProviderCfg {
            id: id.into(),
            name: kind.label().into(),
            kind,
            enabled: false,
            base_url: String::new(),
            model: String::new(),
            consent: false,
            monthly_cap_usd: 10.0,
            tools_mode: ToolsMode::Native,
            price_in_per_mtok: None,
            price_out_per_mtok: None,
        };
        let mut ollama = p("p-ollama", Kind::Ollama);
        ollama.price_in_per_mtok = Some(0.0);
        ollama.price_out_per_mtok = Some(0.0);
        Self {
            providers: vec![p("p-anthropic", Kind::Anthropic), p("p-openai", Kind::Openai), p("p-google", Kind::Google), ollama],
            offline: false,
            daily_cap_usd: default_daily(),
            thread_cap_usd: default_thread(),
            default_price_in_per_mtok: default_in(),
            default_price_out_per_mtok: default_out(),
            max_tokens: default_max_tokens(),
        }
    }
}

impl AgentConfig {
    /// The provider with `id`.
    pub fn provider(&self, id: &str) -> Option<&ProviderCfg> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Prices (input, output) in US dollars per million tokens for a provider.
    pub fn prices(&self, p: &ProviderCfg) -> (f64, f64) {
        (p.price_in_per_mtok.unwrap_or(self.default_price_in_per_mtok), p.price_out_per_mtok.unwrap_or(self.default_price_out_per_mtok))
    }

    /// Check the shape and bounds of a configuration the page sent.
    pub fn validate(&self) -> Result<()> {
        if self.providers.len() > 24 {
            return Err(Failure::invalid("Keep at most 24 providers."));
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in &self.providers {
            let ok_id = !p.id.is_empty() && p.id.len() <= 40 && p.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
            if !ok_id || !seen.insert(p.id.clone()) {
                return Err(Failure::invalid("Provider ids must be unique and use lowercase letters, digits, - and _."));
            }
            if p.name.chars().count() > 60 || p.model.chars().count() > 120 || p.model.chars().any(char::is_control) {
                return Err(Failure::invalid("A provider name or model id is too long or has control characters."));
            }
            if !p.base_url.trim().is_empty() {
                check_base_url(&p.base_url).map_err(|e| Failure::invalid(format!("{}: {}", p.name, e.message)))?;
            }
            for v in [p.monthly_cap_usd, p.price_in_per_mtok.unwrap_or(0.0), p.price_out_per_mtok.unwrap_or(0.0)] {
                if !v.is_finite() || !(0.0..=100_000.0).contains(&v) {
                    return Err(Failure::invalid("Caps and prices must be numbers from 0 to 100000."));
                }
            }
        }
        for v in [self.daily_cap_usd, self.thread_cap_usd, self.default_price_in_per_mtok, self.default_price_out_per_mtok] {
            if !v.is_finite() || !(0.0..=100_000.0).contains(&v) {
                return Err(Failure::invalid("Caps and prices must be numbers from 0 to 100000."));
            }
        }
        if !(64..=16_384).contains(&self.max_tokens) {
            return Err(Failure::invalid("The output limit must be between 64 and 16384 tokens."));
        }
        Ok(())
    }
}

/// The file store.
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// The store under `home`.
    pub fn new(home: &AppHome) -> Self {
        Self { path: home.app_dir().join("agent").join("config.json") }
    }

    /// Load the configuration, or the defaults.
    pub fn load(&self) -> AgentConfig {
        std::fs::read(&self.path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Validate and save.
    pub fn save(&self, config: &AgentConfig) -> Result<()> {
        config.validate()?;
        write_atomic(&self.path, &serde_json::to_vec_pretty(config)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_valid_off_and_without_consent() {
        let c = AgentConfig::default();
        c.validate().unwrap();
        assert!(c.providers.iter().all(|p| !p.enabled && !p.consent));
        assert!(!c.offline);
    }

    #[test]
    fn a_plain_http_public_address_is_refused_and_a_local_one_is_not() {
        let mut c = AgentConfig::default();
        c.providers[0].base_url = "http://api.example.com".into();
        assert!(c.validate().is_err());
        c.providers[0].base_url = "http://127.0.0.1:9999".into();
        c.validate().unwrap();
        assert!(c.providers[0].is_local());
        assert!(!AgentConfig::default().providers[0].is_local());
    }

    #[test]
    fn bounds_are_enforced() {
        let mut c = AgentConfig::default();
        c.providers[1].monthly_cap_usd = -1.0;
        assert!(c.validate().is_err());
        let mut c = AgentConfig::default();
        c.max_tokens = 10;
        assert!(c.validate().is_err());
        let mut c = AgentConfig::default();
        c.providers[1].id = "Bad Id".into();
        assert!(c.validate().is_err());
    }
}
