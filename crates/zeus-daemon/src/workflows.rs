//! Durable workflow drafts, approval bindings, checkpoints and an idempotent job-intent outbox.
use async_trait::async_trait;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tokio::sync::Mutex;
use zeus_api::methods::workflows::*;
use zeus_api::{ApiError, ApiResult, ClientIdentity, ClientKind};
use zeus_types::{ApprovalId, ApprovalState, JobId, JobState, RunId, Timestamp, ToEnvelope};
use zeus_verify::AuditSink;
use zeus_workflow::*;
#[path = "workflow_receipts.rs"]
mod receipts;
use receipts::{Binding, ControlPort};
const RECORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("workflow_records_v1");
fn fail() -> ApiError {
    ApiError::internal("durable workflow state unavailable")
}
fn missing() -> ApiError {
    ApiError::new("api.not_found", "workflow not found")
}
fn runtime_error(e: RuntimeError) -> ApiError {
    match e {
        RuntimeError::Policy(code) | RuntimeError::Port(code) => {
            ApiError::new(code, "workflow cannot continue")
        }
        RuntimeError::Invalid(_) => ApiError::new(
            "workflow.plan_invalid",
            "plan is invalid or needs an unavailable capability",
        ),
        RuntimeError::CheckpointConflict => {
            ApiError::new("api.conflict", "workflow checkpoint changed")
        }
    }
}
fn runtime_fail() -> RuntimeError {
    RuntimeError::Port("workflow.storage_unavailable")
}
#[derive(Clone, Serialize, Deserialize)]
struct Draft {
    owner: String,
    plan: Option<Plan>,
    samples: Vec<PageSample>,
    updated_at: Timestamp,
    dry: Option<DryRunReport>,
}
#[derive(Clone, Serialize, Deserialize)]
struct ExportReceipt {
    path: String,
    records: u64,
    sha256: zeus_types::Hex256,
    seq: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredRun {
    pub owner: String,
    pub plan_id: PlanId,
    pub plan: Plan,
    pub snapshot: RunSnapshot,
    pub approval_id: Option<ApprovalId>,
    pending: Vec<JobIntent>,
    jobs: BTreeMap<String, JobId>,
    exports: BTreeMap<String, ExportReceipt>,
    #[serde(default)]
    active_since: Option<Timestamp>,
    #[serde(default)]
    start_audited: bool,
    #[serde(default)]
    run_receipt: Option<WorkflowsRunResult>,
    #[serde(default)]
    requested_dry_run: Option<DryRunReport>,
}
/// Separate private state database, with atomic intent+checkpoint reservations.
pub(crate) struct WorkflowStore {
    db: Database,
    started: Instant,
    wall: Timestamp,
}
impl WorkflowStore {
    fn open(path: &Path) -> ApiResult<Self> {
        if path.is_symlink() {
            return Err(fail());
        }
        let db = Database::create(path).map_err(|_| fail())?;
        let tx = db.begin_write().map_err(|_| fail())?;
        tx.open_table(RECORDS).map_err(|_| fail())?;
        tx.commit().map_err(|_| fail())?;
        let store = Self {
            db,
            started: Instant::now(),
            wall: Timestamp::now(),
        };
        for (_, row) in store.list::<StoredRun>("run:")? {
            if row.active_since.is_some() {
                store.mutate_run(row.snapshot.report.run_id, |r| {
                    r.active_since = Some(store.now());
                    Ok(())
                })?;
            }
        }
        Ok(store)
    }
    fn get<T: DeserializeOwned>(&self, key: &str) -> ApiResult<Option<T>> {
        let tx = self.db.begin_read().map_err(|_| fail())?;
        let table = tx.open_table(RECORDS).map_err(|_| fail())?;
        table
            .get(key)
            .map_err(|_| fail())?
            .map(|v| serde_json::from_slice(v.value()).map_err(|_| fail()))
            .transpose()
    }
    fn put<T: Serialize>(&self, key: &str, row: &T) -> ApiResult<()> {
        let data = serde_json::to_vec(row).map_err(|_| fail())?;
        let mut tx = self.db.begin_write().map_err(|_| fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| fail())?;
        tx.open_table(RECORDS)
            .map_err(|_| fail())?
            .insert(key, data.as_slice())
            .map_err(|_| fail())?;
        tx.commit().map_err(|_| fail())
    }
    fn list<T: DeserializeOwned>(&self, prefix: &str) -> ApiResult<Vec<(String, T)>> {
        let tx = self.db.begin_read().map_err(|_| fail())?;
        let table = tx.open_table(RECORDS).map_err(|_| fail())?;
        let mut rows = Vec::new();
        for row in table.iter().map_err(|_| fail())? {
            let (key, value) = row.map_err(|_| fail())?;
            if key.value().starts_with(prefix) {
                rows.push((
                    key.value().into(),
                    serde_json::from_slice(value.value()).map_err(|_| fail())?,
                ));
            }
        }
        Ok(rows)
    }
    pub(crate) fn run(&self, id: RunId) -> ApiResult<Option<StoredRun>> {
        self.get(&format!("run:{id}"))
    }
    fn save_run(&self, row: &StoredRun) -> ApiResult<()> {
        let mut row = row.clone();
        self.stamp_report(&mut row);
        self.put(&format!("run:{}", row.snapshot.report.run_id), &row)
    }
    fn stamp_report(&self, row: &mut StoredRun) {
        if row.snapshot.report.status != RunStatus::Running {
            row.snapshot.elapsed_ms = self.elapsed(row);
            row.active_since = None;
        }
        row.snapshot.report.updated_at = Some(self.now());
        row.snapshot.report.elapsed_ms = self.elapsed(row);
        if row.snapshot.report.status == RunStatus::Running
            && row.snapshot.report.report_version > 0
        {
            row.snapshot
                .report
                .started_at
                .get_or_insert_with(|| self.now());
        }
        row.snapshot.report.possibly_incomplete = matches!(
            row.snapshot.report.status,
            RunStatus::Canceled
                | RunStatus::Failed
                | RunStatus::Stopped
                | RunStatus::BudgetExhausted
        ) || row
            .snapshot
            .report
            .stop_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("budget_") || reason == "drift");
    }
    fn mutate_run<T>(
        &self,
        id: RunId,
        mutate: impl FnOnce(&mut StoredRun) -> ApiResult<T>,
    ) -> ApiResult<T> {
        let mut tx = self.db.begin_write().map_err(|_| fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| fail())?;
        let result;
        {
            let mut table = tx.open_table(RECORDS).map_err(|_| fail())?;
            let key = format!("run:{id}");
            let mut row: StoredRun = {
                let value = table
                    .get(key.as_str())
                    .map_err(|_| fail())?
                    .ok_or_else(missing)?;
                serde_json::from_slice(value.value()).map_err(|_| fail())?
            };
            result = mutate(&mut row)?;
            self.stamp_report(&mut row);
            let data = serde_json::to_vec(&row).map_err(|_| fail())?;
            table
                .insert(key.as_str(), data.as_slice())
                .map_err(|_| fail())?;
        }
        tx.commit().map_err(|_| fail())?;
        Ok(result)
    }
    fn reserve(&self, row: &StoredRun, binding: Option<&str>) -> ApiResult<()> {
        let mut tx = self.db.begin_write().map_err(|_| fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| fail())?;
        {
            let mut table = tx.open_table(RECORDS).map_err(|_| fail())?;
            if let Some(key) = binding {
                if table.get(key).map_err(|_| fail())?.is_some() {
                    return Err(ApiError::new("api.conflict", "run key already reserved"));
                }
                let data = serde_json::to_vec(&row.snapshot.report.run_id).map_err(|_| fail())?;
                table.insert(key, data.as_slice()).map_err(|_| fail())?;
            }
            let data = serde_json::to_vec(row).map_err(|_| fail())?;
            table
                .insert(
                    format!("run:{}", row.snapshot.report.run_id).as_str(),
                    data.as_slice(),
                )
                .map_err(|_| fail())?;
        }
        tx.commit().map_err(|_| fail())
    }
    fn now(&self) -> Timestamp {
        self.wall
            .saturating_add_ms(self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
    }
    pub(crate) fn elapsed(&self, row: &StoredRun) -> u64 {
        row.snapshot.elapsed_ms.saturating_add(
            row.active_since
                .map(|since| {
                    self.now()
                        .unix_millis()
                        .saturating_sub(since.unix_millis())
                        .max(0) as u64
                })
                .unwrap_or(0),
        )
    }
    pub(crate) fn exhausted(&self, id: RunId, reason: &str) -> ApiResult<()> {
        self.mutate_run(id, |row| {
            if matches!(
                row.snapshot.report.status,
                RunStatus::Running | RunStatus::AwaitingApproval
            ) {
                row.snapshot.elapsed_ms = self.elapsed(row);
                row.active_since = None;
                row.snapshot.report.status = RunStatus::BudgetExhausted;
                row.snapshot.report.stop_reason = Some(reason.into());
                row.snapshot.report.finished_at = Some(Timestamp::now());
                row.snapshot.checkpoint_seq = row.snapshot.checkpoint_seq.saturating_add(1);
                row.pending.clear();
            }
            Ok(())
        })
    }
    fn commit(
        &self,
        expected: u64,
        next: &RunSnapshot,
        intents: &[JobIntent],
    ) -> RuntimeResult<()> {
        self.commit_with_receipt(expected, next, intents, None)
    }
    fn commit_with_receipt(
        &self,
        expected: u64,
        next: &RunSnapshot,
        intents: &[JobIntent],
        binding: Option<&Binding>,
    ) -> RuntimeResult<()> {
        let mut tx = self.db.begin_write().map_err(|_| runtime_fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| runtime_fail())?;
        {
            let mut table = tx.open_table(RECORDS).map_err(|_| runtime_fail())?;
            if let Some(binding) = binding
                && table
                    .get(binding.key.as_str())
                    .map_err(|_| runtime_fail())?
                    .is_some()
            {
                return Err(RuntimeError::CheckpointConflict);
            }
            let key = format!("run:{}", next.report.run_id);
            let mut row: StoredRun = {
                let value = table
                    .get(key.as_str())
                    .map_err(|_| runtime_fail())?
                    .ok_or_else(runtime_fail)?;
                serde_json::from_slice(value.value()).map_err(|_| runtime_fail())?
            };
            if row.snapshot.checkpoint_seq != expected
                || row.snapshot.report.plan_hash != next.report.plan_hash
            {
                return Err(RuntimeError::CheckpointConflict);
            }
            let elapsed = self.elapsed(&row);
            row.snapshot = next.clone();
            row.snapshot.elapsed_ms = row.snapshot.elapsed_ms.max(elapsed);
            row.active_since = (next.report.status == RunStatus::Running).then(|| self.now());
            self.stamp_report(&mut row);
            if matches!(
                next.report.status,
                RunStatus::Canceled | RunStatus::Failed | RunStatus::BudgetExhausted
            ) {
                row.pending.clear();
            } else {
                row.pending.extend_from_slice(intents);
            }
            let data = serde_json::to_vec(&row).map_err(|_| runtime_fail())?;
            table
                .insert(key.as_str(), data.as_slice())
                .map_err(|_| runtime_fail())?;
            if let Some(binding) = binding {
                let bytes = binding
                    .bytes(&WorkflowsControlResult {
                        replayed: false,
                        run: row.snapshot.report.clone(),
                    })
                    .map_err(|_| runtime_fail())?;
                table
                    .insert(binding.key.as_str(), bytes.as_slice())
                    .map_err(|_| runtime_fail())?;
            }
        }
        tx.commit().map_err(|_| runtime_fail())
    }
}
/// One workflow service; create before the engine, inject its factory, then attach the engine.
pub struct WorkflowService {
    store: Arc<WorkflowStore>,
    network: Arc<crate::workflow_network::WorkflowNetwork>,
    journal: zeus_journal::Journal,
    consent: Arc<crate::consent::ConsentManager>,
    audit: Arc<dyn AuditSink>,
    engine: OnceLock<zeus_engine::Engine>,
    roots: Vec<PathBuf>,
    /// The registered roots of a service caller; services plan only inside them (05 section 7.6).
    service_roots: OnceLock<ServiceRoots>,
    gate: Mutex<()>,
    manifest_gate: std::sync::Mutex<()>,
    export_gate: std::sync::Mutex<()>,
    stopping: AtomicBool,
    audit_receipts: std::sync::Mutex<std::collections::BTreeSet<String>>,
}
/// Looks up the active roots of a service credential id.
pub(crate) type ServiceRoots = Arc<dyn Fn(&str) -> Vec<PathBuf> + Send + Sync>;
impl WorkflowService {
    /// Open private durable state. This performs no HTTP requests.
    pub fn open(
        path: &Path,
        journal: zeus_journal::Journal,
        consent: Arc<crate::consent::ConsentManager>,
        audit: Arc<dyn AuditSink>,
        roots: Vec<PathBuf>,
    ) -> ApiResult<Arc<Self>> {
        let store = Arc::new(WorkflowStore::open(path)?);
        let network = Arc::new(crate::workflow_network::WorkflowNetwork::new(
            store.clone(),
            journal.clone(),
        ));
        Ok(Arc::new(Self {
            store,
            network,
            journal,
            consent,
            audit,
            engine: OnceLock::new(),
            roots,
            service_roots: OnceLock::new(),
            gate: Mutex::new(()),
            manifest_gate: std::sync::Mutex::new(()),
            export_gate: std::sync::Mutex::new(()),
            stopping: AtomicBool::new(false),
            audit_receipts: std::sync::Mutex::new(Default::default()),
        }))
    }
    /// Recover durable audit receipts before starting the workflow runner.
    /// An append may have reached disk before its following workflow checkpoint.
    pub fn recover_audit_receipts(&self, audit: &zeus_verify::AuditLog) -> ApiResult<()> {
        let store_id = self
            .journal
            .meta()
            .map_err(|_| fail())?
            .store_id
            .to_string();
        let runs = self
            .store
            .list::<StoredRun>("run:")?
            .into_iter()
            .map(|(_, row)| row.snapshot.report.run_id)
            .collect::<std::collections::BTreeSet<_>>();
        let mut receipts = self.audit_receipts.lock().map_err(|_| fail())?;
        audit
            .visit_records(|record| {
                if record.data.get("journal_store_id").and_then(|v| v.as_str())
                    == Some(store_id.as_str())
                    && record.subject.run_id.is_some_and(|run| runs.contains(&run))
                    && matches!(
                        record.action.as_str(),
                        "workflow.run_started" | "workflow.run_ended"
                    )
                    && let Some(key) = record.data.get("workflow_receipt").and_then(|v| v.as_str())
                {
                    receipts.insert(key.to_owned());
                }
                Ok(())
            })
            .map_err(|_| {
                ApiError::new(
                    "audit.unavailable",
                    "workflow audit receipt recovery failed",
                )
            })?;
        Ok(())
    }
    fn append_audit_once(&self, mut entry: zeus_verify::AuditEntry) -> ApiResult<()> {
        let run = entry.subject.run_id.ok_or_else(fail)?;
        let store_id = self
            .journal
            .meta()
            .map_err(|_| fail())?
            .store_id
            .to_string();
        let bytes = zeus_verify::canonical::to_jcs_bytes(&entry.data).map_err(|_| fail())?;
        let key = format!(
            "{store_id}:{run}:{}:{}",
            entry.action,
            blake3::hash(&bytes).to_hex()
        );
        let mut receipts = self.audit_receipts.lock().map_err(|_| fail())?;
        if receipts.contains(&key) {
            return Ok(());
        }
        entry
            .data
            .insert("journal_store_id".into(), serde_json::json!(store_id));
        entry
            .data
            .insert("workflow_receipt".into(), serde_json::json!(key));
        entry.write_ahead = true;
        self.audit
            .append(entry)
            .map_err(|_| ApiError::new("audit.unavailable", "workflow audit unavailable"))?;
        receipts.insert(key);
        Ok(())
    }
    /// The engine uses this factory for every workflow job and redirect hop.
    pub fn transport_factory(&self) -> Arc<dyn zeus_engine::JobTransportFactory> {
        Arc::new(crate::workflow_network::Factory(self.network.clone()))
    }
    /// Complete construction once the engine has been built with the factory.
    pub fn attach_engine(&self, engine: zeus_engine::Engine) -> ApiResult<()> {
        self.engine.set(engine).map_err(|_| fail())?;
        if self
            .journal
            .meta()
            .map_err(|_| fail())?
            .kill_switch_engaged()
        {
            self.pause_runs("policy.kill_switch_engaged")?;
        }
        Ok(())
    }
    /// Prevent new workflow requests during shutdown; durable frontier resumes only on restart.
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
    /// Pause every active run after the engine atomically engages its latch. Release never resumes them.
    pub async fn pause_all_for_kill_switch(&self) -> ApiResult<()> {
        self.pause_runs("policy.kill_switch_engaged")
    }
    fn pause_runs(&self, reason: &str) -> ApiResult<()> {
        for (_, row) in self.store.list::<StoredRun>("run:")? {
            self.store.mutate_run(row.snapshot.report.run_id, |row| {
                if matches!(
                    row.snapshot.report.status,
                    RunStatus::Running | RunStatus::AwaitingApproval
                ) {
                    row.snapshot.elapsed_ms = self.store.elapsed(row);
                    row.active_since = None;
                    row.snapshot.report.status = RunStatus::Paused;
                    row.snapshot.report.pause_reason = Some("kill_switch".into());
                    row.snapshot.last_error = Some(reason.into());
                    row.snapshot.checkpoint_seq = row.snapshot.checkpoint_seq.saturating_add(1);
                }
                Ok(())
            })?;
        }
        Ok(())
    }
    fn engine(&self) -> RuntimeResult<&zeus_engine::Engine> {
        self.engine.get().ok_or_else(runtime_fail)
    }
    fn owner(identity: &ClientIdentity, owner: &str) -> bool {
        matches!(identity.kind, ClientKind::Human | ClientKind::Admin)
            || owner == identity.credential_id.as_str()
    }
    fn draft(&self, identity: &ClientIdentity, id: &PlanId) -> ApiResult<Draft> {
        let draft: Draft = self
            .store
            .get(&format!("draft:{id}"))?
            .ok_or_else(missing)?;
        if !Self::owner(identity, &draft.owner)
            || Timestamp::now()
                .unix_millis()
                .saturating_sub(draft.updated_at.unix_millis())
                > 86_400_000
        {
            return Err(missing());
        }
        Ok(draft)
    }
    fn owned_run(&self, identity: &ClientIdentity, id: RunId) -> ApiResult<StoredRun> {
        let run = self.store.run(id)?.ok_or_else(missing)?;
        if !Self::owner(identity, &run.owner) {
            return Err(missing());
        }
        Ok(run)
    }
    /// Bind the service-root lookup once at startup.
    pub(crate) fn attach_service_roots(&self, lookup: ServiceRoots) {
        let _ = self.service_roots.set(lookup);
    }
    /// Resolve the plan destination inside the caller's roots: a service's registered roots, else the user's.
    fn resolved_plan_for(&self, identity: &ClientIdentity, plan: Plan) -> ApiResult<Plan> {
        if identity.kind != ClientKind::Service {
            return self.resolved_plan(plan);
        }
        let roots = self
            .service_roots
            .get()
            .map(|lookup| lookup(identity.credential_id.as_str()))
            .unwrap_or_default();
        if roots.is_empty() {
            return Err(ApiError::new(
                "policy.path_outside_roots",
                "a service plans downloads only inside its registered roots",
            ));
        }
        self.resolved_plan_in(plan, &roots)
    }
    fn resolved_plan(&self, plan: Plan) -> ApiResult<Plan> {
        self.resolved_plan_in(plan, &self.roots)
    }
    fn resolved_plan_in(&self, mut plan: Plan, allowed: &[PathBuf]) -> ApiResult<Plan> {
        let roots =
            zeus_fs::WriteRoots::open(allowed).map_err(|e| ApiError::from(e.to_envelope()))?;
        let requested = Path::new(&plan.sinks.dest_dir);
        let destination = if requested.is_absolute() {
            requested.to_owned()
        } else {
            allowed.first().ok_or_else(fail)?.join(requested)
        };
        let safe = roots
            .resolve_dest_dir(&destination)
            .map_err(|e| ApiError::from(e.to_envelope()))?;
        plan.sinks.dest_dir = safe.to_string_lossy().into_owned();
        Ok(plan)
    }
    /// Fetch bounded page samples or validate a candidate against them.
    pub async fn plan(
        &self,
        identity: &ClientIdentity,
        p: WorkflowsPlanParams,
    ) -> ApiResult<WorkflowsPlanResult> {
        let _gate = self.gate.lock().await;
        let binding = Binding::new(identity, "workflows.plan", p.idempotency_key.as_deref(), &p)?;
        if let Some(mut result) = self.store.replay::<WorkflowsPlanResult>(binding.as_ref())? {
            result.replayed = true;
            return Ok(result);
        }
        if p.goal.len() > 2000 || p.start_urls.is_empty() || p.start_urls.len() > 3 {
            return Err(ApiError::invalid_params(
                "sampling needs 1 to 3 start URLs and a bounded goal",
            ));
        }
        if p.session_origin.is_some() {
            return Err(ApiError::new(
                "workflow.capability_missing",
                "authenticated document sampling requires browser page support",
            ));
        }
        let id = p.plan_id.clone().unwrap_or_else(|| {
            PlanId::parse(
                &zeus_types::IdGenerator::new()
                    .run_id()
                    .to_string()
                    .replacen("run_", "plan_", 1),
            )
            .expect("generated ULID")
        });
        let old = p
            .plan_id
            .as_ref()
            .map(|id| self.draft(identity, id))
            .transpose()?;
        if old.is_none() && self.store.list::<Draft>("draft:")?.len() >= 100 {
            return Err(ApiError::new(
                "workflow.too_many_drafts",
                "draft capacity reached",
            ));
        }
        let mut samples = old.as_ref().map(|d| d.samples.clone()).unwrap_or_default();
        if samples.is_empty() {
            let sample_plan = sampling_plan(&p.start_urls)?;
            for url in &p.start_urls {
                let doc = self
                    .network
                    .document(&sample_plan, url, None)
                    .await
                    .map_err(runtime_error)?;
                let mut body = doc.body;
                while body.len() > 512 * 1024 {
                    body.pop();
                }
                samples.push(PageSample {
                    url_redacted: zeus_workflow::runtime::display_url(&doc.final_url),
                    content_type: Some(doc.content_type),
                    body,
                });
            }
        }
        let mut diagnostics = Vec::new();
        let accepted = if let Some(candidate) = p.candidate {
            diagnostics = validate_plan(&candidate);
            diagnostics.extend(validate_samples(&candidate, &samples));
            if !has_errors(&diagnostics) {
                let candidate = self.resolved_plan_for(identity, candidate)?;
                self.validate_environment(&candidate)
                    .await
                    .map_err(runtime_error)?;
                Some(candidate)
            } else {
                None
            }
        } else {
            old.and_then(|d| d.plan)
        };
        let plan_hash = accepted
            .as_ref()
            .map(Plan::plan_hash)
            .transpose()
            .map_err(|_| fail())?;
        let draft = Draft {
            owner: identity.credential_id.to_string(),
            plan: accepted.clone(),
            samples: samples.clone(),
            updated_at: Timestamp::now(),
            dry: None,
        };
        let result = WorkflowsPlanResult {
            replayed: false,
            plan_id: id.clone(),
            samples,
            plan_schema: serde_json::to_value(schemars::schema_for!(Plan)).map_err(|_| fail())?,
            diagnostics,
            plan: accepted,
            plan_hash,
            draft: None,
        };
        self.store
            .put_with_receipt(&format!("draft:{id}"), &draft, binding.as_ref(), &result)?;
        Ok(result)
    }
    /// Dry-run pages and selectors with the same network restrictions as execution.
    pub async fn dry_run(
        &self,
        identity: &ClientIdentity,
        p: WorkflowsDryRunParams,
    ) -> ApiResult<DryRunReport> {
        let _gate = self.gate.lock().await;
        let mut draft = self.draft(identity, &p.plan_id)?;
        let plan = draft.plan.clone().ok_or_else(|| {
            ApiError::new("workflow.plan_invalid", "draft has no valid candidate")
        })?;
        self.validate_environment(&plan)
            .await
            .map_err(runtime_error)?;
        struct Samples<'a> {
            service: &'a WorkflowService,
            plan: &'a Plan,
        }
        #[async_trait]
        impl PageFetcher for Samples<'_> {
            async fn fetch(
                &self,
                url: &str,
            ) -> Result<PageSample, zeus_workflow::planner::PlannerError> {
                let doc = self
                    .service
                    .network
                    .document(self.plan, url, None)
                    .await
                    .map_err(|_| {
                        zeus_workflow::planner::PlannerError::Fetch(
                            "workflow.document_unavailable".into(),
                        )
                    })?;
                let mut body = doc.body;
                while body.len() > 512 * 1024 {
                    body.pop();
                }
                Ok(PageSample {
                    url_redacted: zeus_workflow::runtime::display_url(&doc.final_url),
                    content_type: Some(doc.content_type),
                    body,
                })
            }
            async fn fetch_document(
                &self,
                url: &str,
            ) -> Result<Document, zeus_workflow::planner::PlannerError> {
                let mut doc = self
                    .service
                    .network
                    .document(self.plan, url, None)
                    .await
                    .map_err(|_| {
                        zeus_workflow::planner::PlannerError::Fetch(
                            "workflow.document_unavailable".into(),
                        )
                    })?;
                while doc.body.len() > 512 * 1024 {
                    doc.body.pop();
                }
                Ok(doc)
            }
        }
        let report = BoundedPlanner
            .dry_run(
                &plan,
                &Samples {
                    service: self,
                    plan: &plan,
                },
                p.sample_items,
            )
            .await
            .map_err(|_| {
                ApiError::new(
                    "workflow.dry_run_failed",
                    "bounded sampling did not complete",
                )
            })?;
        draft.dry = Some(report.clone());
        draft.updated_at = Timestamp::now();
        self.store.put(&format!("draft:{}", p.plan_id), &draft)?;
        Ok(report)
    }
    /// Reserve a run and request its exact human approval. Repeated keys return the same durable run.
    pub async fn run(
        &self,
        identity: &ClientIdentity,
        p: WorkflowsRunParams,
    ) -> ApiResult<WorkflowsRunResult> {
        let _gate = self.gate.lock().await;
        let binding = p
            .idempotency_key
            .as_ref()
            .map(|key| {
                if key.is_empty() || key.len() > 128 {
                    return Err(ApiError::invalid_params("invalid idempotency key"));
                }
                Ok(format!(
                    "key:{}",
                    blake3::hash(format!("{}:{key}", identity.credential_id).as_bytes()).to_hex()
                ))
            })
            .transpose()?;
        let existing = binding
            .as_ref()
            .map(|key| self.store.get::<RunId>(key))
            .transpose()?
            .flatten();
        let row = if let Some(id) = existing {
            let row = self.owned_run(identity, id)?;
            if row.snapshot.report.plan_hash != p.plan_hash || row.plan_id != p.plan_id {
                return Err(ApiError::new(
                    "api.idempotency_conflict",
                    "run key was already bound to another plan",
                ));
            }
            if let Some(mut receipt) = row.run_receipt.clone() {
                receipt.replayed = true;
                return Ok(receipt);
            }
            row
        } else {
            let draft = self.draft(identity, &p.plan_id)?;
            let plan = draft.plan.ok_or_else(missing)?;
            if plan.plan_hash().map_err(|_| fail())? != p.plan_hash {
                return Err(ApiError::new(
                    "workflow.plan_hash_mismatch",
                    "draft changed after review",
                ));
            }
            let dry = draft
                .dry
                .filter(|d| {
                    d.complete
                        && d.plan_hash == p.plan_hash
                        && Timestamp::now()
                            .unix_millis()
                            .saturating_sub(d.generated_at.unix_millis())
                            < 86_400_000
                })
                .ok_or_else(|| {
                    ApiError::new(
                        "workflow.dry_run_required",
                        "a complete recent dry run is required",
                    )
                })?;
            self.validate_environment(&plan)
                .await
                .map_err(runtime_error)?;
            if self
                .store
                .list::<StoredRun>("run:")?
                .iter()
                .filter(|(_, r)| {
                    matches!(
                        r.snapshot.report.status,
                        RunStatus::Running | RunStatus::Paused | RunStatus::AwaitingApproval
                    )
                })
                .count()
                >= 2
            {
                return Err(ApiError::new(
                    "workflow.too_many_runs",
                    "two active workflows are already reserved",
                ));
            }
            let runtime =
                WorkflowRuntime::new(plan.clone(), zeus_types::IdGenerator::new().run_id())
                    .map_err(runtime_error)?;
            let mut snapshot = runtime.snapshot().clone();
            snapshot.report.plan_id = Some(p.plan_id.clone());
            let row = StoredRun {
                owner: identity.credential_id.to_string(),
                plan_id: p.plan_id.clone(),
                plan,
                snapshot,
                approval_id: None,
                pending: vec![],
                jobs: BTreeMap::new(),
                exports: BTreeMap::new(),
                active_since: None,
                start_audited: false,
                run_receipt: None,
                requested_dry_run: Some(dry),
            };
            self.store.reserve(&row, binding.as_deref())?;
            row
        };
        let approval_id = if let Some(id) = row.approval_id {
            id
        } else {
            let dry = row
                .requested_dry_run
                .clone()
                .or_else(|| self.draft(identity, &row.plan_id).ok().and_then(|d| d.dry))
                .ok_or_else(|| {
                    ApiError::new(
                        "workflow.dry_run_required",
                        "run reservation is missing its dry run",
                    )
                })?;
            self.consent
                .request_workflow(
                    &row.plan_id,
                    row.snapshot.report.run_id,
                    &row.snapshot.report.plan_hash,
                    &row.plan,
                    &dry,
                    identity,
                )
                .await?
                .approval
                .id
        };
        self.store
            .mutate_run(row.snapshot.report.run_id, |current| {
                current.approval_id = Some(approval_id);
                current.snapshot.report.approval_id = Some(approval_id);
                let receipt = current
                    .run_receipt
                    .get_or_insert(WorkflowsRunResult {
                        replayed: false,
                        run_id: current.snapshot.report.run_id,
                        status: current.snapshot.report.status,
                        approval_id: Some(approval_id),
                    })
                    .clone();
                Ok(receipt)
            })
    }
    /// Read a redacted run report.
    pub fn get(&self, identity: &ClientIdentity, p: WorkflowsGetParams) -> ApiResult<RunReport> {
        self.public_report(self.owned_run(identity, p.run_id)?)
    }
    fn public_report(&self, row: StoredRun) -> ApiResult<RunReport> {
        let elapsed = self.store.elapsed(&row);
        let mut report = row.snapshot.report;
        report.plan_id = Some(row.plan_id);
        report.approval_id = row.approval_id;
        report.elapsed_ms = elapsed;
        if report.report_version == 0 {
            // Older 'started_at' was recorded at reservation, before human approval.
            report.created_at = report.created_at.or(report.started_at.take());
        }
        report.possibly_incomplete = matches!(
            report.status,
            RunStatus::Canceled
                | RunStatus::Failed
                | RunStatus::Stopped
                | RunStatus::BudgetExhausted
        ) || report
            .stop_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("budget_") || reason == "drift");
        if let Some(counts) = &mut report.counts {
            let jobs = self
                .journal
                .read(|tx| {
                    let mut ids = row
                        .jobs
                        .values()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>();
                    // An enqueue can commit before the outbox records its returned ID.
                    for intent in &row.pending {
                        if let Some(key) = &intent.spec.idempotency_key
                            && let Some(receipt) = tx.idempotency(&row.owner, key)?
                        {
                            ids.insert(receipt.job_id);
                        }
                    }
                    ids.into_iter()
                        .map(|id| tx.job(id))
                        .collect::<zeus_journal::JournalResult<Vec<_>>>()
                })
                .map_err(|_| fail())?;
            counts.files_enqueued = 0;
            counts.files_completed = 0;
            counts.files_failed = 0;
            counts.files_canceled = 0;
            counts.files_quarantined = 0;
            counts.bytes_done = 0;
            for record in jobs
                .into_iter()
                .flatten()
                .filter(|record| record.job.spec.source.run_id == Some(report.run_id))
            {
                counts.files_enqueued = counts.files_enqueued.saturating_add(1);
                counts.bytes_done = counts
                    .bytes_done
                    .saturating_add(record.job.progress.bytes_done);
                match record.job.state {
                    JobState::Completed => counts.files_completed += 1,
                    JobState::Failed => counts.files_failed += 1,
                    JobState::Canceled => counts.files_canceled += 1,
                    _ => {}
                }
                if record.job.quarantined {
                    counts.files_quarantined += 1;
                }
            }
            report.files_enqueued = counts.files_enqueued;
            report.files_completed = counts.files_completed;
            report.files_failed = counts.files_failed;
            report.bytes_done = counts.bytes_done;
        }
        Ok(report)
    }
    /// List only runs visible to this identity.
    pub fn list(
        &self,
        identity: &ClientIdentity,
        p: WorkflowsListParams,
    ) -> ApiResult<WorkflowsListResult> {
        p.page.validate()?;
        let offset = p
            .page
            .cursor
            .as_deref()
            .unwrap_or("0")
            .parse::<usize>()
            .map_err(|_| ApiError::invalid_params("invalid cursor"))?;
        let rows = self
            .store
            .list::<StoredRun>("run:")?
            .into_iter()
            .filter(|(_, r)| Self::owner(identity, &r.owner))
            .map(|(_, r)| r)
            .collect::<Vec<_>>();
        let limit = p.page.effective_limit();
        if offset > rows.len() {
            return Err(ApiError::invalid_params("invalid cursor"));
        }
        Ok(zeus_api::Page {
            next_cursor: (offset + limit < rows.len()).then(|| (offset + limit).to_string()),
            items: rows
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|row| self.public_report(row))
                .collect::<ApiResult<Vec<_>>>()?,
        })
    }
    /// Apply control to discovery and owned jobs.
    pub async fn control(
        &self,
        identity: &ClientIdentity,
        p: WorkflowsControlParams,
    ) -> ApiResult<WorkflowsControlResult> {
        let binding = Binding::new(
            identity,
            "workflows.control",
            p.idempotency_key.as_deref(),
            &p,
        )?;
        if let Some(mut result) = self
            .store
            .replay::<WorkflowsControlResult>(binding.as_ref())?
        {
            result.replayed = true;
            return Ok(result);
        }
        // Narrowing authority must not queue behind document I/O or crawl-delay.
        // The checkpoint compare-and-swap rejects any concurrent state mutation.
        let _gate = if p.action == RunControl::Resume {
            Some(self.gate.lock().await)
        } else {
            None
        };
        if let Some(mut result) = self
            .store
            .replay::<WorkflowsControlResult>(binding.as_ref())?
        {
            result.replayed = true;
            return Ok(result);
        }
        let row = self.owned_run(identity, p.run_id)?;
        let mut runtime =
            WorkflowRuntime::restore(row.plan, row.snapshot).map_err(runtime_error)?;
        runtime
            .control(
                &ControlPort {
                    service: self,
                    binding: binding.as_ref(),
                },
                match p.action {
                    RunControl::Pause => RunAction::Pause,
                    RunControl::Resume => RunAction::Resume,
                    RunControl::Cancel => RunAction::Cancel,
                },
            )
            .await
            .map_err(runtime_error)?;
        if binding.is_some() {
            let mut receipt = self
                .store
                .replay::<WorkflowsControlResult>(binding.as_ref())?
                .ok_or_else(fail)?;
            receipt.replayed = false;
            return Ok(receipt);
        }
        Ok(WorkflowsControlResult {
            replayed: false,
            run: self
                .store
                .run(p.run_id)?
                .ok_or_else(missing)?
                .snapshot
                .report,
        })
    }
    /// Export provenance records after the journal outbox has caught up.
    pub fn manifest(
        &self,
        identity: &ClientIdentity,
        p: WorkflowsManifestParams,
    ) -> ApiResult<WorkflowsManifestResult> {
        let _gate = self.manifest_gate.lock().map_err(|_| fail())?;
        self.owned_run(identity, p.run_id)?;
        let binding = Binding::new(
            identity,
            "workflows.manifest",
            p.idempotency_key.as_deref(),
            &p,
        )?;
        if let Some(mut result) = self
            .store
            .replay::<WorkflowsManifestResult>(binding.as_ref())?
        {
            result.replayed = true;
            return Ok(result);
        }
        let (path, records) = self.export_bound(p.run_id, p.format, binding.as_ref())?;
        let file = Path::new(&path);
        let digests = zeus_verify::hash::verify_file(file, &Default::default(), &mut |_| true)
            .map_err(|_| fail())?
            .0;
        let content = if digests.size_bytes <= 512 * 1024 {
            {
                use std::io::Read;
                let mut body = String::new();
                std::fs::File::open(file)
                    .map_err(|_| fail())?
                    .take(512 * 1024 + 1)
                    .read_to_string(&mut body)
                    .map_err(|_| fail())?;
                if zeus_verify::hash_bytes(body.as_bytes()).sha256 != digests.sha256 {
                    return Err(fail());
                }
                Some(body)
            }
        } else {
            None
        };
        if content.as_ref().is_some_and(|s| s.len() > 512 * 1024) {
            return Err(fail());
        }
        let result = WorkflowsManifestResult {
            replayed: false,
            path,
            records,
            content,
            sha256: Some(digests.sha256),
        };
        if let Some(binding) = &binding {
            self.store.finish_export(binding, &result)?;
        }
        Ok(result)
    }
    fn export(&self, run: RunId, format: ManifestFormat) -> ApiResult<(String, u64)> {
        self.export_bound(run, format, None)
    }
    fn export_bound(
        &self,
        run: RunId,
        format: ManifestFormat,
        binding: Option<&Binding>,
    ) -> ApiResult<(String, u64)> {
        let _gate = self.export_gate.lock().map_err(|_| fail())?;
        let row = self.store.run(run)?.ok_or_else(missing)?;
        let roots = zeus_fs::WriteRoots::open(&self.roots).map_err(|_| fail())?;
        let dest = roots
            .resolve_dest_dir(Path::new(&row.plan.sinks.dest_dir))
            .map_err(|_| fail())?;
        let root = roots.root_for(&dest).map_err(|_| fail())?;
        let (kind, ext) = match format {
            ManifestFormat::Jsonl => (zeus_verify::provenance::ExportFormat::Jsonl, "jsonl"),
            ManifestFormat::Csv => (zeus_verify::provenance::ExportFormat::Csv, "csv"),
            ManifestFormat::Croissant => (
                zeus_verify::provenance::ExportFormat::Croissant,
                "croissant.json",
            ),
        };
        let seq = self
            .journal
            .read(|t| {
                Ok(t.jobs(&Default::default())?
                    .iter()
                    .filter(|r| {
                        r.job.spec.source.run_id == Some(run) && r.job.state == JobState::Completed
                    })
                    .map(|r| r.internal.last_state_seq)
                    .max()
                    .unwrap_or(0))
            })
            .map_err(|_| fail())?;
        if self.journal.meta().map_err(|_| fail())?.manifest_cursor < seq {
            return Err(ApiError::new(
                "api.unavailable",
                "provenance export is waiting for durable records",
            ));
        }
        if let Some(receipt) = row.exports.get(ext).filter(|r| r.seq == seq) {
            let path = Path::new(&receipt.path);
            root.check_existing(path).map_err(|_| fail())?;
            let hash = zeus_verify::hash::verify_file(path, &Default::default(), &mut |_| true)
                .map_err(|_| fail())?
                .0
                .sha256;
            if hash != receipt.sha256 {
                return Err(ApiError::new(
                    "integrity.hash_mismatch",
                    "export was changed after creation",
                ));
            }
            return Ok((receipt.path.clone(), receipt.records));
        }
        let name = format!("zeus-run-{run}-{seq}");
        let mut output = dest.join(format!("{name}.manifest.{ext}"));
        for n in 0..100u32 {
            root.check_existing(&output).map_err(|_| fail())?;
            if !output.exists() {
                break;
            }
            output = dest.join(format!("{name}-{n}.manifest.{ext}"));
        }
        if output.exists() {
            return Err(fail());
        }
        let source = root.path().join(".zeus/manifest.jsonl");
        root.check_existing(&source).map_err(|_| fail())?;
        if let Some(binding) = binding {
            self.store.reserve_export(binding)?;
        }
        std::fs::create_dir_all(&dest).map_err(|_| fail())?;
        let records = zeus_verify::provenance::export_manifest(
            &source,
            &output,
            kind,
            &zeus_verify::provenance::ExportFilter {
                run_id: Some(run),
                strip_session_origin: true,
                strip_url_query: true,
                ..Default::default()
            },
        )
        .map_err(|_| fail())?;
        let sha256 = zeus_verify::hash::verify_file(&output, &Default::default(), &mut |_| true)
            .map_err(|_| fail())?
            .0
            .sha256;
        let path = output.display().to_string();
        self.store.mutate_run(run, |r| {
            r.exports.insert(
                ext.into(),
                ExportReceipt {
                    path: path.clone(),
                    records,
                    sha256,
                    seq,
                },
            );
            Ok(())
        })?;
        Ok((path, records))
    }
    /// Drive active runs from a dedicated task; do not put document I/O on the daemon's maintenance loop.
    pub async fn tick(&self) -> ApiResult<()> {
        if self.stopping.load(Ordering::SeqCst) {
            return Ok(());
        }
        let _gate = self.gate.lock().await;
        for (_, row) in self.store.list::<StoredRun>("run:")? {
            if matches!(
                row.snapshot.report.status,
                RunStatus::BudgetExhausted | RunStatus::Canceled | RunStatus::Paused
            ) {
                let _ = self
                    .control_jobs(
                        row.snapshot.report.run_id,
                        if row.snapshot.report.status == RunStatus::Paused {
                            RunAction::Pause
                        } else {
                            RunAction::Cancel
                        },
                    )
                    .await;
                continue;
            }
            if !matches!(
                row.snapshot.report.status,
                RunStatus::Running | RunStatus::AwaitingApproval
            ) {
                continue;
            }
            let id = row.snapshot.report.run_id;
            let approval = row
                .approval_id
                .map(|id| self.journal.read(|t| t.approval(id)))
                .transpose()
                .map_err(|_| fail())?
                .flatten();
            let Some(approval) = approval else { continue };
            if matches!(
                approval.state,
                ApprovalState::Denied | ApprovalState::Expired | ApprovalState::Canceled
            ) {
                let mut ended = row;
                ended.snapshot.report.status = RunStatus::Failed;
                ended.snapshot.report.stop_reason = Some("approval_denied".into());
                ended.snapshot.report.finished_at = Some(Timestamp::now());
                self.store.save_run(&ended)?;
                continue;
            }
            if approval.state != ApprovalState::Approved {
                continue;
            }
            if self
                .journal
                .meta()
                .map_err(|_| fail())?
                .kill_switch_engaged()
            {
                self.pause_runs("policy.kill_switch_engaged")?;
                continue;
            }
            if !row.start_audited {
                let mut entry = zeus_verify::AuditEntry::new(
                    Timestamp::now(),
                    zeus_types::Actor::system(),
                    "workflow.run_started",
                    zeus_verify::AuditOutcome::Ok,
                );
                entry.subject.run_id = Some(id);
                entry.data.insert(
                    "plan_blake3".into(),
                    serde_json::json!(row.snapshot.report.plan_hash),
                );
                self.append_audit_once(entry)?;
            }
            let active = self.store.mutate_run(id, |r| {
                if r.snapshot.report.status == RunStatus::AwaitingApproval {
                    r.snapshot.report.status = RunStatus::Running;
                    r.snapshot.checkpoint_seq = r.snapshot.checkpoint_seq.saturating_add(1);
                }
                if r.snapshot.report.status == RunStatus::Running {
                    r.start_audited = true;
                    if r.active_since.is_none() {
                        r.active_since = Some(self.store.now());
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            })?;
            if !active {
                continue;
            }
            if self.drain(id).await.is_err() {
                continue;
            }
            let row = self.store.run(id)?.ok_or_else(missing)?;
            let mut runtime =
                WorkflowRuntime::restore(row.plan, row.snapshot).map_err(runtime_error)?;
            let _ = runtime.step(self).await;
        }
        Ok(())
    }
    async fn drain(&self, run: RunId) -> RuntimeResult<()> {
        loop {
            self.authorize(
                run,
                &self
                    .store
                    .run(run)
                    .map_err(|_| runtime_fail())?
                    .ok_or_else(runtime_fail)?
                    .snapshot
                    .report
                    .plan_hash,
            )
            .await?;
            let row = self
                .store
                .run(run)
                .map_err(|_| runtime_fail())?
                .ok_or_else(runtime_fail)?;
            let Some(mut intent) = row.pending.first().cloned() else {
                return Ok(());
            };
            let root = zeus_fs::WriteRoots::open(&self.roots)
                .map_err(|_| RuntimeError::Policy("policy.path_outside_roots"))?;
            let dir = Path::new(intent.spec.dest_dir.as_deref().ok_or_else(runtime_fail)?);
            root.root_for(dir)
                .and_then(|r| r.check_existing(dir))
                .map_err(|_| RuntimeError::Policy("policy.path_outside_roots"))?;
            if intent.on_existing == OnExisting::Skip
                && intent
                    .spec
                    .filename
                    .as_ref()
                    .is_some_and(|name| dir.join(name).exists())
            {
                self.store
                    .mutate_run(run, |r| {
                        r.pending.remove(0);
                        r.snapshot.report.files_enqueued =
                            r.snapshot.report.files_enqueued.saturating_sub(1);
                        if let Some(counts) = &mut r.snapshot.report.counts {
                            counts.items_skipped_existing =
                                counts.items_skipped_existing.saturating_add(1);
                        }
                        Ok(())
                    })
                    .map_err(|_| runtime_fail())?;
                continue;
            }
            // Intent reservations are durable before admission; deterministic keys close the crash-after-enqueue gap.
            intent.spec.source.client_id = Some(row.owner.clone());
            let outcome = self
                .engine()?
                .enqueue(zeus_engine::EnqueueRequest {
                    spec: intent.spec.clone(),
                    caller: zeus_engine::Caller {
                        client_id: row.owner.clone(),
                        kind: zeus_types::ActorKind::System,
                    },
                })
                .await
                .map_err(|_| RuntimeError::Port("workflow.enqueue_failed"))?;
            let key = intent
                .spec
                .idempotency_key
                .clone()
                .ok_or_else(runtime_fail)?;
            self.store
                .mutate_run(run, |r| {
                    r.jobs.insert(key.clone(), outcome.job.id);
                    r.pending
                        .retain(|p| p.spec.idempotency_key.as_ref() != Some(&key));
                    Ok(())
                })
                .map_err(|_| runtime_fail())?;
        }
    }
}
#[async_trait]
impl RuntimePort for WorkflowService {
    async fn authorize(&self, run: RunId, hash: &str) -> RuntimeResult<()> {
        if self.stopping.load(Ordering::SeqCst) {
            return Err(RuntimeError::Port("workflow.shutting_down"));
        }
        if self
            .journal
            .meta()
            .map_err(|_| runtime_fail())?
            .kill_switch_engaged()
        {
            return Err(RuntimeError::Policy("policy.kill_switch_engaged"));
        }
        let row = self
            .store
            .run(run)
            .map_err(|_| runtime_fail())?
            .ok_or_else(runtime_fail)?;
        if row.snapshot.report.plan_hash != hash {
            return Err(RuntimeError::CheckpointConflict);
        }
        let a = self
            .journal
            .read(|t| {
                row.approval_id
                    .map(|id| t.approval(id))
                    .transpose()
                    .map(Option::flatten)
            })
            .map_err(|_| runtime_fail())?
            .ok_or(RuntimeError::Policy("policy.approval_required"))?;
        if a.state != ApprovalState::Approved
            || a.run_id != Some(run)
            || a.facts
                .as_ref()
                .and_then(|f| f.get("plan_blake3"))
                .and_then(|v| v.as_str())
                != Some(hash)
        {
            return Err(RuntimeError::Policy("policy.approval_required"));
        }
        Ok(())
    }
    async fn validate_environment(&self, plan: &Plan) -> RuntimeResult<()> {
        if plan.session.is_some() {
            return Err(RuntimeError::Policy("workflow.capability_missing"));
        }
        if plan.sinks.items.is_some() {
            return Err(RuntimeError::Policy("workflow.capability_missing"));
        }
        if !plan.post.convert.is_empty() {
            return Err(RuntimeError::Policy("plan.convert_unavailable"));
        }
        if plan.sinks.on_existing == OnExisting::Replace {
            return Err(RuntimeError::Policy(
                "workflow.destructive_capability_missing",
            ));
        }
        self.resolved_plan(plan.clone())
            .map_err(|_| RuntimeError::Policy("policy.path_outside_roots"))?;
        zeus_workflow::pagination::frontier(plan).map_err(|d| RuntimeError::Invalid(vec![d]))?;
        Ok(())
    }
    async fn fetch(&self, request: FetchRequest) -> RuntimeResult<Document> {
        let row = self
            .store
            .run(request.run_id)
            .map_err(|_| runtime_fail())?
            .ok_or_else(runtime_fail)?;
        self.network
            .document(&row.plan, &request.url, Some(request.run_id))
            .await
    }
    async fn job_totals(&self, run: RunId) -> RuntimeResult<JobTotals> {
        let row = self
            .store
            .run(run)
            .map_err(|_| runtime_fail())?
            .ok_or_else(runtime_fail)?;
        let mut totals = JobTotals::default();
        for id in row.jobs.values() {
            let job = self.engine()?.get(*id).await.map_err(|_| runtime_fail())?;
            totals.bytes_done = totals.bytes_done.saturating_add(job.progress.bytes_done);
            match job.state {
                JobState::Completed => totals.completed += 1,
                JobState::Failed => totals.failed += 1,
                JobState::Canceled => {}
                _ => {
                    totals.active += 1;
                    totals.bytes_reserved = totals.bytes_reserved.saturating_add(
                        job.progress
                            .bytes_total
                            .or(job.spec.expected_size)
                            .unwrap_or(0)
                            .saturating_sub(job.progress.bytes_done),
                    );
                }
            }
        }
        totals.active += row.pending.len() as u32;
        for intent in row.pending {
            totals.bytes_reserved = totals
                .bytes_reserved
                .saturating_add(intent.declared_size.unwrap_or(0));
        }
        Ok(totals)
    }
    async fn commit(
        &self,
        expected: u64,
        next: &RunSnapshot,
        jobs: &[JobIntent],
    ) -> RuntimeResult<()> {
        if !jobs.is_empty()
            || matches!(
                next.report.status,
                RunStatus::Running | RunStatus::AwaitingApproval
            )
        {
            self.authorize(next.report.run_id, &next.report.plan_hash)
                .await?;
        }
        self.store.commit(expected, next, jobs)
    }
    async fn control_jobs(&self, run: RunId, action: RunAction) -> RuntimeResult<()> {
        let row = self
            .store
            .run(run)
            .map_err(|_| runtime_fail())?
            .ok_or_else(runtime_fail)?;
        for id in row.jobs.values() {
            let job = self.engine()?.get(*id).await.map_err(|_| runtime_fail())?;
            let action = match action {
                RunAction::Pause
                    if matches!(
                        job.state,
                        JobState::Queued
                            | JobState::Probing
                            | JobState::Downloading
                            | JobState::RetryScheduled
                    ) =>
                {
                    zeus_engine::ControlAction::Pause
                }
                RunAction::Resume if job.state == JobState::Paused => {
                    zeus_engine::ControlAction::Resume
                }
                RunAction::Cancel
                    if !matches!(
                        job.state,
                        JobState::Completed
                            | JobState::Canceled
                            | JobState::Verifying
                            | JobState::Finalizing
                    ) =>
                {
                    zeus_engine::ControlAction::Cancel
                }
                _ => continue,
            };
            let _ = self.engine()?.control_workflow(*id, action).await;
        }
        Ok(())
    }
    async fn finish(&self, plan: &Plan, snapshot: &RunSnapshot) -> RuntimeResult<()> {
        if let Some(manifest) = &plan.sinks.manifest {
            self.export(snapshot.report.run_id, manifest.format)
                .map_err(|_| RuntimeError::Port("workflow.manifest_pending"))?;
        }
        let mut entry = zeus_verify::AuditEntry::new(
            Timestamp::now(),
            zeus_types::Actor::system(),
            "workflow.run_ended",
            zeus_verify::AuditOutcome::Ok,
        );
        entry.subject.run_id = Some(snapshot.report.run_id);
        entry.data.insert(
            "status".into(),
            serde_json::to_value(snapshot.report.status).map_err(|_| runtime_fail())?,
        );
        entry.data.insert(
            "plan_blake3".into(),
            serde_json::json!(snapshot.report.plan_hash),
        );
        self.append_audit_once(entry)
            .map_err(|_| RuntimeError::Port("audit.unavailable"))?;
        Ok(())
    }
}
fn sampling_plan(urls: &[String]) -> ApiResult<Plan> {
    let hosts = urls
        .iter()
        .map(|text| {
            url::Url::parse(text)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .ok_or_else(|| ApiError::invalid_params("invalid sample URL"))
        })
        .collect::<ApiResult<Vec<_>>>()?;
    serde_json::from_value(serde_json::json!({"plan_version":0,"name":"Bounded page samples","allowed_domains":hosts,"sources":[{"type":"start_urls","urls":urls}],"extract":{"items":{"kind":"css","query":"a[href]"},"fields":{"url":{"extract":{"kind":"attr","name":"href"}}}},"fetch":{"min_interval_ms":200,"max_concurrency_per_host":1},"budgets":{"max_pages":3,"max_files":1,"max_bytes":1,"max_duration_ms":60000,"max_depth":1},"sinks":{"dest_dir":"samples"}})).map_err(|_|fail())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn receipt_service(path: &Path) -> Arc<WorkflowService> {
        let journal = zeus_journal::Journal::open(
            &path.join("journal.redb"),
            &Default::default(),
            Arc::new(zeus_control::SystemClock::new()),
        )
        .unwrap();
        let audit = Arc::new(zeus_verify::MemoryAuditLog::new(
            zeus_types::IdGenerator::new().chain_id(),
            zeus_types::IdGenerator::new().store_id(),
            "test",
            Timestamp::now(),
            None,
        ));
        let consent = Arc::new(crate::consent::ConsentManager::new(
            journal.clone(),
            audit.clone(),
            Arc::new(zeus_api::auth::MemoryCredentialStore::default()),
        ));
        WorkflowService::open(
            &path.join("workflow.redb"),
            journal,
            consent,
            audit,
            vec![path.to_owned()],
        )
        .unwrap()
    }
    fn receipt_identity() -> ClientIdentity {
        ClientIdentity {
            credential_id: zeus_api::auth::CredentialId::generate(),
            kind: ClientKind::Human,
            role: zeus_api::ClientRole::Cli,
            permissions: Default::default(),
            client_name: "test".into(),
        }
    }
    #[tokio::test]
    async fn keyed_plan_replays_after_restart_without_overwriting_a_later_draft() {
        let dir = tempfile::tempdir().unwrap();
        let service = receipt_service(dir.path());
        let identity = receipt_identity();
        let row = row();
        let key = format!("draft:{}", row.plan_id);
        let mut draft = Draft {
            owner: identity.credential_id.to_string(),
            plan: Some(row.plan),
            samples: vec![PageSample {
                url_redacted: "https://example.org/list".into(),
                content_type: Some("text/html".into()),
                body: "<a href='/file'>File</a>".into(),
            }],
            updated_at: Timestamp::now(),
            dry: None,
        };
        service.store.put(&key, &draft).unwrap();
        let params = WorkflowsPlanParams {
            idempotency_key: Some("plan-retry".into()),
            goal: "Download linked files".into(),
            start_urls: vec!["https://example.org/list".into()],
            session_origin: None,
            candidate: None,
            plan_id: Some(row.plan_id),
            draft: None,
        };
        let first = service.plan(&identity, params.clone()).await.unwrap();
        draft.samples[0].body = "later draft".into();
        service.store.put(&key, &draft).unwrap();
        drop(service);
        let service = receipt_service(dir.path());
        let mut replay = service.plan(&identity, params.clone()).await.unwrap();
        assert!(replay.replayed);
        replay.replayed = false;
        assert_eq!(first, replay);
        assert_eq!(
            service.store.get::<Draft>(&key).unwrap().unwrap().samples[0].body,
            "later draft"
        );
        let mut changed = params;
        changed.goal = "A different goal".into();
        assert_eq!(
            service
                .plan(&identity, changed)
                .await
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "api.idempotency_conflict"
        );
    }

    #[tokio::test]
    async fn keyed_control_replays_after_restart_without_repeating_an_old_pause() {
        let dir = tempfile::tempdir().unwrap();
        let service = receipt_service(dir.path());
        let identity = receipt_identity();
        let mut row = row();
        row.owner = identity.credential_id.to_string();
        row.snapshot.report.status = RunStatus::Running;
        let run = row.snapshot.report.run_id;
        service.store.reserve(&row, None).unwrap();
        let params = WorkflowsControlParams {
            idempotency_key: Some("control-retry".into()),
            run_id: run,
            action: RunControl::Pause,
        };
        let first = service.control(&identity, params.clone()).await.unwrap();
        assert_eq!(first.run.status, RunStatus::Paused);
        // A later independent action has already resumed this run.
        service
            .store
            .mutate_run(run, |row| {
                row.snapshot.report.status = RunStatus::Running;
                row.snapshot.checkpoint_seq += 1;
                Ok(())
            })
            .unwrap();
        drop(service);
        let service = receipt_service(dir.path());
        let mut replay = service.control(&identity, params.clone()).await.unwrap();
        assert!(replay.replayed);
        replay.replayed = false;
        assert_eq!(first, replay);
        let current = service.store.run(run).unwrap().unwrap();
        assert_eq!(current.snapshot.report.status, RunStatus::Running);
        assert_eq!(current.snapshot.checkpoint_seq, 2);
        let mut changed = params.clone();
        changed.action = RunControl::Cancel;
        assert_eq!(
            service
                .control(&identity, changed)
                .await
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "api.idempotency_conflict"
        );
        // Keys are scoped to the credential; another human's independent intent can pause.
        assert!(
            !service
                .control(&receipt_identity(), params)
                .await
                .unwrap()
                .replayed
        );
        assert_eq!(
            service
                .store
                .run(run)
                .unwrap()
                .unwrap()
                .snapshot
                .checkpoint_seq,
            3
        );
    }

    #[test]
    fn keyed_manifest_replays_a_snapshot_and_uncertain_publication_never_exports_again() {
        let dir = tempfile::tempdir().unwrap();
        let service = receipt_service(dir.path());
        let identity = receipt_identity();
        let mut row = row();
        row.plan.sinks.dest_dir = dir.path().display().to_string();
        let run = row.snapshot.report.run_id;
        service.store.reserve(&row, None).unwrap();
        std::fs::create_dir_all(dir.path().join(".zeus")).unwrap();
        let header = serde_json::json!({
            "record_type":"header", "manifest_version":0, "created_at":Timestamp::now(),
            "generator":{"name":"zeus","version":"test"},
            "privacy":{"url_query":"dropped","session_origin_included":false,"os_metadata":"off"},
            "hash_algorithms":["sha256","blake3"]
        });
        std::fs::write(
            dir.path().join(".zeus/manifest.jsonl"),
            format!("{header}\n"),
        )
        .unwrap();
        let params = WorkflowsManifestParams {
            idempotency_key: Some("export-retry".into()),
            run_id: run,
            format: ManifestFormat::Jsonl,
        };
        let first = service.manifest(&identity, params.clone()).unwrap();
        // The current run export index can change without changing this intent's receipt.
        service
            .store
            .mutate_run(run, |row| {
                row.exports.clear();
                Ok(())
            })
            .unwrap();
        drop(service);
        let service = receipt_service(dir.path());
        let mut replay = service.manifest(&identity, params.clone()).unwrap();
        assert!(replay.replayed);
        replay.replayed = false;
        assert_eq!(first, replay);
        let mut changed = params.clone();
        changed.format = ManifestFormat::Csv;
        assert_eq!(
            service
                .manifest(&identity, changed)
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "api.idempotency_conflict"
        );
        let mut interrupted = params;
        interrupted.idempotency_key = Some("export-interrupted".into());
        let binding = Binding::new(
            &identity,
            "workflows.manifest",
            interrupted.idempotency_key.as_deref(),
            &interrupted,
        )
        .unwrap()
        .unwrap();
        service.store.reserve_export(&binding).unwrap();
        drop(service);
        let service = receipt_service(dir.path());
        assert_eq!(
            service
                .manifest(&identity, interrupted)
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "api.conflict"
        );
        let exports = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".manifest."))
            .count();
        assert_eq!(exports, 1);
    }
    fn row() -> StoredRun {
        let plan = sampling_plan(&["https://example.org/list".into()]).unwrap();
        let run = zeus_types::IdGenerator::new().run_id();
        StoredRun {
            owner: "owner".into(),
            plan_id: PlanId::parse(&run.to_string().replacen("run_", "plan_", 1)).unwrap(),
            snapshot: WorkflowRuntime::new(plan.clone(), run)
                .unwrap()
                .snapshot()
                .clone(),
            plan,
            approval_id: None,
            pending: vec![],
            jobs: BTreeMap::new(),
            exports: BTreeMap::new(),
            active_since: None,
            start_audited: false,
            run_receipt: None,
            requested_dry_run: None,
        }
    }

    fn report_intent(run_id: RunId, key: &str, index: u32) -> JobIntent {
        let mut source = zeus_types::Source::new(zeus_types::SourceKind::Workflow);
        source.run_id = Some(run_id);
        let mut spec = zeus_types::JobSpec::new(
            zeus_types::HttpUrl::parse("https://example.org/report-file").unwrap(),
            source,
        );
        spec.idempotency_key = Some(key.into());
        JobIntent {
            spec,
            index,
            on_existing: OnExisting::Skip,
            byte_budget_remaining: 1024,
            declared_size: None,
        }
    }

    fn report_job(run_id: RunId, state: JobState, bytes_done: u64) -> zeus_journal::JobRecord {
        let spec = report_intent(run_id, "fixture-only", 1).spec;
        let mut job = zeus_types::Job::new(
            zeus_types::IdGenerator::new().job_id(),
            spec,
            state,
            Timestamp::now(),
        );
        job.progress.bytes_done = bytes_done;
        let internal = serde_json::from_value(serde_json::json!({
            "epoch": 1,
            "dest_path": "report-fixture.bin",
            "zpart_path": "report-fixture.bin.zpart",
            "zpart_file_id": null,
            "total_bytes": null,
            "data_complete": false,
            "bound_validators": {},
            "range_support": "unknown"
        }))
        .unwrap();
        zeus_journal::JobRecord::new(job, internal)
    }

    #[test]
    fn public_report_uses_current_durable_job_states_and_excludes_pending_intents() {
        let dir = tempfile::tempdir().unwrap();
        let service = receipt_service(dir.path());
        let mut stored = row();
        let run_id = stored.snapshot.report.run_id;
        stored.snapshot.report.files_enqueued = 99;
        stored.snapshot.report.files_completed = 98;
        stored.snapshot.report.files_failed = 97;
        stored.snapshot.report.bytes_done = 96;
        stored.snapshot.report.counts = Some(RunCounts {
            pages_fetched: 4,
            pages_failed: 2,
            items_seen: 12,
            items_duplicate: 3,
            items_filtered: 2,
            files_enqueued: 99,
            files_completed: 98,
            files_failed: 97,
            files_canceled: 96,
            files_quarantined: 95,
            bytes_done: 94,
            ..Default::default()
        });
        stored.pending = vec![
            report_intent(run_id, "wf:report:not-admitted-one", 1),
            report_intent(run_id, "wf:report:not-admitted-two", 2),
        ];
        let downloading = report_job(run_id, JobState::Downloading, 100);
        let mut completed = report_job(run_id, JobState::Completed, 200);
        completed.job.quarantined = true;
        completed.job.quarantine_reasons = vec![zeus_types::QuarantineReason::ExecutableType];
        let mut failed = report_job(run_id, JobState::Failed, 30);
        let canceled = report_job(run_id, JobState::Canceled, 40);
        let foreign = report_job(
            zeus_types::IdGenerator::new().run_id(),
            JobState::Completed,
            9000,
        );
        for (index, record) in [&downloading, &completed, &failed, &canceled, &foreign]
            .into_iter()
            .enumerate()
        {
            stored.jobs.insert(format!("intent-{index}"), record.id());
        }
        // Duplicate outbox mappings and stale missing IDs must not inflate the totals.
        stored
            .jobs
            .insert("same-completed-job".into(), completed.id());
        stored.jobs.insert(
            "missing-job".into(),
            zeus_types::IdGenerator::new().job_id(),
        );
        service
            .journal
            .transact(|tx| {
                for record in [&downloading, &completed, &failed, &canceled, &foreign] {
                    tx.put_job(record)?;
                }
                Ok(())
            })
            .unwrap();

        let first = service.public_report(stored.clone()).unwrap();
        let counts = first.counts.as_ref().unwrap();
        assert_eq!(counts.files_enqueued, 4);
        assert_eq!(counts.files_completed, 1);
        assert_eq!(counts.files_failed, 1);
        assert_eq!(counts.files_canceled, 1);
        assert_eq!(counts.files_quarantined, 1);
        assert_eq!(counts.bytes_done, 370);
        assert_eq!((counts.pages_fetched, counts.pages_failed), (4, 2));
        assert_eq!(
            (
                counts.items_seen,
                counts.items_duplicate,
                counts.items_filtered
            ),
            (12, 3, 2)
        );
        assert_eq!(
            (
                first.files_enqueued,
                first.files_completed,
                first.files_failed,
                first.bytes_done
            ),
            (4, 1, 1, 370)
        );

        // Engine state may advance without another workflow checkpoint being committed.
        failed.job.state = JobState::Completed;
        failed.job.progress.bytes_done = 50;
        service.journal.transact(|tx| tx.put_job(&failed)).unwrap();
        let current = service.public_report(stored).unwrap();
        let counts = current.counts.as_ref().unwrap();
        assert_eq!(
            (
                counts.files_enqueued,
                counts.files_completed,
                counts.files_failed
            ),
            (4, 2, 0)
        );
        assert_eq!(counts.bytes_done, 390);
        assert_eq!(
            (
                current.files_enqueued,
                current.files_completed,
                current.files_failed,
                current.bytes_done
            ),
            (4, 2, 0, 390)
        );
    }

    #[test]
    fn public_report_recovers_admitted_jobs_before_outbox_mapping_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let service = receipt_service(dir.path());
        let mut stored = row();
        let run_id = stored.snapshot.report.run_id;
        let key = "wf:report:receipt-before-mapping";
        stored.pending = vec![
            report_intent(run_id, key, 1),
            report_intent(run_id, "wf:report:other-owner", 2),
            report_intent(run_id, "wf:report:no-receipt", 3),
        ];
        assert!(stored.jobs.is_empty());
        let before = service.public_report(stored.clone()).unwrap();
        assert_eq!(before.counts.unwrap().files_enqueued, 0);
        assert_eq!(before.files_enqueued, 0);

        let admitted = report_job(run_id, JobState::Downloading, 64);
        let hidden_receipt_job = report_job(run_id, JobState::Completed, 9000);
        let now = Timestamp::now();
        service
            .journal
            .transact(|tx| {
                tx.put_job(&admitted)?;
                tx.put_job(&hidden_receipt_job)?;
                for (owner, receipt_key, job_id) in [
                    (stored.owner.as_str(), key, admitted.id()),
                    (
                        "another-requester",
                        "wf:report:other-owner",
                        hidden_receipt_job.id(),
                    ),
                ] {
                    tx.put_idempotency(&zeus_journal::IdempotencyRow {
                        client_id: owner.into(),
                        key: receipt_key.into(),
                        request_hash: zeus_types::Hex256::from_bytes([0; 32]),
                        job_id,
                        method: Some("jobs.enqueue".into()),
                        result: None,
                        created_at: now,
                        expires_at: now.saturating_add_ms(86_400_000),
                    })?;
                }
                Ok(())
            })
            .unwrap();

        // The durable engine receipt closes the enqueue-before-outbox-checkpoint gap.
        let receipt_only = service.public_report(stored.clone()).unwrap();
        let counts = receipt_only.counts.unwrap();
        assert_eq!(
            (
                counts.files_enqueued,
                counts.files_completed,
                counts.bytes_done
            ),
            (1, 0, 64)
        );
        assert_eq!(
            (receipt_only.files_enqueued, receipt_only.bytes_done),
            (1, 64)
        );
        stored.jobs.insert("now-mapped".into(), admitted.id());
        let mapped = service.public_report(stored).unwrap();
        assert_eq!(mapped.counts.unwrap().files_enqueued, 1);
        assert_eq!(mapped.files_enqueued, 1);
    }

    #[test]
    fn public_report_keeps_legacy_counts_unknown_and_relabels_reservation_time() {
        let dir = tempfile::tempdir().unwrap();
        let service = receipt_service(dir.path());
        let reservation = Timestamp::UNIX_EPOCH.saturating_add_ms(1000);
        let known_creation = Timestamp::UNIX_EPOCH.saturating_add_ms(500);
        for created_at in [None, Some(known_creation)] {
            let mut legacy = row();
            legacy.snapshot.report.report_version = 0;
            legacy.snapshot.report.counts = None;
            legacy.snapshot.report.status = RunStatus::AwaitingApproval;
            legacy.snapshot.report.created_at = created_at;
            legacy.snapshot.report.started_at = Some(reservation);
            legacy.snapshot.report.files_enqueued = 7;
            legacy.snapshot.report.files_completed = 5;
            legacy.snapshot.report.files_failed = 2;
            legacy.snapshot.report.bytes_done = 123;
            let run_id = legacy.snapshot.report.run_id;
            let record = report_job(run_id, JobState::Completed, 999);
            legacy.jobs.insert("legacy-job".into(), record.id());
            service.journal.transact(|tx| tx.put_job(&record)).unwrap();

            let report = service.public_report(legacy).unwrap();
            assert_eq!(report.report_version, 0);
            assert!(
                report.counts.is_none(),
                "unknown discovery counts must stay unknown"
            );
            assert_eq!(report.created_at, created_at.or(Some(reservation)));
            assert_eq!(report.started_at, None);
            assert_eq!(
                (
                    report.files_enqueued,
                    report.files_completed,
                    report.files_failed,
                    report.bytes_done
                ),
                (7, 5, 2, 123)
            );
        }
    }

    #[test]
    fn durable_intents_and_keys_commit_together_and_stale_checkpoints_cannot_undo_pause() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("workflow.redb");
        let store = WorkflowStore::open(&path).unwrap();
        let row = row();
        let id = row.snapshot.report.run_id;
        store.reserve(&row, Some("key:one")).unwrap();
        assert!(store.reserve(&row, Some("key:one")).is_err());
        let mut next = row.snapshot.clone();
        next.checkpoint_seq = 1;
        next.report.status = RunStatus::Running;
        next.report.files_enqueued = 1;
        let mut source = zeus_types::Source::new(zeus_types::SourceKind::Workflow);
        source.run_id = Some(id);
        let mut spec = zeus_types::JobSpec::new(
            zeus_types::HttpUrl::parse("https://example.org/file").unwrap(),
            source,
        );
        spec.idempotency_key = Some("wf:test:one".into());
        let intent = JobIntent {
            spec,
            index: 1,
            on_existing: OnExisting::Skip,
            byte_budget_remaining: 1,
            declared_size: None,
        };
        store.commit(0, &next, &[intent]).unwrap();
        store
            .mutate_run(id, |row| {
                row.snapshot.report.status = RunStatus::Paused;
                row.snapshot.checkpoint_seq += 1;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            store.commit(1, &next, &[]),
            Err(RuntimeError::CheckpointConflict)
        ));
        drop(store);
        let reopened = WorkflowStore::open(&path).unwrap();
        let actual = reopened.run(id).unwrap().unwrap();
        assert_eq!(actual.snapshot.report.status, RunStatus::Paused);
        assert_eq!(actual.pending.len(), 1);
        assert_eq!(reopened.get::<RunId>("key:one").unwrap(), Some(id));
    }
    #[test]
    fn exhausting_budget_discards_pending_intents_and_invalidates_inflight_checkpoint() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = WorkflowStore::open(&dir.path().join("workflow.redb")).unwrap();
        let mut row = row();
        row.snapshot.report.status = RunStatus::Running;
        row.active_since = Some(store.now());
        let id = row.snapshot.report.run_id;
        store.reserve(&row, None).unwrap();
        store.exhausted(id, "budget_bytes").unwrap();
        let actual = store.run(id).unwrap().unwrap();
        assert_eq!(actual.snapshot.report.status, RunStatus::BudgetExhausted);
        assert_eq!(
            actual.snapshot.report.stop_reason.as_deref(),
            Some("budget_bytes")
        );
        assert!(actual.active_since.is_none());
        assert!(matches!(
            store.commit(0, &row.snapshot, &[]),
            Err(RuntimeError::CheckpointConflict)
        ));
    }
    #[tokio::test(start_paused = true)]
    async fn pause_cancel_and_kill_interrupt_hour_long_pacing_without_waiting_for_tick() {
        for action in [0, 1, 2] {
            let dir = tempfile::TempDir::new().unwrap();
            let journal = zeus_journal::Journal::open(
                &dir.path().join("journal.redb"),
                &Default::default(),
                Arc::new(zeus_control::SystemClock::new()),
            )
            .unwrap();
            let audit = Arc::new(zeus_verify::MemoryAuditLog::new(
                zeus_types::IdGenerator::new().chain_id(),
                zeus_types::IdGenerator::new().store_id(),
                "test",
                Timestamp::now(),
                None,
            ));
            let consent = Arc::new(crate::consent::ConsentManager::new(
                journal.clone(),
                audit.clone(),
                Arc::new(zeus_api::auth::MemoryCredentialStore::default()),
            ));
            let service = WorkflowService::open(
                &dir.path().join("workflow.redb"),
                journal.clone(),
                consent,
                audit,
                vec![dir.path().to_owned()],
            )
            .unwrap();
            let identity = ClientIdentity {
                credential_id: zeus_api::auth::CredentialId::generate(),
                kind: ClientKind::Human,
                role: zeus_api::ClientRole::Cli,
                permissions: Default::default(),
                client_name: "test".into(),
            };
            let mut stored = row();
            stored.owner = identity.credential_id.to_string();
            stored.snapshot.report.status = RunStatus::Running;
            let run = stored.snapshot.report.run_id;
            let approval_id = zeus_types::IdGenerator::new().approval_id();
            stored.approval_id = Some(approval_id);
            let now = Timestamp::now();
            let approval: zeus_types::Approval = serde_json::from_value(serde_json::json!({
                "id": approval_id, "kind": "workflow_plan", "state": "approved",
                "run_id": run, "allowed_scopes": [], "reason_codes": [],
                "summary": {"title":"Test plan"}, "created_at": now,
                "expires_at": now.saturating_add_ms(86_400_000),
                "facts": {"plan_blake3": stored.snapshot.report.plan_hash}
            }))
            .unwrap();
            journal.transact(|tx| tx.put_approval(&approval)).unwrap();
            service.store.reserve(&stored, None).unwrap();
            let (started, ready) = tokio::sync::oneshot::channel();
            let runner = service.clone();
            let task = tokio::spawn(async move {
                let _tick = runner.gate.lock().await;
                started.send(()).unwrap();
                runner
                    .network
                    .gate(
                        &url::Url::parse("https://example.org/file").unwrap(),
                        3_600_000,
                        1,
                        Some(run),
                    )
                    .await
                    .is_err()
            });
            ready.await.unwrap();
            tokio::task::yield_now().await;
            assert!(
                !task.is_finished(),
                "long pacing must be pending before control"
            );
            let expected = match action {
                0 | 1 => {
                    let control = if action == 0 {
                        RunControl::Pause
                    } else {
                        RunControl::Cancel
                    };
                    let result = tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        service.control(
                            &identity,
                            WorkflowsControlParams {
                                idempotency_key: Some(format!("test-control-{action}")),
                                run_id: run,
                                action: control,
                            },
                        ),
                    )
                    .await
                    .expect("control queued behind the tick gate")
                    .unwrap();
                    result.run.status
                }
                _ => {
                    journal.transact(|tx| tx.set_kill_switch(true)).unwrap();
                    service.pause_all_for_kill_switch().await.unwrap();
                    RunStatus::Paused
                }
            };
            assert_eq!(
                service
                    .store
                    .run(run)
                    .unwrap()
                    .unwrap()
                    .snapshot
                    .report
                    .status,
                expected
            );
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(250), task)
                    .await
                    .expect("pacing ignored durable authority change")
                    .unwrap()
            );
        }
    }

    #[tokio::test]
    async fn audit_end_receipt_recovers_the_append_before_checkpoint_crash_gap() {
        let dir = tempfile::TempDir::new().unwrap();
        let journal = zeus_journal::Journal::open(
            &dir.path().join("journal.redb"),
            &Default::default(),
            Arc::new(zeus_control::SystemClock::new()),
        )
        .unwrap();
        let audit = Arc::new(
            zeus_verify::AuditLog::open(
                &dir.path().join("audit"),
                None,
                journal.meta().unwrap().store_id,
                "test",
                Default::default(),
            )
            .unwrap(),
        );
        let make = || {
            let consent = Arc::new(crate::consent::ConsentManager::new(
                journal.clone(),
                audit.clone(),
                Arc::new(zeus_api::auth::MemoryCredentialStore::default()),
            ));
            WorkflowService::open(
                &dir.path().join("workflow.redb"),
                journal.clone(),
                consent,
                audit.clone(),
                vec![dir.path().to_owned()],
            )
            .unwrap()
        };
        let service = make();
        let stored = row();
        service.store.reserve(&stored, None).unwrap();
        let mut ended = stored.snapshot.clone();
        ended.report.status = RunStatus::Completed;
        service.finish(&stored.plan, &ended).await.unwrap();
        service.finish(&stored.plan, &ended).await.unwrap();
        // Deliberately leave the workflow checkpoint uncommitted, as a crash would.
        drop(service);
        let reopened = make();
        reopened.recover_audit_receipts(audit.as_ref()).unwrap();
        reopened.finish(&stored.plan, &ended).await.unwrap();
        let mut records = 0;
        audit
            .visit_records(|record| {
                if record.action == "workflow.run_ended"
                    && record.subject.run_id == Some(ended.report.run_id)
                {
                    records += 1;
                    assert!(record.data.get("workflow_receipt").is_some());
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(records, 1);
    }
}
