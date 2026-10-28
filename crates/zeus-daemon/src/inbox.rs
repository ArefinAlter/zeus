//! Owner-isolated durable intake. The private request table is never a public projection.
//! Every package/receipt change commits in one immediate-durability redb transaction.
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::HashSet, path::Path};
use zeus_api::methods::{
    inbox::*,
    jobs::{EnqueueParams, EnqueueResult, NewJob},
};
use zeus_api::{ApiError, ApiResult, Page, PageRequest};
use zeus_types::{IdGenerator, RedactMode, Timestamp};

const PACKAGES: TableDefinition<&str, &[u8]> = TableDefinition::new("inbox_packages_v1");
const VIEWS: TableDefinition<&str, &[u8]> = TableDefinition::new("inbox_views_v1");
const RECEIPTS: TableDefinition<&str, &[u8]> = TableDefinition::new("inbox_receipts_v1");
const LIMITS: TableDefinition<&str, u64> = TableDefinition::new("inbox_limits_v1");
const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_PACKAGES: usize = 256;
const MAX_RECEIPTS: usize = 20_000;
pub(crate) fn failed() -> ApiError {
    ApiError::internal("durable inbox unavailable")
}
pub(crate) fn invalid(message: &str) -> ApiError {
    ApiError::invalid_params(message.to_owned())
}
pub(crate) fn conflict(message: &str) -> ApiError {
    ApiError::new("api.conflict", message)
}
fn missing() -> ApiError {
    ApiError::new("api.not_found", "intake record not found")
}
fn encoded<T: Serialize>(value: &T) -> ApiResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| failed())
}
fn decoded<T: DeserializeOwned>(value: &[u8]) -> ApiResult<T> {
    serde_json::from_slice(value).map_err(|_| failed())
}
fn digest<T: Serialize>(value: &T) -> ApiResult<String> {
    Ok(blake3::hash(&encoded(value)?).to_hex().to_string())
}
fn owner_prefix(owner: &str) -> String {
    format!("{}:", blake3::hash(owner.as_bytes()).to_hex())
}
fn package_key(owner: &str, id: &InboxPackageId) -> String {
    format!("{}{id}", owner_prefix(owner))
}
fn receipt_binding(owner: &str, method: &str, key: &str) -> ApiResult<String> {
    if key.is_empty() || key.len() > 128 {
        return Err(invalid("idempotency key must contain 1 to 128 bytes"));
    }
    Ok(format!(
        "{}{}",
        owner_prefix(owner),
        digest(&(method, key))?
    ))
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub id: InboxEntryId,
    pub item: InboxIntakeItem,
    pub fingerprint: String,
    pub created_at: Timestamp,
    pub job_id: Option<zeus_types::JobId>,
    pub error: Option<zeus_types::ErrorEnvelope>,
}
impl Entry {
    fn view(&self, default: &Option<String>) -> InboxEntryView {
        InboxEntryView {
            id: self.id.clone(),
            redacted_url: self.item.job.url.redact(RedactMode::Log),
            untrusted_label: self.item.untrusted_label.clone(),
            untrusted_filename: self.item.job.filename.clone(),
            dest_dir: self.item.job.dest_dir.clone().or_else(|| default.clone()),
            job_id: self.job_id,
            error: self.error.clone(),
            created_at: self.created_at,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Preview {
    pub view: InboxPreviewResult,
    pub jobs: Vec<NewJob>,
    pub ids: Vec<InboxEntryId>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Batch {
    pub id: InboxBatchId,
    pub preview: InboxPreviewId,
    pub ids: Vec<InboxEntryId>,
    pub params: EnqueueParams,
    pub result: Option<InboxSubmitResult>,
    pub error_code: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Package {
    pub view: InboxPackageView,
    pub entries: Vec<Entry>,
    pub previews: Vec<Preview>,
    pub batches: Vec<Batch>,
}
impl Package {
    pub fn editable(&self, revision: u64) -> ApiResult<()> {
        if self.view.revision != revision {
            return Err(conflict("package revision changed; reload and review"));
        }
        if self.view.active_batch.is_some() {
            return Err(conflict("reconcile the pending batch before editing"));
        }
        if self.view.archived {
            return Err(conflict("unarchive the package before editing"));
        }
        Ok(())
    }
    fn recount(&mut self) {
        self.view.pending_entries =
            self.entries.iter().filter(|e| e.job_id.is_none()).count() as u32;
        self.view.enqueued_entries = self.entries.len() as u32 - self.view.pending_entries;
    }
    fn changed(&mut self) -> ApiResult<()> {
        self.view.revision = self.view.revision.checked_add(1).ok_or_else(failed)?;
        self.view.updated_at = Timestamp::now();
        self.recount();
        Ok(())
    }
    pub fn candidates(&self, p: &InboxPreviewParams) -> ApiResult<Vec<Entry>> {
        self.editable(p.expected_revision)?;
        if p.entry_ids.len() > 50 {
            return Err(invalid("preview accepts at most 50 entries"));
        }
        let ids: HashSet<_> = p.entry_ids.iter().collect();
        if ids.len() != p.entry_ids.len() {
            return Err(invalid("duplicate entry reference"));
        }
        let entries: Vec<_> = if p.entry_ids.is_empty() {
            self.entries
                .iter()
                .filter(|e| e.job_id.is_none())
                .take(50)
                .cloned()
                .collect()
        } else {
            p.entry_ids
                .iter()
                .map(|id| {
                    self.entries
                        .iter()
                        .find(|e| &e.id == id && e.job_id.is_none())
                        .cloned()
                        .ok_or_else(missing)
                })
                .collect::<ApiResult<_>>()?
        };
        if entries.is_empty() {
            return Err(invalid("package has no selected pending entries"));
        }
        Ok(entries)
    }
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    hash: String,
    value: serde_json::Value,
}
/// A gate spans prepare/dispatch/commit without nesting the daemon's enqueue mutation gate.
pub(crate) struct InboxStore {
    db: Database,
    pub gate: tokio::sync::Mutex<()>,
    ids: IdGenerator,
}
impl InboxStore {
    pub fn open(path: &Path) -> ApiResult<Self> {
        if path.is_symlink() {
            return Err(failed());
        }
        let db = Database::create(path).map_err(|_| failed())?;
        let tx = db.begin_write().map_err(|_| failed())?;
        tx.open_table(PACKAGES).map_err(|_| failed())?;
        tx.open_table(VIEWS).map_err(|_| failed())?;
        tx.open_table(RECEIPTS).map_err(|_| failed())?;
        tx.open_table(LIMITS).map_err(|_| failed())?;
        tx.commit().map_err(|_| failed())?;
        Ok(Self {
            db,
            gate: Default::default(),
            ids: IdGenerator::new(),
        })
    }
    pub fn load(&self, owner: &str, id: &InboxPackageId) -> ApiResult<Package> {
        let tx = self.db.begin_read().map_err(|_| failed())?;
        let tab = tx.open_table(PACKAGES).map_err(|_| failed())?;
        let value = tab
            .get(package_key(owner, id).as_str())
            .map_err(|_| failed())?
            .ok_or_else(missing)?;
        if value.value().len() > MAX_PACKAGE_BYTES {
            return Err(failed());
        }
        decoded(value.value())
    }
    pub fn replay<P: Serialize, R: DeserializeOwned>(
        &self,
        owner: &str,
        method: &str,
        key: &str,
        params: &P,
    ) -> ApiResult<Option<R>> {
        let binding = receipt_binding(owner, method, key)?;
        let hash = digest(params)?;
        let tx = self.db.begin_read().map_err(|_| failed())?;
        let tab = tx.open_table(RECEIPTS).map_err(|_| failed())?;
        let Some(value) = tab.get(binding.as_str()).map_err(|_| failed())? else {
            return Ok(None);
        };
        let mut receipt: Receipt = decoded(value.value())?;
        if receipt.hash != hash {
            return Err(ApiError::new(
                "api.idempotency_conflict",
                "retry key is already bound to a different request",
            ));
        }
        if let Some(obj) = receipt.value.as_object_mut()
            && obj.contains_key("replayed")
        {
            obj.insert("replayed".into(), true.into());
        }
        Ok(Some(
            serde_json::from_value(receipt.value).map_err(|_| failed())?,
        ))
    }
    fn save<P: Serialize, R: Serialize>(
        &self,
        owner: &str,
        method: &str,
        key: &str,
        params: &P,
        package: &Package,
        result: &R,
    ) -> ApiResult<()> {
        let bytes = encoded(package)?;
        if bytes.len() > MAX_PACKAGE_BYTES {
            return Err(invalid(
                "package private state exceeds 16 MiB; use a new package",
            ));
        }
        let binding = receipt_binding(owner, method, key)?;
        let pkey = package_key(owner, &package.view.id);
        let prefix = owner_prefix(owner);
        let mut tx = self.db.begin_write().map_err(|_| failed())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| failed())?;
        {
            let mut tab = tx.open_table(RECEIPTS).map_err(|_| failed())?;
            let fresh = tab.get(binding.as_str()).map_err(|_| failed())?.is_none();
            let mut limits = tx.open_table(LIMITS).map_err(|_| failed())?;
            if fresh {
                let counter = format!("receipts:{prefix}");
                let saved = limits
                    .get(counter.as_str())
                    .map_err(|_| failed())?
                    .map(|v| v.value());
                let count = if let Some(count) = saved {
                    count
                } else {
                    let mut count = 0;
                    for row in tab.range(prefix.as_str()..).map_err(|_| failed())? {
                        let (key, _) = row.map_err(|_| failed())?;
                        if !key.value().starts_with(&prefix) {
                            break;
                        }
                        count += 1;
                        if count >= MAX_RECEIPTS as u64 {
                            break;
                        }
                    }
                    count
                };
                if count >= MAX_RECEIPTS as u64 {
                    return Err(invalid("owner receipt capacity reached"));
                }
                limits
                    .insert(counter.as_str(), count + 1)
                    .map_err(|_| failed())?;
            }
            let receipt = Receipt {
                hash: digest(params)?,
                value: serde_json::to_value(result).map_err(|_| failed())?,
            };
            insert_record(&mut tab, &mut limits, binding.as_str(), &encoded(&receipt)?)?;
        }
        {
            let mut limits = tx.open_table(LIMITS).map_err(|_| failed())?;
            insert_record(
                &mut tx.open_table(PACKAGES).map_err(|_| failed())?,
                &mut limits,
                &pkey,
                &bytes,
            )?;
            insert_record(
                &mut tx.open_table(VIEWS).map_err(|_| failed())?,
                &mut limits,
                &pkey,
                &encoded(&package.view)?,
            )?;
        }
        tx.commit().map_err(|_| failed())
    }
    fn save_package(&self, owner: &str, package: &Package) -> ApiResult<()> {
        let bytes = encoded(package)?;
        if bytes.len() > MAX_PACKAGE_BYTES {
            return Err(failed());
        }
        let key = package_key(owner, &package.view.id);
        let mut tx = self.db.begin_write().map_err(|_| failed())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| failed())?;
        {
            let mut limits = tx.open_table(LIMITS).map_err(|_| failed())?;
            insert_record(
                &mut tx.open_table(PACKAGES).map_err(|_| failed())?,
                &mut limits,
                &key,
                &bytes,
            )?;
            insert_record(
                &mut tx.open_table(VIEWS).map_err(|_| failed())?,
                &mut limits,
                &key,
                &encoded(&package.view)?,
            )?;
        }
        tx.commit().map_err(|_| failed())
    }
    pub fn put(&self, owner: &str, p: &InboxPackagePutParams) -> ApiResult<InboxMutationResult> {
        if let Some(r) = self.replay(owner, "put", &p.idempotency_key, p)? {
            return Ok(r);
        }
        if p.untrusted_name.trim().is_empty()
            || p.untrusted_name.len() > 160
            || p.untrusted_name.chars().any(char::is_control)
        {
            return Err(invalid(
                "package name must contain 1 to 160 printable UTF-8 bytes",
            ));
        }
        if p.default_dest_dir
            .as_ref()
            .is_some_and(|s| s.len() > 4096 || s.contains('\0'))
        {
            return Err(invalid("invalid destination default"));
        }
        let mut package = if let Some(id) = &p.package_id {
            let pkg = self.load(owner, id)?;
            pkg.editable(
                p.expected_revision
                    .ok_or_else(|| invalid("update requires expected_revision"))?,
            )?;
            pkg
        } else {
            if p.expected_revision.is_some() {
                return Err(invalid("create must omit expected_revision"));
            }
            if self.metadata(owner)?.len() >= MAX_PACKAGES {
                return Err(invalid("owner package capacity reached"));
            }
            let now = Timestamp::now();
            Package {
                view: InboxPackageView {
                    id: InboxPackageId::from_job_id(self.ids.job_id()),
                    revision: 0,
                    untrusted_name: String::new(),
                    default_dest_dir: None,
                    archived: false,
                    pending_entries: 0,
                    enqueued_entries: 0,
                    active_batch: None,
                    created_at: now,
                    updated_at: now,
                },
                entries: vec![],
                previews: vec![],
                batches: vec![],
            }
        };
        package.view.untrusted_name = p.untrusted_name.clone();
        package.view.default_dest_dir = p.default_dest_dir.clone();
        package.changed()?;
        let result = InboxMutationResult {
            package: package.view.clone(),
            replayed: false,
        };
        self.save(owner, "put", &p.idempotency_key, p, &package, &result)?;
        Ok(result)
    }
    fn metadata(&self, owner: &str) -> ApiResult<Vec<InboxPackageView>> {
        let prefix = owner_prefix(owner);
        let tx = self.db.begin_read().map_err(|_| failed())?;
        let tab = tx.open_table(VIEWS).map_err(|_| failed())?;
        let mut rows = Vec::new();
        for row in tab.range(prefix.as_str()..).map_err(|_| failed())? {
            let (key, value) = row.map_err(|_| failed())?;
            if !key.value().starts_with(&prefix) {
                break;
            }
            if rows.len() >= MAX_PACKAGES {
                return Err(failed());
            }
            rows.push(decoded(value.value())?);
        }
        Ok(rows)
    }
    pub fn list(
        &self,
        owner: &str,
        p: &InboxPackagesListParams,
    ) -> ApiResult<InboxPackagesListResult> {
        let rows = self
            .metadata(owner)?
            .into_iter()
            .filter(|v| p.include_archived || !v.archived)
            .collect();
        paged(
            rows,
            &p.page,
            &digest(&(owner, "list", p.include_archived))?,
        )
    }
    pub fn get(&self, owner: &str, p: &InboxPackageGetParams) -> ApiResult<InboxPackageGetResult> {
        let package = self.load(owner, &p.package_id)?;
        let rows = package
            .entries
            .iter()
            .filter(|e| !p.pending_only || e.job_id.is_none())
            .map(|e| e.view(&package.view.default_dest_dir))
            .collect();
        let entries = paged(
            rows,
            &p.page,
            &digest(&(owner, &p.package_id, p.pending_only, package.view.revision))?,
        )?;
        let previews = if p.include_history {
            bounded_projections(
                package.previews.iter().rev().map(|v| v.view.clone()),
                16,
                768 * 1024,
            )?
        } else {
            vec![]
        };
        let submissions = if p.include_history {
            bounded_projections(
                package
                    .batches
                    .iter()
                    .rev()
                    .map(|batch| InboxSubmissionView {
                        batch_id: batch.id.clone(),
                        package_id: package.view.id.clone(),
                        state: if batch.result.is_some() {
                            InboxDispatchState::Complete
                        } else {
                            InboxDispatchState::Pending
                        },
                        preview_id: batch.preview.clone(),
                        result: batch.result.clone(),
                        error_code: batch.error_code.clone(),
                    }),
                20,
                256 * 1024,
            )?
        } else {
            vec![]
        };
        Ok(InboxPackageGetResult {
            package: package.view,
            entries,
            previews,
            submissions,
        })
    }
    pub fn add(&self, owner: &str, p: &InboxEntriesAddParams) -> ApiResult<InboxEntriesAddResult> {
        if let Some(r) = self.replay(owner, "add", &p.idempotency_key, p)? {
            return Ok(r);
        }
        if p.entries.is_empty() || p.entries.len() > 200 || encoded(p)?.len() > 512 * 1024 {
            return Err(invalid("intake requires 1 to 200 entries within 512 KiB"));
        }
        let mut pkg = self.load(owner, &p.package_id)?;
        pkg.editable(p.expected_revision)?;
        let mut added = vec![];
        let mut duplicates = vec![];
        for item in &p.entries {
            if item.job.run_id.is_some()
                || item
                    .untrusted_label
                    .as_ref()
                    .is_some_and(|s| s.len() > 500 || s.chars().any(char::is_control))
            {
                return Err(invalid("invalid intake label or workflow identity"));
            }
            if item
                .job
                .labels
                .keys()
                .any(|k| k.starts_with("zeus.inbox") || k.starts_with("zeus.package"))
            {
                return Err(invalid("reserved intake identity label"));
            }
            let fingerprint = digest(item)?;
            if let Some(entry) = pkg.entries.iter().find(|e| e.fingerprint == fingerprint) {
                duplicates.push(entry.id.clone());
                continue;
            }
            if pkg.entries.len() >= MAX_ENTRIES {
                return Err(invalid("package entry capacity reached"));
            }
            let id = InboxEntryId::from_job_id(self.ids.job_id());
            added.push(id.clone());
            pkg.entries.push(Entry {
                id,
                item: item.clone(),
                fingerprint,
                created_at: Timestamp::now(),
                job_id: None,
                error: None,
            });
        }
        pkg.changed()?;
        let result = InboxEntriesAddResult {
            package: pkg.view.clone(),
            added_ids: added,
            duplicate_ids: duplicates,
            replayed: false,
        };
        self.save(owner, "add", &p.idempotency_key, p, &pkg, &result)?;
        Ok(result)
    }
    pub fn remove(
        &self,
        owner: &str,
        p: &InboxEntriesRemoveParams,
    ) -> ApiResult<InboxMutationResult> {
        if let Some(r) = self.replay(owner, "remove", &p.idempotency_key, p)? {
            return Ok(r);
        }
        if p.entry_ids.is_empty() || p.entry_ids.len() > 500 {
            return Err(invalid("remove requires 1 to 500 entry references"));
        }
        let mut pkg = self.load(owner, &p.package_id)?;
        pkg.editable(p.expected_revision)?;
        let ids: HashSet<_> = p.entry_ids.iter().collect();
        if ids.len() != p.entry_ids.len()
            || ids
                .iter()
                .any(|id| !pkg.entries.iter().any(|e| &e.id == *id))
        {
            return Err(missing());
        }
        pkg.entries.retain(|e| !ids.contains(&e.id));
        pkg.changed()?;
        let result = InboxMutationResult {
            package: pkg.view.clone(),
            replayed: false,
        };
        self.save(owner, "remove", &p.idempotency_key, p, &pkg, &result)?;
        Ok(result)
    }
    pub fn archive(
        &self,
        owner: &str,
        p: &InboxPackageArchiveParams,
    ) -> ApiResult<InboxMutationResult> {
        if let Some(r) = self.replay(owner, "archive", &p.idempotency_key, p)? {
            return Ok(r);
        }
        let mut pkg = self.load(owner, &p.package_id)?;
        if pkg.view.revision != p.expected_revision || pkg.view.active_batch.is_some() {
            return Err(conflict(
                "reload/reconcile the current package before changing archive state",
            ));
        }
        pkg.view.archived = p.archived;
        pkg.changed()?;
        let result = InboxMutationResult {
            package: pkg.view.clone(),
            replayed: false,
        };
        self.save(owner, "archive", &p.idempotency_key, p, &pkg, &result)?;
        Ok(result)
    }
    pub fn preview_id(&self) -> InboxPreviewId {
        InboxPreviewId::from_job_id(self.ids.job_id())
    }
    pub fn save_preview(
        &self,
        owner: &str,
        p: &InboxPreviewParams,
        preview: Preview,
    ) -> ApiResult<InboxPreviewResult> {
        let mut pkg = self.load(owner, &p.package_id)?;
        pkg.editable(p.expected_revision)?;
        // Previews are bounded ephemeral review handles; completed dispatch receipts are retained separately.
        if pkg.previews.len() >= 16 {
            pkg.previews.remove(0);
        }
        let result = preview.view.clone();
        pkg.previews.push(preview);
        self.save(owner, "preview", &p.idempotency_key, p, &pkg, &result)?;
        Ok(result)
    }
    pub fn begin(&self, owner: &str, p: &InboxSubmitParams) -> ApiResult<(Package, Batch)> {
        // A repeated submit key recovers its exact durable intent, including after preview expiry/restart.
        if let Some(batch_id) =
            self.replay::<_, InboxBatchId>(owner, "submit-intent", &p.idempotency_key, p)?
        {
            let pkg = self.load(owner, &p.package_id)?;
            let batch = pkg
                .batches
                .iter()
                .find(|b| b.id == batch_id)
                .cloned()
                .ok_or_else(failed)?;
            return Ok((pkg, batch));
        }
        let mut pkg = self.load(owner, &p.package_id)?;
        pkg.editable(p.expected_revision)?;
        let preview = pkg
            .previews
            .iter()
            .find(|v| v.view.preview_id == p.preview_id)
            .cloned()
            .ok_or_else(missing)?;
        if preview.view.expires_at <= Timestamp::now() {
            return Err(conflict("preview expired; review a new preview"));
        }
        if preview.view.package_revision != p.expected_revision || preview.jobs.is_empty() {
            return Err(conflict("preview has no current executable entries"));
        }
        if pkg.batches.len() >= 2000 {
            return Err(invalid("package batch capacity reached"));
        }
        let id = InboxBatchId::from_job_id(self.ids.job_id());
        let batch = Batch {
            id: id.clone(),
            preview: p.preview_id.clone(),
            ids: preview.ids,
            params: EnqueueParams {
                jobs: preview.jobs,
                dry_run: false,
                idempotency_key: Some(format!("inbox-{}", id.as_str())),
            },
            result: None,
            error_code: None,
        };
        pkg.view.active_batch = Some(id.clone());
        pkg.batches.push(batch.clone());
        pkg.changed()?;
        self.save(owner, "submit-intent", &p.idempotency_key, p, &pkg, &id)?;
        Ok((pkg, batch))
    }
    pub fn batch(&self, owner: &str, p: &InboxSubmissionGetParams) -> ApiResult<Batch> {
        self.load(owner, &p.package_id)?
            .batches
            .into_iter()
            .find(|b| b.id == p.batch_id)
            .ok_or_else(missing)
    }
    pub fn retry_batch(&self, owner: &str, p: &InboxSubmissionRetryParams) -> ApiResult<Batch> {
        let id = if let Some(id) =
            self.replay::<_, InboxBatchId>(owner, "retry", &p.idempotency_key, p)?
        {
            id
        } else {
            let package = self.load(owner, &p.package_id)?;
            if !package.batches.iter().any(|b| b.id == p.batch_id) {
                return Err(missing());
            }
            self.save(owner, "retry", &p.idempotency_key, p, &package, &p.batch_id)?;
            p.batch_id.clone()
        };
        self.batch(
            owner,
            &InboxSubmissionGetParams {
                owner_credential_id: None,
                package_id: p.package_id.clone(),
                batch_id: id,
            },
        )
    }
    pub fn view_batch(
        &self,
        owner: &str,
        p: &InboxSubmissionGetParams,
    ) -> ApiResult<InboxSubmissionView> {
        let batch = self.batch(owner, p)?;
        Ok(InboxSubmissionView {
            batch_id: batch.id,
            package_id: p.package_id.clone(),
            state: if batch.result.is_some() {
                InboxDispatchState::Complete
            } else {
                InboxDispatchState::Pending
            },
            preview_id: batch.preview,
            result: batch.result,
            error_code: batch.error_code,
        })
    }
    pub fn finish(
        &self,
        owner: &str,
        id: &InboxPackageId,
        batch: &Batch,
        result: EnqueueResult,
    ) -> ApiResult<InboxSubmitResult> {
        if result.jobs.len() != batch.ids.len() {
            return Err(failed());
        }
        let mut pkg = self.load(owner, id)?;
        if let Some(saved) = pkg
            .batches
            .iter()
            .find(|b| b.id == batch.id)
            .and_then(|b| b.result.clone())
        {
            let mut r = saved;
            r.replayed = true;
            return Ok(r);
        }
        if pkg.view.active_batch.as_ref() != Some(&batch.id) {
            return Err(failed());
        }
        let entries: Vec<_> = batch
            .ids
            .iter()
            .cloned()
            .zip(result.jobs)
            .map(|(entry_id, mut result)| {
                if let Some(error) = &mut result.error {
                    error.message = "download request rejected; inspect its safe error code".into();
                    error.details = Default::default();
                }
                InboxSubmissionEntry { entry_id, result }
            })
            .collect();
        for submitted in &entries {
            let entry = pkg
                .entries
                .iter_mut()
                .find(|e| e.id == submitted.entry_id)
                .ok_or_else(failed)?;
            entry.job_id = submitted.result.job_id;
            entry.error = submitted.result.error.clone();
        }
        pkg.view.active_batch = None;
        pkg.changed()?;
        let result = InboxSubmitResult {
            batch_id: batch.id.clone(),
            package_id: id.clone(),
            package_revision: pkg.view.revision,
            entries,
            remaining_pending: pkg.view.pending_entries,
            replayed: false,
        };
        let saved = pkg
            .batches
            .iter_mut()
            .find(|b| b.id == batch.id)
            .ok_or_else(failed)?;
        saved.result = Some(result.clone());
        saved.error_code = None;
        self.save_package(owner, &pkg)?;
        Ok(result)
    }
    pub fn record_error(
        &self,
        owner: &str,
        id: &InboxPackageId,
        batch: &InboxBatchId,
        code: &str,
    ) -> ApiResult<()> {
        let mut pkg = self.load(owner, id)?;
        let row = pkg
            .batches
            .iter_mut()
            .find(|b| &b.id == batch)
            .ok_or_else(missing)?;
        row.error_code = Some(
            code.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_')
                .take(100)
                .collect(),
        );
        self.save_package(owner, &pkg)
    }
}
fn paged<T: Serialize>(mut rows: Vec<T>, p: &PageRequest, binding: &str) -> ApiResult<Page<T>> {
    p.validate()?;
    let offset = if let Some(cursor) = &p.cursor {
        let (bound, offset) = cursor
            .split_once(':')
            .ok_or_else(|| invalid("invalid inbox cursor"))?;
        if bound != binding {
            return Err(conflict(
                "cursor no longer matches owner, filters or package revision",
            ));
        }
        offset
            .parse::<usize>()
            .map_err(|_| invalid("invalid inbox cursor"))?
    } else {
        0
    };
    if offset > rows.len() {
        return Err(invalid("cursor outside result"));
    }
    let limit = p.effective_limit();
    let mut bytes = 0;
    let mut count = 0;
    for row in rows.iter().skip(offset).take(limit) {
        let size = encoded(row)?.len();
        if bytes + size > 256 * 1024 && count > 0 {
            break;
        }
        bytes += size;
        count += 1;
    }
    let next = (offset.saturating_add(count) < rows.len())
        .then(|| format!("{binding}:{}", offset + count));
    Ok(Page {
        items: rows.drain(offset..).take(count).collect(),
        next_cursor: next,
    })
}

fn insert_record(
    table: &mut redb::Table<'_, &str, &[u8]>,
    limits: &mut redb::Table<'_, &str, u64>,
    key: &str,
    value: &[u8],
) -> ApiResult<()> {
    let previous = table
        .get(key)
        .map_err(|_| failed())?
        .map(|v| v.value().len() as u64);
    if previous.is_none() && table.len().map_err(|_| failed())? >= 65_536 {
        return Err(invalid("global inbox record capacity reached"));
    }
    let used = limits
        .get("bytes")
        .map_err(|_| failed())?
        .map_or(0, |v| v.value());
    let next = used
        .saturating_sub(previous.unwrap_or(0))
        .saturating_add(value.len() as u64);
    if next > 256 * 1024 * 1024 || previous.is_none() && next > 224 * 1024 * 1024 {
        return Err(invalid(
            "inbox storage capacity reached; retained recovery receipts remain available",
        ));
    }
    table.insert(key, value).map_err(|_| failed())?;
    limits.insert("bytes", next).map_err(|_| failed())?;
    Ok(())
}

#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;

fn bounded_projections<T: Serialize>(
    rows: impl Iterator<Item = T>,
    limit: usize,
    budget: usize,
) -> ApiResult<Vec<T>> {
    let mut result = Vec::new();
    let mut bytes = 0;
    for row in rows.take(limit) {
        let size = encoded(&row)?.len();
        if bytes + size > budget {
            return Err(ApiError::new(
                "api.payload_too_large",
                "requested history exceeds its bounded response; read the package without history and inspect a specific dispatch with inbox.submissions.get",
            ));
        }
        bytes += size;
        result.push(row);
    }
    Ok(result)
}
