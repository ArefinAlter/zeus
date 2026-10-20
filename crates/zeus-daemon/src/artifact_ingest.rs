//! `artifacts.ingest` (05 section 9.13, 13 section 7) and the record owners that scope it for services.
use super::*;
use zeus_artifact::{ArtifactFilter, ParentRef};

const OWNERS: redb::TableDefinition<&str, &str> = redb::TableDefinition::new("ingested_by_v1");

/// Which service credential ingested each record (`<state>/artifact-ingests.redb`). Records hold no client ids
/// (13 section 4.5 rule 2), so the service view of ingested records is kept beside the store.
pub(super) struct IngestOwners {
    db: redb::Database,
}

impl IngestOwners {
    /// Open or create the table.
    pub(super) fn open(state: &Path) -> ApiResult<Self> {
        let path = state.join("artifact-ingests.redb");
        if path.is_symlink() {
            return Err(internal());
        }
        let db = redb::Database::create(&path).map_err(|_| internal())?;
        let tx = db.begin_write().map_err(|_| internal())?;
        tx.open_table(OWNERS).map_err(|_| internal())?;
        tx.commit().map_err(|_| internal())?;
        Ok(Self { db })
    }

    /// The credential that ingested a record, if a service did.
    pub(super) fn owner(&self, id: zeus_types::ArtifactId) -> Option<String> {
        use redb::ReadableDatabase;
        let tx = self.db.begin_read().ok()?;
        let table = tx.open_table(OWNERS).ok()?;
        let value = table.get(id.to_string().as_str()).ok()??;
        Some(value.value().to_owned())
    }

    fn record(&self, id: zeus_types::ArtifactId, credential: &str) -> ApiResult<()> {
        let mut tx = self.db.begin_write().map_err(|_| internal())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| internal())?;
        tx.open_table(OWNERS)
            .map_err(|_| internal())?
            .insert(id.to_string().as_str(), credential)
            .map_err(|_| internal())?;
        tx.commit().map_err(|_| internal())
    }
}

/// The ingest key of an API call (05 section 9.13): `api:` and 32 hex digits of `BLAKE3(client_id ":" key)`.
fn ingest_key(client_id: &str, idempotency_key: &str) -> String {
    let digest = blake3::hash(format!("{client_id}:{idempotency_key}").as_bytes());
    format!("api:{}", &digest.to_hex()[..32])
}

/// Stream a file through both digests.
fn hash_file(path: &Path) -> std::io::Result<zeus_verify::hash::Digests> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = zeus_verify::hash::DualHasher::new();
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hasher.finalize());
        }
        hasher.update(&buffer[..read]);
    }
}

/// A parent role: lowercase `snake_case`, at most 64 characters (13 section 4.3 item 3).
fn is_role(role: &str) -> bool {
    (1..=64).contains(&role.len())
        && role.starts_with(|c: char| c.is_ascii_lowercase())
        && role
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Quarantine flags and reasons gathered from every source that touches the new record.
#[derive(Default)]
struct Quarantine {
    flagged: bool,
    reasons: Vec<zeus_types::QuarantineReason>,
}

impl Quarantine {
    fn carry(&mut self, flagged: bool, reasons: &[zeus_types::QuarantineReason]) {
        self.flagged |= flagged;
        for reason in reasons {
            if !self.reasons.contains(reason) {
                self.reasons.push(*reason);
            }
        }
    }
}

impl RuntimeBackend {
    /// Hash the file, then let the store refuse it if its bytes change before the store's own read-back.
    /// Quarantine is never shed: equal content, the download that wrote the file and quarantined parents all
    /// carry theirs into the new record (13 section 4.4).
    pub(super) async fn artifact_ingest(
        &self,
        ctx: &CallContext,
        p: ArtifactsIngestParams,
    ) -> ApiResult<ArtifactsIngestResult> {
        let outside = || {
            ApiError::new(
                zeus_types::codes::POLICY_PATH_OUTSIDE_ROOTS,
                "artifacts.ingest takes a regular file inside a write root the caller may use",
            )
        };
        let (parents, operation) = Self::ingest_inputs(&p)?;
        let requested = PathBuf::from(&p.path);
        if !requested.is_absolute() {
            return Err(outside());
        }
        self.admit()?;
        let (binding, replay) = {
            let _gate = self.mutations.lock().await;
            self.receipt::<ArtifactsIngestResult>(
                ctx,
                "artifacts.ingest",
                p.idempotency_key.as_deref(),
                &p,
            )?
        };
        if let Some(mut result) = replay {
            result.replayed = true;
            return Ok(result);
        }

        // Inside a visible root, a regular file (no link), neither partial nor Zeus state.
        let roots = zeus_fs::WriteRoots::open(&self.visible_roots(ctx)).map_err(|_| outside())?;
        let path = roots.resolve_dest_dir(&requested).map_err(|_| outside())?;
        let state = self.state_dir.canonicalize().map_err(|_| internal())?;
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| outside())?;
        if !metadata.is_file()
            || path.starts_with(&state)
            || zeus_fs::finalize::is_zpart(&path)
            || path.components().any(|c| c.as_os_str() == ".zeus")
        {
            return Err(outside());
        }
        // Parents outside the caller's view are refused exactly like unknown ones.
        if parents.iter().any(|parent| {
            parent.artifact_id.is_some_and(|id| {
                self.artifacts
                    .get(id)
                    .is_ok_and(|r| !self.artifact_visible(ctx, &r))
            })
        }) {
            return Err(error(zeus_artifact::ArtifactError::LineageInvalid));
        }

        let hashed = path.clone();
        let digests = tokio::task::spawn_blocking(move || hash_file(&hashed))
            .await
            .map_err(|_| internal())?
            .map_err(|_| outside())?;
        let quarantine = self.inherited_quarantine(&path, &digests, &parents)?;
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(outside)?;
        let mut draft = zeus_artifact::ArtifactDraft::local(
            "zeus",
            env!("CARGO_PKG_VERSION"),
            Some(p.name.clone().unwrap_or(file_name)),
        );
        let record = &mut draft.record;
        record.declared_media_type = p.media_type.clone();
        if !parents.is_empty() {
            record.origin = zeus_artifact::ArtifactOrigin::Derived;
            record.produced_at = Some(Timestamp::now());
        }
        record.parents = parents;
        if let Some(operation) = operation {
            record.operation = operation;
        }
        record.metadata = p.metadata.clone().unwrap_or_default();
        record.quarantined = quarantine.flagged;
        record.quarantine_reasons = quarantine.reasons;

        self.audit_tool_call(
            ctx.identity.action_actor(),
            "artifacts.ingest",
            &p,
            &[(
                "path_blake3",
                serde_json::json!(
                    blake3::hash(path.to_string_lossy().as_bytes())
                        .to_hex()
                        .to_string()
                ),
            )],
        )?;
        let key = p
            .idempotency_key
            .as_deref()
            .map(|k| ingest_key(ctx.identity.credential_id.as_str(), k));
        let before = Timestamp::now();
        let store = self.artifacts.clone();
        let committed = tokio::task::spawn_blocking(move || {
            store.ingest_expected(&path, draft, key.as_deref(), Some(digests))
        })
        .await
        .map_err(|_| internal())?
        .map_err(error)?;
        // An older record under the same ingest key is the first commit of an earlier attempt.
        let created = committed.created_at >= before;
        if Self::scope(ctx).service().is_some() {
            self.ingest_owners
                .record(committed.id, ctx.identity.credential_id.as_str())?;
        }
        let result = ArtifactsIngestResult {
            artifact: self.export_value(committed.id)?,
            created,
            replayed: !created,
        };
        {
            let _gate = self.mutations.lock().await;
            self.store_receipt(binding, &result)?;
        }
        Ok(result)
    }

    /// Validate the caller-supplied record members and parse parents and operation.
    fn ingest_inputs(
        p: &ArtifactsIngestParams,
    ) -> ApiResult<(Vec<ParentRef>, Option<zeus_artifact::Operation>)> {
        let text = |value: &Option<String>| {
            value
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.chars().count() > 255)
        };
        if text(&p.name) || text(&p.media_type) {
            return Err(ApiError::invalid_params(
                "name and media_type have 1 to 255 characters",
            ));
        }
        if p.parents.len() > 1024 || p.metadata.as_ref().is_some_and(|m| m.len() > 32) {
            return Err(ApiError::invalid_params(
                "at most 1024 parents and 32 metadata members",
            ));
        }
        if p.metadata
            .as_ref()
            .and_then(|m| serde_json::to_vec(m).ok())
            .is_some_and(|bytes| bytes.len() > 64 * 1024)
        {
            return Err(ApiError::invalid_params("metadata exceeds 64 KiB"));
        }
        let parents: Vec<ParentRef> = p
            .parents
            .iter()
            .map(|v| serde_json::from_value(v.clone()))
            .collect::<Result<_, _>>()
            .map_err(|_| {
                ApiError::invalid_params("parents must be artifact.v1 ParentRef objects")
            })?;
        if parents.iter().any(|parent| !is_role(&parent.role)) {
            return Err(ApiError::invalid_params(
                "a parent role is lowercase snake_case of at most 64 characters",
            ));
        }
        let operation: Option<zeus_artifact::Operation> = p
            .operation
            .clone()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| ApiError::invalid_params("operation must be an artifact.v1 Operation"))?;
        if operation
            .as_ref()
            .is_some_and(|o| o.name.is_empty() || o.name.len() > 128 || o.version.len() > 64)
        {
            return Err(ApiError::invalid_params(
                "operation name has 1 to 128 characters and version at most 64",
            ));
        }
        Ok((parents, operation))
    }

    /// Quarantine of equal content in the store, of the download that wrote this path or these bytes, and of
    /// every parent.
    fn inherited_quarantine(
        &self,
        path: &Path,
        digests: &zeus_verify::hash::Digests,
        parents: &[ParentRef],
    ) -> ApiResult<Quarantine> {
        let mut quarantine = Quarantine::default();
        let by_content = |blake3| {
            self.artifacts.list(&ArtifactFilter {
                blake3: Some(blake3),
                include_deleted: true,
                limit: 10_000,
                ..Default::default()
            })
        };
        for record in by_content(digests.blake3).map_err(error)? {
            quarantine.carry(record.quarantined, &record.quarantine_reasons);
        }
        for record in self
            .journal
            .read(|tx| tx.jobs(&zeus_journal::store::JobFilter::default()))
            .map_err(error)?
        {
            if record
                .job
                .result
                .as_ref()
                .is_some_and(|r| Path::new(&r.path) == path || r.blake3 == digests.blake3)
            {
                quarantine.carry(record.job.quarantined, &record.job.quarantine_reasons);
            }
        }
        for parent in parents {
            let records = match parent.artifact_id {
                Some(id) => self.artifacts.get(id).into_iter().collect(),
                None => by_content(parent.blake3).map_err(error)?,
            };
            for record in records {
                quarantine.carry(record.quarantined, &record.quarantine_reasons);
            }
        }
        Ok(quarantine)
    }
}
