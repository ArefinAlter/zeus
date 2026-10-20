//! Validated human configuration changes with atomic persistence.
use super::*;
impl RuntimeBackend {
    pub(super) fn model_human(&self, ctx: &CallContext) -> ApiResult<zeus_types::Actor> {
        self.trust.terminal(
            ctx,
            if ctx.identity.role == zeus_api::ClientRole::Tui {
                Surface::Tui
            } else {
                Surface::CliInteractive
            },
        )
    }
    pub(super) fn read_configuration(&self, p: ConfigGetParams) -> ApiResult<ConfigGetResult> {
        fn flatten(
            prefix: &str,
            value: &serde_json::Value,
            result: &mut std::collections::BTreeMap<String, serde_json::Value>,
        ) {
            if let Some(object) = value.as_object() {
                for (key, value) in object {
                    flatten(
                        &if prefix.is_empty() {
                            key.clone()
                        } else {
                            format!("{prefix}.{key}")
                        },
                        value,
                        result,
                    );
                }
            } else {
                result.insert(prefix.into(), value.clone());
            }
        }
        let mut values = std::collections::BTreeMap::new();
        flatten(
            "",
            &serde_json::to_value(&self.config).map_err(|_| internal())?,
            &mut values,
        );
        if let Some(key) = p.key {
            values.retain(|k, _| k == &key || k.starts_with(&format!("{key}.")));
            if values.is_empty() {
                return Err(ApiError::invalid_params("unknown configuration key"));
            }
        }
        Ok(ConfigGetResult {
            values,
            redacted_keys: Vec::new(),
        })
    }
    pub(super) async fn write_configuration(
        &self,
        ctx: &CallContext,
        p: ConfigSetParams,
    ) -> ApiResult<ConfigSetResult> {
        use std::io::Write;
        use zeus_fs::Filesystem;
        use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};
        let actor = self.model_human(ctx)?;
        let _gate = self.mutations.lock().await;
        let path = self.config_path.get().ok_or_else(|| {
            ApiError::new(
                "api.unavailable",
                "configuration path was not bound at startup",
            )
        })?;
        if path.is_symlink() {
            return Err(ApiError::invalid_params(
                "configuration cannot be a symlink",
            ));
        }
        let old = DaemonConfig::load(path)
            .map_err(|_| ApiError::invalid_params("cannot load stored configuration"))?;
        let (config, previous) = apply_setting(&old, &p.key, p.value.clone())?;
        let body = toml::to_string_pretty(&config).map_err(|_| internal())?;
        let mut entry =
            AuditEntry::new(Timestamp::now(), actor, "config.changed", AuditOutcome::Ok);
        entry.data.insert("key".into(), serde_json::json!(p.key));
        entry.data.insert(
            "configuration_blake3".into(),
            serde_json::json!(blake3::hash(body.as_bytes()).to_hex().to_string()),
        );
        self.audit.append(entry).map_err(error)?;
        let parent = path.parent().ok_or_else(internal)?;
        std::fs::create_dir_all(parent).map_err(|_| internal())?;
        let temp = parent.join(format!(
            ".config.{}.tmp",
            zeus_api::auth::Nonce::generate().as_str()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temp).map_err(|_| internal())?;
            zeus_api::transport::secure_private_file(&temp).map_err(|_| internal())?;
            file.write_all(body.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|_| internal())?;
            drop(file);
            std::fs::rename(&temp, path).map_err(|_| internal())?;
            zeus_api::transport::secure_private_file(path).map_err(|_| internal())?;
            zeus_fs::RealFs.sync_dir(parent).map_err(error)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result?;
        Ok(ConfigSetResult {
            replayed: false,
            previous: Some(previous),
            applied: false,
            restart_required: true,
        })
    }
}

/// The configuration with `key` set to `new`, validated, and the value it replaces. Keys name existing settings,
/// except that `api.rate_limits.service.<name>.<rate>` creates that service's entry from the defaults.
fn apply_setting(
    old: &DaemonConfig,
    key: &str,
    new: serde_json::Value,
) -> ApiResult<(DaemonConfig, serde_json::Value)> {
    let mut value = serde_json::to_value(old).map_err(|_| internal())?;
    let parts = key.split('.').collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 5 || parts.iter().any(|s| s.is_empty()) {
        return Err(ApiError::invalid_params("invalid configuration key"));
    }
    if crate::config::INEFFECTIVE_KEYS.contains(&key) {
        return Err(ApiError::not_implemented(
            "this setting is accepted in the file for compatibility but the engine does not use it yet",
        ));
    }
    if let ["api", "rate_limits", "service", name, _] = parts.as_slice()
        && let Some(services) = value
            .pointer_mut("/api/rate_limits/service")
            .and_then(serde_json::Value::as_object_mut)
        && !services.contains_key(*name)
    {
        services.insert(
            (*name).to_owned(),
            serde_json::to_value(crate::config::RateConfig::default()).map_err(|_| internal())?,
        );
    }
    let mut target = &mut value;
    for part in &parts {
        target = target
            .get_mut(*part)
            .ok_or_else(|| ApiError::invalid_params("unknown configuration key"))?;
    }
    let previous = std::mem::replace(target, new);
    let config: DaemonConfig = serde_json::from_value(value)
        .map_err(|_| ApiError::invalid_params("configuration value has the wrong type"))?;
    if crate::config::changes_ineffective(old, &config) {
        return Err(ApiError::not_implemented(
            "this setting is accepted in the file for compatibility but the engine does not use it yet",
        ));
    }
    config.validate().map_err(|_| {
        ApiError::invalid_params("configuration value is outside its permitted bounds")
    })?;
    Ok((config, previous))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_replace_existing_keys_and_create_service_rates() {
        let old = DaemonConfig::default();
        let (config, previous) = apply_setting(
            &old,
            "api.rate_limits.agent.mutating_per_s",
            serde_json::json!(5),
        )
        .unwrap();
        assert_eq!(previous, 50);
        assert_eq!(config.api.rate_limits.agent.mutating_per_s, 5);
        let (config, previous) = apply_setting(
            &config,
            "api.rate_limits.service.ogun.read_per_s",
            serde_json::json!(400),
        )
        .unwrap();
        assert_eq!(previous, 200, "a new entry starts from the defaults");
        assert_eq!(config.api.rate_limits.service["ogun"].read_per_s, 400);
        assert_eq!(config.api.rate_limits.service["ogun"].mutating_per_s, 50);
        // The file form round-trips.
        let text = toml::to_string_pretty(&config).unwrap();
        assert_eq!(DaemonConfig::parse(&text).unwrap(), config);
        for (key, value, code) in [
            (
                "api.rate_limits.agent.mutating_per_s",
                serde_json::json!(0),
                "api.invalid_params",
            ),
            (
                "api.rate_limits.service.Ogun.read_per_s",
                serde_json::json!(5),
                "api.invalid_params",
            ),
            (
                "api.rate_limits.service.agent.read_per_s",
                serde_json::json!(5),
                "api.invalid_params",
            ),
            (
                "api.rate_limits.service.ogun.unknown",
                serde_json::json!(5),
                "api.invalid_params",
            ),
            (
                "api.rate_limits.human.read_per_s",
                serde_json::json!(5),
                "api.invalid_params",
            ),
            (
                "scheduler.segment_size_mib",
                serde_json::json!(8),
                "api.not_implemented",
            ),
        ] {
            let error = apply_setting(&old, key, value).unwrap_err();
            assert_eq!(error.envelope().code.as_str(), code, "{key}");
        }
    }
}
