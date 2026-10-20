//! Credential management (`clients.*`) and service roots (`files.roots.register`), 05 sections 7.6, 9.6 and 9.11.
use super::*;
use zeus_api::auth::CredentialId;
use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};

/// Admin-bucket calls per credential (05 section 13.2): `clients.add`, `clients.revoke`, `files.roots.register`.
const ADMIN_PER_MINUTE: usize = 10;

/// A bound idempotency receipt: its store key and its record, when the caller sent a key.
type Bound = Option<(String, crate::mutations::Receipt)>;

impl RuntimeBackend {
    /// The view of the calling connection (05 section 7.5).
    pub(super) fn scope(ctx: &CallContext) -> crate::visibility::Scope {
        crate::visibility::Scope::of(&ctx.identity)
    }

    /// Refuse ids outside the caller's view exactly like missing ones.
    pub(super) fn require_jobs_visible(
        &self,
        ctx: &CallContext,
        ids: &[zeus_types::JobId],
    ) -> ApiResult<()> {
        let scope = Self::scope(ctx);
        if scope.service().is_none() {
            return Ok(());
        }
        for id in ids {
            let record = self
                .journal
                .read(|tx| tx.job(*id))
                .map_err(error)?
                .ok_or_else(|| ApiError::new("api.not_found", "job not found"))?;
            scope.require_job(&record.job)?;
        }
        Ok(())
    }

    fn admin_bucket(&self, ctx: &CallContext) -> ApiResult<()> {
        let mut buckets = self.admin_limits.lock().map_err(|_| internal())?;
        if buckets.len() > 1024 {
            buckets.clear();
        }
        buckets
            .entry(ctx.identity.credential_id.to_string())
            .or_insert_with(|| crate::approvals::DecisionLimiter::new(ADMIN_PER_MINUTE, 60_000))
            .try_acquire(self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
            .map_err(|e| e.with_detail("limit", "admin"))
    }

    /// Write-ahead `tool.call` for a credential or root mutation (05 section 14).
    pub(super) fn audit_tool_call(
        &self,
        actor: zeus_types::Actor,
        tool: &str,
        params: &impl serde::Serialize,
        data: &[(&str, serde_json::Value)],
    ) -> ApiResult<()> {
        let mut value = serde_json::to_value(params).map_err(|_| internal())?;
        if let Some(object) = value.as_object_mut() {
            object.remove("idempotency_key");
        }
        let mut entry = AuditEntry::new(Timestamp::now(), actor, "tool.call", AuditOutcome::Ok);
        entry.write_ahead = true;
        entry.data.insert("tool".into(), serde_json::json!(tool));
        entry.data.insert(
            "args_blake3".into(),
            serde_json::json!(crate::approvals::facts_hash(&value)?),
        );
        for (key, value) in data {
            entry.data.insert((*key).into(), value.clone());
        }
        self.audit.append(entry).map_err(error)?;
        Ok(())
    }

    /// Replay a stored result, or bind a new receipt for `method` (05 section 12).
    pub(super) fn receipt<T: serde::de::DeserializeOwned>(
        &self,
        ctx: &CallContext,
        method: &str,
        key: Option<&str>,
        params: &impl serde::Serialize,
    ) -> ApiResult<(Bound, Option<T>)> {
        let Some(key) = key else {
            return Ok((None, None));
        };
        let binding = crate::mutations::MutationStore::binding(
            ctx.identity.credential_id.as_str(),
            method,
            key,
        )?;
        let receipt = self.receipts.begin(
            &binding,
            serde_json::to_value(params).map_err(|_| internal())?,
        )?;
        let replay = receipt
            .result
            .clone()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| internal())?;
        Ok((Some((binding, receipt)), replay))
    }

    pub(super) fn store_receipt(
        &self,
        binding: Bound,
        result: &impl serde::Serialize,
    ) -> ApiResult<()> {
        if let Some((key, mut receipt)) = binding {
            receipt.result = Some(serde_json::to_value(result).map_err(|_| internal())?);
            self.receipts.save(&key, &receipt)?;
        }
        Ok(())
    }

    pub(super) fn list_clients(&self, p: ClientsListParams) -> ApiResult<ClientsListResult> {
        Ok(ClientsListResult {
            clients: self.credentials.records(p.include_revoked),
        })
    }

    pub(super) async fn add_client(
        &self,
        ctx: &CallContext,
        p: ClientsAddParams,
    ) -> ApiResult<ClientsAddResult> {
        let actor = self.trust.terminal(ctx, Surface::CliInteractive)?;
        self.add_client_as(actor, ctx, p).await
    }

    /// `clients.add` after the caller proved a verified physical terminal.
    pub(super) async fn add_client_as(
        &self,
        actor: zeus_types::Actor,
        ctx: &CallContext,
        p: ClientsAddParams,
    ) -> ApiResult<ClientsAddResult> {
        let _gate = self.mutations.lock().await;
        let (binding, replay) =
            self.receipt::<ClientsAddResult>(ctx, "clients.add", p.idempotency_key.as_deref(), &p)?;
        if let Some(mut result) = replay {
            result.replayed = true;
            return Ok(result);
        }
        self.admin_bucket(ctx)?;
        let id = CredentialId::generate();
        self.audit_tool_call(
            actor,
            "clients.add",
            &p,
            &[
                ("credential_id", serde_json::json!(id.as_str())),
                ("kind", serde_json::json!(p.kind)),
                ("role", serde_json::json!(p.role)),
            ],
        )?;
        let (client, file) = self.credentials.add(
            &self.state_dir.join("credentials"),
            crate::provision::NewCredential {
                id,
                name: &p.name,
                kind: p.kind,
                role: p.role,
                permissions: p.permissions.clone(),
            },
            Timestamp::now(),
        )?;
        let result = ClientsAddResult {
            replayed: false,
            client,
            credential_file: file.display().to_string(),
        };
        self.store_receipt(binding, &result)?;
        Ok(result)
    }

    pub(super) async fn revoke_client(
        &self,
        ctx: &CallContext,
        p: ClientsRevokeParams,
    ) -> ApiResult<ClientsRevokeResult> {
        let actor = self.trust.terminal(ctx, Surface::CliInteractive)?;
        self.revoke_client_as(actor, ctx, p).await
    }

    /// `clients.revoke` after the caller proved a verified physical terminal.
    pub(super) async fn revoke_client_as(
        &self,
        actor: zeus_types::Actor,
        ctx: &CallContext,
        p: ClientsRevokeParams,
    ) -> ApiResult<ClientsRevokeResult> {
        let target = CredentialId::parse(&p.credential_id)
            .map_err(|_| ApiError::invalid_params("invalid credential id"))?;
        if target == ctx.identity.credential_id {
            return Err(ApiError::invalid_params(
                "a connection cannot revoke its own credential",
            ));
        }
        let _gate = self.mutations.lock().await;
        let (binding, replay) = self.receipt::<ClientsRevokeResult>(
            ctx,
            "clients.revoke",
            p.idempotency_key.as_deref(),
            &p,
        )?;
        if let Some(mut result) = replay {
            result.replayed = true;
            return Ok(result);
        }
        self.admin_bucket(ctx)?;
        let before = self
            .credentials
            .record(&target)
            .ok_or_else(|| ApiError::new("api.not_found", "credential not found"))?;
        self.audit_tool_call(
            actor,
            "clients.revoke",
            &p,
            &[("credential_id", serde_json::json!(target.as_str()))],
        )?;
        let connections_closed = self
            .connections
            .lock()
            .map_err(|_| internal())?
            .values()
            .filter(|id| **id == target)
            .count()
            .min(u32::MAX as usize) as u32;
        let client = self.credentials.revoke(
            &self.state_dir.join("credentials"),
            &target,
            Timestamp::now(),
        )?;
        if !before.revoked {
            // 05 section 7.6 rule 8: its pending approvals end as `cancel` by `system`; its jobs continue and a
            // person can cancel them.
            self.consent.withdraw_requester(target.as_str()).await?;
        }
        let result = ClientsRevokeResult {
            replayed: false,
            client,
            connections_closed: if before.revoked {
                0
            } else {
                connections_closed
            },
        };
        self.store_receipt(binding, &result)?;
        Ok(result)
    }

    pub(super) async fn register_root(
        &self,
        ctx: &CallContext,
        p: FilesRootsRegisterParams,
    ) -> ApiResult<FilesRootsRegisterResult> {
        let outside = || {
            ApiError::new(
                zeus_types::codes::POLICY_PATH_OUTSIDE_ROOTS,
                "a service root must lie inside a user write root, below it and outside the state directory",
            )
        };
        if p.label
            .as_ref()
            .is_some_and(|l| l.is_empty() || l.chars().count() > 64)
        {
            return Err(ApiError::invalid_params(
                "label must have 1 to 64 characters",
            ));
        }
        let requested = PathBuf::from(&p.path);
        if !requested.is_absolute() {
            return Err(outside());
        }
        self.admit()?;
        let _gate = self.mutations.lock().await;
        let (binding, replay) = self.receipt::<FilesRootsRegisterResult>(
            ctx,
            "files.roots.register",
            p.idempotency_key.as_deref(),
            &p,
        )?;
        if let Some(mut result) = replay {
            result.replayed = true;
            return Ok(result);
        }
        self.admin_bucket(ctx)?;
        let roots =
            zeus_fs::WriteRoots::open(&self.config.effective_write_roots()).map_err(error)?;
        let resolved = roots.resolve_dest_dir(&requested).map_err(|_| outside())?;
        let state = self.state_dir.canonicalize().map_err(|_| internal())?;
        if roots.roots().iter().any(|r| r.path() == resolved)
            || resolved.starts_with(&state)
            || state.starts_with(&resolved)
        {
            return Err(outside());
        }
        self.audit_tool_call(
            ctx.identity.action_actor(),
            "files.roots.register",
            &p,
            &[(
                "path_blake3",
                serde_json::json!(
                    blake3::hash(resolved.to_string_lossy().as_bytes())
                        .to_hex()
                        .to_string()
                ),
            )],
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&resolved)
                .map_err(|_| internal())?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(&resolved).map_err(|_| internal())?;
        // Re-check after creation: a component replaced by a link in between must not move the root.
        let canonical = roots.resolve_dest_dir(&resolved).map_err(|_| outside())?;
        if canonical != resolved {
            return Err(outside());
        }
        let (root, created) = self.credentials.register_root(
            &ctx.identity.credential_id,
            canonical,
            p.label.clone(),
            Timestamp::now(),
        )?;
        let result = FilesRootsRegisterResult {
            replayed: false,
            root,
            created,
        };
        self.store_receipt(binding, &result)?;
        Ok(result)
    }

    /// Resolve a service job's destination inside its registered roots (05 section 7.6 rule 3): an absent or
    /// relative `dest_dir` lands in its first root; anything else must already lie inside one of them.
    pub(super) fn service_destination(
        &self,
        client_id: &CredentialId,
        dest: Option<&str>,
    ) -> ApiResult<String> {
        let roots = self.credentials.roots(client_id);
        let outside = || {
            ApiError::new(
                zeus_types::codes::POLICY_PATH_OUTSIDE_ROOTS,
                "a service writes only inside its registered roots",
            )
        };
        let first = roots.first().cloned().ok_or_else(outside)?;
        let set = zeus_fs::WriteRoots::open(&roots).map_err(|_| outside())?;
        let dir = match dest {
            None => first,
            Some(d) if Path::new(d).is_absolute() => PathBuf::from(d),
            Some(d) => set.roots()[0]
                .join_checked(Path::new(d))
                .map_err(|_| outside())?,
        };
        set.resolve_dest_dir(&dir)
            .map(|d| d.display().to_string())
            .map_err(|_| outside())
    }
}
