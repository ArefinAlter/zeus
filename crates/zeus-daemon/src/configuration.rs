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
        let mut value = serde_json::to_value(&old).map_err(|_| internal())?;
        let parts = p.key.split('.').collect::<Vec<_>>();
        if parts.is_empty() || parts.len() > 3 || parts.iter().any(|s| s.is_empty()) {
            return Err(ApiError::invalid_params("invalid configuration key"));
        }
        let mut target = &mut value;
        for part in &parts[..parts.len() - 1] {
            target = target
                .get_mut(*part)
                .ok_or_else(|| ApiError::invalid_params("unknown configuration key"))?;
        }
        let target = target
            .get_mut(parts[parts.len() - 1])
            .ok_or_else(|| ApiError::invalid_params("unknown configuration key"))?;
        let previous = target.clone();
        *target = p.value;
        let config: DaemonConfig = serde_json::from_value(value)
            .map_err(|_| ApiError::invalid_params("configuration value has the wrong type"))?;
        config.validate().map_err(|_| {
            ApiError::invalid_params("configuration value is outside its permitted bounds")
        })?;
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
