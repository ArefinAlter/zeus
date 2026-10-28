use super::*;
use base64::Engine as _;
use zeus_types::{HttpUrl, JobSpec, JobState};
impl SourceService {
    /// Review exact immutable bytes under a current account grant.
    pub async fn inspect(
        self: &Arc<Self>,
        identity: &ClientIdentity,
        p: SourcesInspectParams,
    ) -> ApiResult<SourcePlanView> {
        let _gate = self.gate.lock().await;
        let key = receipt_key(identity, "inspect", &p.idempotency_key)?;
        let hash = digest(&p)?;
        if let Some(prior) = self.replay(&key, &hash)? {
            return Ok(prior);
        }
        p.budget.validate().map_err(source_error)?;
        let owner = identity.credential_id.to_string();
        let account = self.account(&owner, &p.account_id, p.account_revision)?;
        if p.budget.max_received_bytes > account.view.budget_ceiling.max_received_bytes
            || p.budget.timeout_ms > account.view.budget_ceiling.timeout_ms
        {
            return Err(denied());
        }
        let plan_id = id("srcplan");
        let (metadata, label, notices, torrent_files) = match &p.target {
            SourceTarget::Torrent { file_indices, .. } => {
                let raw = p.metainfo_base64.as_ref().ok_or_else(invalid)?;
                if raw.expose().len() > 1024 * 1024 {
                    return Err(invalid());
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(raw.expose())
                    .map_err(|_| invalid())?;
                let review = zeus_sources::torrent::inspect(&bytes).map_err(source_error)?;
                zeus_sources::torrent::peer_scope(&account.view.config, &p.target, &review)
                    .map_err(source_error)?;
                let bytes = file_indices
                    .iter()
                    .try_fold(0u64, |sum, index| {
                        sum.checked_add(review.files[*index].bytes)
                    })
                    .ok_or_else(invalid)?;
                let files = review
                    .files
                    .iter()
                    .map(|f| SourceTorrentFileView {
                        index: f.index,
                        untrusted_path: f.untrusted_path.clone(),
                        bytes: f.bytes,
                    })
                    .collect();
                (ObjectMetadata{bytes,strong_etag:None,sha256:None,immutable_revision:review.info_hash,integrity_kind:"torrent_piece_sha1_and_local_file_sha256".into()},format!("Torrent · {} selected files",file_indices.len()),vec!["Only the reviewed numeric peers are contacted. Tracker, magnet and DHT discovery are disabled; incoming listeners and payload upload are disabled.".into(),"The receive budget counts torrent piece writes including corrupt/retried pieces; handshake/control traffic is bounded by peer/rate/time limits, not included in this byte counter.".into()], files)
            }
            target => {
                if p.metainfo_base64.is_some() {
                    return Err(invalid());
                }
                let secret = self.secret(&account)?;
                let bound = BoundSource::prepare(&account.view.config, target, secret.as_ref())
                    .map_err(source_error)?;
                let authority = Arc::new(transport::InspectAuthority {
                    service: self.clone(),
                    owner: owner.clone(),
                    account: p.account_id.clone(),
                    revision: p.account_revision,
                    deadline: Instant::now()
                        + Duration::from_millis(p.budget.timeout_ms.min(30_000)),
                });
                let transport = SourceTransport::new(
                    bound,
                    transport::virtual_url(&plan_id)?,
                    self.network.clone(),
                    authority,
                )
                .map_err(source_error)?;
                let metadata = transport.inspect().await.map_err(source_error)?;
                let label = match target {
                    SourceTarget::S3 { bucket, key, .. } => format!("S3 · {bucket}/{key}"),
                    SourceTarget::HuggingFace {
                        repository, path, ..
                    } => format!("Hugging Face · {repository}/{path}"),
                    _ => return Err(invalid()),
                };
                (metadata,label,vec!["Review pins the provider revision or strong entity validator. The cumulative body budget counts retry bytes.".into()], vec![])
            }
        };
        if metadata.bytes > p.budget.max_received_bytes {
            return Err(source_error(SourceError::Budget));
        }
        self.account(&owner, &p.account_id, p.account_revision)?;
        let view = SourcePlanView {
            plan_id: plan_id.clone(),
            account_id: p.account_id,
            account_revision: p.account_revision,
            untrusted_label: label,
            metadata,
            budget: p.budget,
            expires_at: self.now().saturating_add_ms(10 * 60 * 1000),
            notices,
            torrent_files,
        };
        let row = Plan {
            owner,
            view: view.clone(),
            target: p.target,
            metainfo: p.metainfo_base64.map(|s| s.expose().to_owned()),
        };
        self.receipt(
            key,
            hash,
            &view,
            vec![(format!("plan:{plan_id}"), encoded(&row)?)],
        )?;
        Ok(view)
    }
    pub(super) fn destination(&self, owner: &str, destination: Option<&str>) -> ApiResult<PathBuf> {
        let roots = zeus_fs::WriteRoots::open(&self.write_roots)
            .map_err(|e| ApiError::from(e.to_envelope()))?;
        let default = self.write_roots.first().ok_or_else(denied)?;
        let dest = roots
            .resolve_dest_dir(destination.map(Path::new).unwrap_or(default))
            .map_err(|e| ApiError::from(e.to_envelope()))?;
        let scoped = zeus_fs::WriteRoots::open(&(self.roots)(owner)).map_err(|_| denied())?;
        scoped.root_for(&dest).map_err(|_| denied())?;
        Ok(dest)
    }
    pub(super) fn operation_scope(&self, row: &Operation) -> ApiResult<()> {
        self.account(&row.owner, &row.view.account_id, row.view.account_revision)?;
        let destination = self.destination(&row.owner, Some(&row.dest_dir))?;
        let current = zeus_fs::DirectoryCap::open_root(&destination)
            .and_then(|cap| cap.file_id())
            .map_err(|_| denied())?;
        if current != row.dest_file_id {
            return Err(denied());
        }
        Ok(())
    }
    /// Persist an owned operation before admitting HTTP work or starting reviewed peers.
    pub async fn submit(
        self: &Arc<Self>,
        identity: &ClientIdentity,
        p: SourcesSubmitParams,
    ) -> ApiResult<SourceOperationView> {
        let _gate = self.gate.lock().await;
        let key = receipt_key(identity, "submit", &p.idempotency_key)?;
        let hash = digest(&p)?;
        if let Some(prior) = self.replay::<SourceOperationView>(&key, &hash)? {
            let mut row = self.owned(identity, &prior.operation_id)?;
            // Complete interrupted held admission with the same engine idempotency key.
            if row.view.state == "queued"
                && row.view.job_id.is_none()
                && !matches!(row.target, SourceTarget::Torrent { .. })
            {
                self.admit_http(&mut row).await?;
            }
            return self.project(row);
        }
        let owner = identity.credential_id.to_string();
        let plan = self
            .store
            .get::<Plan>(&format!("plan:{}", p.plan_id))?
            .ok_or_else(missing)?;
        if plan.owner != owner || plan.view.expires_at <= self.now() {
            return Err(denied());
        }
        self.account(&owner, &plan.view.account_id, plan.view.account_revision)?;
        if !(1..=32).contains(&p.max_connections) {
            return Err(invalid());
        }
        zeus_fs::directory::validate_filename(std::ffi::OsStr::new(&p.filename))
            .map_err(|_| invalid())?;
        let dest = self.destination(&owner, p.dest_dir.as_deref())?;
        match &plan.target {
            SourceTarget::Torrent { file_indices, .. } => {
                if file_indices.len() > 1
                    && (p.torrent_filenames.len() != file_indices.len()
                        || file_indices
                            .iter()
                            .any(|i| !p.torrent_filenames.contains_key(i)))
                {
                    return Err(invalid());
                }
                let mut names = std::collections::BTreeSet::new();
                for (index, name) in &p.torrent_filenames {
                    if !file_indices.contains(index) || !names.insert(name.to_lowercase()) {
                        return Err(invalid());
                    }
                    zeus_fs::directory::validate_filename(std::ffi::OsStr::new(name))
                        .map_err(|_| invalid())?;
                }
            }
            _ if !p.torrent_filenames.is_empty() => return Err(invalid()),
            _ => {}
        }
        let operation_id = id("srcop");
        let now = self.now();
        let view = SourceOperationView {
            operation_id: operation_id.clone(),
            plan_id: plan.view.plan_id,
            account_id: plan.view.account_id,
            account_revision: plan.view.account_revision,
            job_id: None,
            state: "queued".into(),
            untrusted_label: plan.view.untrusted_label,
            received_bytes: 0,
            payload_uploaded_bytes: 0,
            budget: plan.view.budget,
            metadata: plan.view.metadata,
            output_sha256: None,
            files: vec![],
            error_code: None,
        };
        let mut row = Operation {
            owner,
            view: view.clone(),
            target: plan.target,
            deadline: now.saturating_add_ms(view.budget.timeout_ms),
            created_at: now,
            dest_file_id: zeus_fs::DirectoryCap::open_root(&dest)
                .and_then(|cap| cap.file_id())
                .map_err(|_| denied())?,
            dest_dir: dest.to_string_lossy().into_owned(),
            filename: p.filename.clone(),
            metainfo: plan.metainfo,
            request: p,
            actor_kind: identity.action_actor().kind,
            source: identity.role.job_source(identity.credential_id.as_str()),
            publications: vec![],
        };
        self.receipt(
            key,
            hash,
            &view,
            vec![(format!("operation:{operation_id}"), encoded(&row)?)],
        )?;
        if matches!(row.target, SourceTarget::Torrent { .. }) {
            row.view.state = "running".into();
            self.store
                .save(&format!("operation:{operation_id}"), &row)?;
            self.start_torrent(&row)?;
        } else {
            self.admit_http(&mut row).await?;
        }
        Ok(row.view)
    }
    async fn admit_http(self: &Arc<Self>, row: &mut Operation) -> ApiResult<()> {
        self.account(&row.owner, &row.view.account_id, row.view.account_revision)?;
        if row.deadline <= self.now() {
            return Err(source_error(SourceError::Budget));
        }
        self.operation_scope(row)?;
        let engine = self.engine.get().ok_or_else(storage)?;
        let mut spec = JobSpec::new(
            HttpUrl::parse(transport::virtual_url(&row.view.operation_id)?.as_str())
                .map_err(|_| invalid())?,
            row.source.clone(),
        );
        spec.dest_dir = Some(row.dest_dir.clone());
        spec.filename = Some(row.filename.clone());
        spec.expected_size = Some(row.view.metadata.bytes);
        spec.expected_sha256 = row
            .view
            .metadata
            .sha256
            .as_ref()
            .map(|v| v.parse().map_err(|_| invalid()))
            .transpose()?;
        spec.max_connections = Some(row.request.max_connections);
        spec.overwrite = Some(zeus_types::Overwrite::Fail);
        spec.idempotency_key = Some(format!("source-{}", row.view.operation_id));
        let result = engine
            .enqueue_held(zeus_engine::api::EnqueueRequest {
                spec,
                caller: zeus_engine::api::Caller {
                    client_id: row.owner.clone(),
                    kind: row.actor_kind,
                },
            })
            .await
            .map_err(|e| ApiError::from(e.to_envelope()))?;
        row.view.job_id = Some(result.job.id);
        row.view.state = "running".into();
        row.view.error_code = None;
        self.store
            .save(&format!("operation:{}", row.view.operation_id), row)?;
        engine
            .control(result.job.id, zeus_engine::api::ControlAction::Resume)
            .await
            .map_err(|e| ApiError::from(e.to_envelope()))?;
        Ok(())
    }
    fn owned(&self, identity: &ClientIdentity, id: &str) -> ApiResult<Operation> {
        let row = self
            .store
            .get::<Operation>(&format!("operation:{id}"))?
            .ok_or_else(missing)?;
        if row.owner != identity.credential_id.as_str() {
            return Err(missing());
        }
        Ok(row)
    }
    fn project(&self, mut row: Operation) -> ApiResult<SourceOperationView> {
        row.view.files = row
            .publications
            .iter()
            .filter(|p| p.published)
            .map(|p| p.file.clone())
            .collect();
        if let Some(id) = row.view.job_id
            && let Some(job) = self.journal.job(id).map_err(|_| storage())?
        {
            match job.job.state {
                JobState::Completed => {
                    row.view.state = "completed".into();
                    row.view.output_sha256 = job.job.result.as_ref().map(|r| r.sha256.to_string());
                    row.view.error_code = None;
                }
                JobState::Canceled => row.view.state = "canceled".into(),
                JobState::Failed => {
                    row.view.state = "failed".into();
                    row.view.error_code = Some("sources.transfer_failed".into());
                }
                JobState::Paused => row.view.state = "paused".into(),
                _ => {}
            }
        }
        Ok(row.view)
    }
    /// Return one owned operation and its actual publication evidence.
    pub fn get(
        &self,
        identity: &ClientIdentity,
        p: SourcesGetParams,
    ) -> ApiResult<SourceOperationView> {
        self.project(self.owned(identity, &p.operation_id)?)
    }
    /// Return at most one hundred owned operations, newest first.
    pub fn list(&self, identity: &ClientIdentity) -> ApiResult<SourcesListResult> {
        let mut rows = self.store.list::<Operation>("operation:")?;
        rows.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        let operations = rows
            .into_iter()
            .filter(|r| r.owner == identity.credential_id.as_str())
            .take(100)
            .map(|r| self.project(r))
            .collect::<ApiResult<Vec<_>>>()?;
        Ok(SourcesListResult { operations })
    }
    /// Apply an owner-bound transition without broadening the original budget.
    pub async fn control(
        self: &Arc<Self>,
        identity: &ClientIdentity,
        p: SourcesControlParams,
    ) -> ApiResult<SourceOperationView> {
        let _gate = self.gate.lock().await;
        let key = receipt_key(identity, "control", &p.idempotency_key)?;
        let hash = digest(&p)?;
        if let Some(prior) = self.replay::<SourceOperationView>(&key, &hash)? {
            return self.project(self.owned(identity, &prior.operation_id)?);
        }
        let mut row = self.owned(identity, &p.operation_id)?;
        if matches!(
            self.project(row.clone())?.state.as_str(),
            "completed" | "canceled"
        ) {
            return Err(ApiError::new(
                "api.conflict",
                "source operation is terminal",
            ));
        }
        if matches!(p.action, SourceControlAction::Resume)
            && matches!(row.target, SourceTarget::Torrent { .. })
            && self
                .active
                .lock()
                .map_err(|_| storage())?
                .contains_key(&row.view.operation_id)
        {
            return Err(ApiError::new(
                "api.conflict",
                "source worker is still stopping",
            ));
        }
        let action = match p.action {
            SourceControlAction::Pause => {
                row.view.state = "paused".into();
                zeus_engine::api::ControlAction::Pause
            }
            SourceControlAction::Cancel => {
                row.view.state = "canceled".into();
                zeus_engine::api::ControlAction::Cancel
            }
            SourceControlAction::Resume => {
                self.account(&row.owner, &row.view.account_id, row.view.account_revision)?;
                if row.deadline <= self.now() {
                    return Err(source_error(SourceError::Budget));
                }
                self.operation_scope(&row)?;
                row.view.state = "running".into();
                row.view.error_code = None;
                zeus_engine::api::ControlAction::Resume
            }
        };
        row = self.store.mutate::<Operation, _>(
            &format!("operation:{}", row.view.operation_id),
            |current| {
                if matches!(current.view.state.as_str(), "completed" | "canceled") {
                    return Err(ApiError::new(
                        "api.conflict",
                        "source operation is terminal",
                    ));
                }
                // Byte charges and publication receipts may advance after the first read.
                // A control transition updates only state, preserving their durable values.
                current.view.state = row.view.state.clone();
                current.view.error_code = row.view.error_code.clone();
                Ok(current.clone())
            },
        )?;
        if let Some(id) = row.view.job_id {
            let engine = self.engine.get().ok_or_else(storage)?;
            let current = engine
                .get(id)
                .await
                .map_err(|e| ApiError::from(e.to_envelope()))?;
            let action = if matches!(p.action, SourceControlAction::Resume)
                && current.state == JobState::Failed
            {
                zeus_engine::api::ControlAction::Retry
            } else {
                action
            };
            if !(matches!(p.action, SourceControlAction::Pause)
                && current.state == JobState::Paused)
            {
                engine
                    .control(id, action)
                    .await
                    .map_err(|e| ApiError::from(e.to_envelope()))?;
            }
        } else if matches!(row.target, SourceTarget::Torrent { .. }) {
            if matches!(p.action, SourceControlAction::Resume) {
                self.start_torrent(&row)?;
            } else if let Some(token) = self
                .active
                .lock()
                .map_err(|_| storage())?
                .get(&row.view.operation_id)
            {
                token.cancel();
            }
        } else if matches!(p.action, SourceControlAction::Resume) {
            self.admit_http(&mut row).await?;
        }
        self.receipt(key, hash, &row.view, vec![])?;
        Ok(row.view)
    }
}
