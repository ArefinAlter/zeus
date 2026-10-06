//! Rung selection: the first usable rung of the ladder wins.

use std::collections::BTreeSet;

use crate::error::LlmError;
use crate::privacy::PrivacyTier;
use crate::provider::{ProviderKind, ProviderStatus, StatusMap};

/// Inputs to rung selection.
#[derive(Debug, Clone, Default)]
pub struct LadderConfig {
    /// Status per provider from the policy manifest merged with user configuration. A provider that is
    /// absent counts as [`ProviderStatus::Enabled`] so a missing manifest never silently disables the user's own
    /// keys; the policy layer writes explicit `Disabled` entries for kill switches.
    pub status: StatusMap,
    /// Whether the user opted in to providers marked experimental.
    pub allow_experimental: bool,
}

/// The ordered set of rungs plus the policy needed to choose one.
#[derive(Debug, Clone, Default)]
pub struct Ladder {
    config: LadderConfig,
}

impl Ladder {
    /// Build a ladder from configuration.
    pub fn new(config: LadderConfig) -> Self {
        Self { config }
    }

    /// Status of one provider after applying defaults.
    pub fn status_of(&self, kind: ProviderKind) -> ProviderStatus {
        self.config
            .status
            .get(&kind)
            .copied()
            .unwrap_or(ProviderStatus::Enabled)
    }

    /// Whether policy allows using `kind` at all (ignoring whether it is configured).
    pub fn is_allowed(&self, kind: ProviderKind) -> bool {
        match self.status_of(kind) {
            ProviderStatus::Enabled => true,
            ProviderStatus::Experimental => self.config.allow_experimental,
            ProviderStatus::Disabled => false,
        }
    }

    /// Pick the first rung that is allowed by policy, configured, and permitted by the privacy tier.
    ///
    /// `configured` is the set of providers that report [`crate::provider::LlmProvider::is_configured`].
    /// With nothing configured the answer is [`LlmError::NotConfigured`]: the default is no model access.
    pub fn select(
        &self,
        configured: &BTreeSet<ProviderKind>,
        privacy: PrivacyTier,
    ) -> Result<ProviderKind, LlmError> {
        let mut saw_disabled = None;
        for kind in ProviderKind::LADDER {
            if !configured.contains(&kind) {
                continue;
            }
            if !self.is_allowed(kind) {
                saw_disabled = Some(kind);
                continue;
            }
            if privacy.check_provider(kind).is_err() {
                continue;
            }
            return Ok(kind);
        }
        match saw_disabled {
            Some(kind) => Err(LlmError::DisabledByPolicy(kind.as_str())),
            None => Err(LlmError::NotConfigured),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(kinds: &[ProviderKind]) -> BTreeSet<ProviderKind> {
        kinds.iter().copied().collect()
    }

    #[test]
    fn nothing_configured_means_no_model_access() {
        let ladder = Ladder::default();
        assert!(matches!(
            ladder.select(&BTreeSet::new(), PrivacyTier::MetadataOnly),
            Err(LlmError::NotConfigured)
        ));
    }

    #[test]
    fn highest_rung_wins() {
        let ladder = Ladder::default();
        let configured = set(&[
            ProviderKind::ApiKeyAnthropic,
            ProviderKind::OpenrouterOauth,
            ProviderKind::McpSampling,
        ]);
        assert_eq!(
            ladder
                .select(&configured, PrivacyTier::MetadataOnly)
                .unwrap(),
            ProviderKind::McpSampling
        );
    }

    #[test]
    fn disabled_rung_is_skipped_and_reported_when_nothing_else_remains() {
        let mut config = LadderConfig::default();
        config
            .status
            .insert(ProviderKind::ChatgptOauth, ProviderStatus::Disabled);
        let ladder = Ladder::new(config);
        let only = set(&[ProviderKind::ChatgptOauth]);
        assert!(matches!(
            ladder.select(&only, PrivacyTier::MetadataOnly),
            Err(LlmError::DisabledByPolicy("chatgpt_oauth"))
        ));
        let both = set(&[ProviderKind::ChatgptOauth, ProviderKind::ApiKeyAnthropic]);
        assert_eq!(
            ladder.select(&both, PrivacyTier::MetadataOnly).unwrap(),
            ProviderKind::ApiKeyAnthropic
        );
    }

    #[test]
    fn experimental_needs_opt_in() {
        let mut config = LadderConfig::default();
        config
            .status
            .insert(ProviderKind::CliDelegate, ProviderStatus::Experimental);
        let configured = set(&[ProviderKind::CliDelegate, ProviderKind::LocalOllama]);
        let off = Ladder::new(config.clone());
        assert_eq!(
            off.select(&configured, PrivacyTier::MetadataOnly).unwrap(),
            ProviderKind::LocalOllama
        );
        config.allow_experimental = true;
        let on = Ladder::new(config);
        assert_eq!(
            on.select(&configured, PrivacyTier::MetadataOnly).unwrap(),
            ProviderKind::CliDelegate
        );
    }

    #[test]
    fn local_only_tier_skips_remote_rungs() {
        let ladder = Ladder::default();
        let configured = set(&[ProviderKind::OpenrouterOauth, ProviderKind::LocalLlamaCpp]);
        assert_eq!(
            ladder.select(&configured, PrivacyTier::LocalOnly).unwrap(),
            ProviderKind::LocalLlamaCpp
        );
        let remote_only = set(&[ProviderKind::OpenrouterOauth]);
        assert!(ladder.select(&remote_only, PrivacyTier::LocalOnly).is_err());
    }
}
