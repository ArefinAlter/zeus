//! Durable, bounded JSONL item sink. Admission and its outcome are one database transaction;
//! exact file identity, prefix hash and pending bytes close the append-before-receipt crash gap.
use super::*;
use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
};
use zeus_fs::{DirectoryCap, opened_file_metadata};
use zeus_types::FileId;

const MAX_SINK_BYTES: u64 = ITEM_SINK_MAX_BYTES;
const MAX_RECORD_BYTES: usize = 1024 * 1024;
const BATCH_RECORDS: usize = 128;
const BATCH_BYTES: usize = 1024 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct ItemSinkState {
    receipt: Option<ItemReceipt>,
    pending: Vec<ItemOutcome>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    retired: BTreeMap<String, ItemOutcome>,
}
#[derive(Clone, Serialize, Deserialize)]
struct ItemReceipt {
    root_id: FileId,
    directory_id: FileId,
    file_id: FileId,
    bytes: u64,
    records: u64,
    blake3: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct ItemOutcome {
    index: u32,
    item: ItemDraft,
    dest_dir: String,
    filename: String,
    job_id: Option<JobId>,
    recorded_at: Timestamp,
}
#[derive(Serialize)]
struct ItemRecord<'a> {
    record_type: &'static str,
    run_id: RunId,
    index: u32,
    page_url: &'a str,
    url: &'a str,
    url_redacted: bool,
    fields: &'a BTreeMap<String, String>,
    action: &'static str,
    job_id: Option<JobId>,
    path: String,
    recorded_at: Timestamp,
}
impl ItemSinkState {
    pub(super) fn report(&self, run: RunId) -> ItemSinkReport {
        ItemSinkReport {
            file_name: format!("zeus-run-{run}.items.jsonl"),
            records: self.receipt.as_ref().map_or(0, |r| r.records),
            bytes: self.receipt.as_ref().map_or(0, |r| r.bytes),
            pending_records: self.pending.len().try_into().unwrap_or(u32::MAX),
            max_bytes: MAX_SINK_BYTES,
        }
    }
    pub(super) fn record(&mut self, intent: &JobIntent, job_id: Option<JobId>) {
        if let Some(item) = intent.item.clone() {
            self.pending.push(ItemOutcome {
                index: intent.index,
                item,
                dest_dir: intent.spec.dest_dir.clone().unwrap_or_default(),
                filename: intent.spec.filename.clone().unwrap_or_default(),
                job_id,
                recorded_at: Timestamp::now(),
            });
        }
    }
    pub(super) fn retire(&mut self, intent: &JobIntent) {
        if let (Some(item), Some(key)) =
            (intent.item.as_ref(), intent.spec.idempotency_key.as_ref())
        {
            self.retired
                .entry(key.clone())
                .or_insert_with(|| ItemOutcome {
                    index: intent.index,
                    item: item.clone(),
                    dest_dir: intent.spec.dest_dir.clone().unwrap_or_default(),
                    filename: intent.spec.filename.clone().unwrap_or_default(),
                    job_id: None,
                    recorded_at: Timestamp::now(),
                });
        }
    }
    pub(super) fn admit_retired(&mut self, key: &str, job_id: JobId) {
        if let Some(mut outcome) = self.retired.remove(key) {
            outcome.job_id = Some(job_id);
            outcome.recorded_at = Timestamp::now();
            self.pending.push(outcome);
        }
    }
    pub(super) fn ready(&self) -> bool {
        self.pending.len() >= BATCH_RECORDS
            || self
                .pending
                .iter()
                .map(|v| {
                    v.item
                        .fields
                        .iter()
                        .map(|(k, s)| k.len() + s.len())
                        .sum::<usize>()
                        + v.item.page_url.len()
                        + v.item.url.len()
                        + v.dest_dir.len()
                        + v.filename.len()
                        + 256
                })
                .sum::<usize>()
                >= BATCH_BYTES
    }
}
fn sink_error(code: &'static str) -> ApiError {
    ApiError::new(
        code,
        "workflow item export cannot continue; existing files were preserved",
    )
}
fn io_error() -> ApiError {
    sink_error("workflow.items_unavailable")
}
fn changed() -> ApiError {
    sink_error("workflow.items_changed")
}

pub(super) fn runtime_error(error: ApiError) -> RuntimeError {
    RuntimeError::Port(match error.envelope().code.as_str() {
        "workflow.items_authority" => "workflow.items_authority",
        "workflow.items_changed" => "workflow.items_changed",
        "workflow.items_limit" => "workflow.items_limit",
        "workflow.items_unavailable" => "workflow.items_unavailable",
        "workflow.items_verify_timeout" => "workflow.items_verify_timeout",
        _ => "workflow.items_pending",
    })
}
impl WorkflowService {
    pub(super) fn hold_item_export(&self, run: RunId, code: &'static str) -> ApiResult<bool> {
        self.store.mutate_run(run, |r| {
            // A concurrent human pause/cancel remains authoritative. Do not revive a canceled run.
            if r.snapshot.report.status != RunStatus::Running {
                return Ok(false);
            }
            r.snapshot.report.status = RunStatus::Paused;
            r.snapshot.report.pause_reason = Some("item_export".into());
            r.snapshot.last_error = Some(code.into());
            r.snapshot.report.last_error = Some(zeus_types::ErrorEnvelope::from_code(
                code,
                "Item export needs attention; pending records and existing files were preserved",
            ));
            r.snapshot.checkpoint_seq = r.snapshot.checkpoint_seq.saturating_add(1);
            Ok(true)
        })
    }
    fn item_authority(&self, run: RunId, hash: &str) -> ApiResult<()> {
        let denied = || sink_error("workflow.items_authority");
        if self.stopping.load(Ordering::SeqCst)
            || self
                .journal
                .meta()
                .map_err(|_| denied())?
                .kill_switch_engaged()
        {
            return Err(denied());
        }
        let row = self.store.run(run)?.ok_or_else(denied)?;
        if row.snapshot.report.status != RunStatus::Running
            || row.snapshot.report.plan_hash != hash
            || !self.consent.requester_active(&row.owner)
            || (row.plan.session.is_some()
                && !crate::workflow_network::workflow_session_live(&self.journal, &row.plan, run))
        {
            return Err(denied());
        }
        let approval = row
            .approval_id
            .and_then(|id| self.journal.read(|tx| tx.approval(id)).ok().flatten())
            .ok_or_else(denied)?;
        if approval.state != ApprovalState::Approved
            || approval.run_id != Some(run)
            || approval
                .facts
                .as_ref()
                .and_then(|f| f.get("plan_blake3"))
                .and_then(|v| v.as_str())
                != Some(hash)
        {
            return Err(denied());
        }
        Ok(())
    }
    pub(super) fn flush_items(&self, run: RunId) -> ApiResult<()> {
        let _gate = self.export_gate.lock().map_err(|_| io_error())?;
        let row = self.store.run(run)?.ok_or_else(missing)?;
        if row.plan.sinks.items.is_none() || row.items.pending.is_empty() {
            return Ok(());
        }
        let authority = || self.item_authority(run, &row.snapshot.report.plan_hash);
        authority()?;
        let roots = zeus_fs::WriteRoots::open(&self.roots).map_err(|_| io_error())?;
        let destination = roots
            .resolve_dest_dir(Path::new(&row.plan.sinks.dest_dir))
            .map_err(|_| io_error())?;
        let root = roots.root_for(&destination).map_err(|_| io_error())?;
        let root_cap = DirectoryCap::open_root(root.path()).map_err(|_| io_error())?;
        let root_id = root_cap.file_id().map_err(|_| io_error())?;
        if row
            .items
            .receipt
            .as_ref()
            .is_some_and(|r| r.root_id != root_id)
        {
            return Err(changed());
        }
        let relative = destination
            .strip_prefix(root.path())
            .map_err(|_| io_error())?;
        authority()?;
        let directory = if row.items.receipt.is_some() {
            root_cap.open_dir(relative)
        } else {
            root_cap.create_dir_all(relative)
        }
        .map_err(|_| io_error())?;
        let directory_id = directory.file_id().map_err(|_| io_error())?;
        let name = format!("zeus-run-{run}.items.jsonl");
        let receipt = match row.items.receipt.clone() {
            Some(receipt) => receipt,
            None => {
                // A pre-existing name has no receipt and is never adopted, even when empty.
                // A crash between exclusive creation and the identity receipt fails closed.
                authority()?;
                let file = directory
                    .create_new(OsStr::new(&name))
                    .map_err(|_| changed())?;
                file.sync_all().map_err(|_| io_error())?;
                directory.sync().map_err(|_| io_error())?;
                let receipt = ItemReceipt {
                    root_id: root_id.clone(),
                    directory_id: directory_id.clone(),
                    file_id: opened_file_metadata(&file)
                        .map_err(|_| io_error())?
                        .file_id
                        .ok_or_else(io_error)?,
                    bytes: 0,
                    records: 0,
                    blake3: blake3::hash(&[]).to_hex().to_string(),
                };
                self.store.mutate_run(run, |r| {
                    if r.items.receipt.is_some() {
                        return Err(changed());
                    }
                    r.items.receipt = Some(receipt.clone());
                    Ok(())
                })?;
                receipt
            }
        };
        if receipt.root_id != root_id || receipt.directory_id != directory_id {
            return Err(changed());
        }
        let mut bytes = Vec::new();
        for outcome in &row.items.pending {
            let path = Path::new(&outcome.dest_dir).join(&outcome.filename);
            root.check_existing(&path).map_err(|_| changed())?;
            let path = path
                .strip_prefix(root.path())
                .map_err(|_| changed())?
                .to_str()
                .ok_or_else(changed)?
                .replace('\\', "/");
            if path.is_empty() || path.len() > 4096 {
                return Err(changed());
            }
            let record = ItemRecord {
                record_type: "item",
                run_id: run,
                index: outcome.index,
                page_url: &outcome.item.page_url,
                url: &outcome.item.url,
                url_redacted: outcome.item.url_redacted,
                fields: &outcome.item.fields,
                action: if outcome.job_id.is_some() {
                    "enqueued"
                } else {
                    "skipped_existing"
                },
                job_id: outcome.job_id,
                path,
                recorded_at: outcome.recorded_at,
            };
            let line = serde_json::to_vec(&record).map_err(|_| io_error())?;
            if line.len() > MAX_RECORD_BYTES {
                return Err(sink_error("workflow.items_limit"));
            }
            bytes.extend_from_slice(&line);
            bytes.push(b'\n');
            if bytes.len() as u64 > MAX_SINK_BYTES.saturating_sub(receipt.bytes) {
                return Err(sink_error("workflow.items_limit"));
            }
        }
        let mut file = directory
            .open_write_existing(OsStr::new(&name))
            .map_err(|_| changed())?;
        let next = append_exact(
            &mut file,
            &receipt,
            &bytes,
            row.items.pending.len() as u64,
            &authority,
        )?;
        authority()?;
        // Validate the still-bound name immediately before committing the durable cursor.
        if directory
            .entry_metadata(OsStr::new(&name))
            .map_err(|_| changed())?
            .file_id
            .as_ref()
            != Some(&receipt.file_id)
        {
            return Err(changed());
        }
        self.store.mutate_run(run, |r| {
            let current = r.items.receipt.as_ref().ok_or_else(changed)?;
            if current.bytes != receipt.bytes
                || current.blake3 != receipt.blake3
                || r.items.pending.len() != row.items.pending.len()
                || r.items.pending.iter().map(|v| v.index).ne(row
                    .items
                    .pending
                    .iter()
                    .map(|v| v.index))
            {
                return Err(changed());
            }
            r.items.receipt = Some(next);
            r.items.pending.clear();
            Ok(())
        })?;
        Ok(())
    }
}
fn append_exact(
    file: &mut File,
    receipt: &ItemReceipt,
    pending: &[u8],
    records: u64,
    authority: &dyn Fn() -> ApiResult<()>,
) -> ApiResult<ItemReceipt> {
    let before = file.metadata().map_err(|_| io_error())?;
    let identity = opened_file_metadata(file).map_err(|_| changed())?.file_id;
    let final_len = receipt
        .bytes
        .checked_add(pending.len() as u64)
        .ok_or_else(changed)?;
    if identity.as_ref() != Some(&receipt.file_id)
        || final_len > MAX_SINK_BYTES
        || before.len() < receipt.bytes
        || before.len() > final_len
    {
        return Err(changed());
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
    let started = Instant::now();
    let mut remaining = receipt.bytes;
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        authority()?;
        if started.elapsed().as_secs() >= 5 {
            return Err(sink_error("workflow.items_verify_timeout"));
        }
        let take = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..take])
            .map_err(|_| changed())?;
        hash.update(&buffer[..take]);
        remaining -= take as u64;
    }
    if hash.finalize().to_hex().as_str() != receipt.blake3 {
        return Err(changed());
    }
    let existing = (before.len() - receipt.bytes) as usize;
    let mut tail = vec![0; existing];
    file.read_exact(&mut tail).map_err(|_| changed())?;
    if tail != pending[..existing] {
        return Err(changed());
    }
    let checked = file.metadata().map_err(|_| io_error())?;
    if checked.len() != before.len() || checked.modified().ok() != before.modified().ok() {
        return Err(changed());
    }
    authority()?;
    // Append only missing bytes. Never truncate, replace or rewrite an existing prefix/tail.
    file.write_all(&pending[existing..])
        .map_err(|_| io_error())?;
    file.sync_all().map_err(|_| io_error())?;
    if file.metadata().map_err(|_| io_error())?.len() != final_len {
        return Err(changed());
    }
    hash.update(pending);
    Ok(ItemReceipt {
        bytes: final_len,
        records: receipt.records.saturating_add(records),
        blake3: hash.finalize().to_hex().to_string(),
        ..receipt.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn empty_receipt(file: &File) -> ItemReceipt {
        let id = opened_file_metadata(file).unwrap().file_id.unwrap();
        ItemReceipt {
            root_id: id.clone(),
            directory_id: id.clone(),
            file_id: id,
            bytes: 0,
            records: 0,
            blake3: blake3::hash(&[]).to_hex().to_string(),
        }
    }
    #[test]
    fn item_append_recovers_every_partial_tail_without_duplicate_or_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let cap = DirectoryCap::open_root(dir.path()).unwrap();
        let first = b"first-record\n";
        let second = b"second-record\nthird-record\n";
        for cut in 0..=second.len() {
            let name = format!("cut-{cut}");
            let mut file = cap.create_new(OsStr::new(&name)).unwrap();
            let receipt = empty_receipt(&file);
            let first_receipt = append_exact(&mut file, &receipt, first, 1, &|| Ok(())).unwrap();
            file.write_all(&second[..cut]).unwrap();
            file.sync_all().unwrap();
            drop(file);
            let mut recovered = cap.open_write_existing(OsStr::new(&name)).unwrap();
            let next = append_exact(&mut recovered, &first_receipt, second, 2, &|| Ok(())).unwrap();
            assert_eq!(next.records, 3);
            assert_eq!(next.bytes, (first.len() + second.len()) as u64);
            assert_eq!(
                next.blake3,
                blake3::hash(&[first.as_slice(), second.as_slice()].concat())
                    .to_hex()
                    .to_string()
            );
            // Crash after the bytes reached disk, before the receipt commit.
            let replay =
                append_exact(&mut recovered, &first_receipt, second, 2, &|| Ok(())).unwrap();
            assert_eq!(replay.bytes, next.bytes);
            drop(recovered);
            assert_eq!(
                std::fs::read(dir.path().join(name)).unwrap(),
                [first.as_slice(), second.as_slice()].concat()
            );
        }
    }
    #[test]
    fn item_append_refuses_changed_prefix_tail_and_replaced_identity_without_writes() {
        for kind in ["prefix", "tail", "replacement", "truncated"] {
            let dir = tempfile::tempdir().unwrap();
            let cap = DirectoryCap::open_root(dir.path()).unwrap();
            let mut file = cap.create_new(OsStr::new("sink")).unwrap();
            let initial = empty_receipt(&file);
            let receipt = append_exact(&mut file, &initial, b"first\n", 1, &|| Ok(())).unwrap();
            let modified = file.metadata().unwrap().modified().unwrap();
            drop(file);
            match kind {
                "prefix" => {
                    std::fs::write(dir.path().join("sink"), b"other\n").unwrap();
                    let f = std::fs::OpenOptions::new()
                        .write(true)
                        .open(dir.path().join("sink"))
                        .unwrap();
                    f.set_times(std::fs::FileTimes::new().set_modified(modified))
                        .unwrap();
                }
                "tail" => {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(dir.path().join("sink"))
                        .unwrap()
                        .write_all(b"bad")
                        .unwrap();
                }
                "replacement" => {
                    std::fs::rename(dir.path().join("sink"), dir.path().join("old")).unwrap();
                    std::fs::write(dir.path().join("sink"), b"first\n").unwrap();
                }
                _ => std::fs::write(dir.path().join("sink"), b"fir").unwrap(),
            }
            let before = std::fs::read(dir.path().join("sink")).unwrap();
            let mut file = cap.open_write_existing(OsStr::new("sink")).unwrap();
            assert!(
                append_exact(&mut file, &receipt, b"second\n", 1, &|| Ok(())).is_err(),
                "{kind}"
            );
            drop(file);
            assert_eq!(std::fs::read(dir.path().join("sink")).unwrap(), before);
        }
    }
    #[test]
    fn item_append_stops_when_authority_is_lost_after_prefix_verification() {
        let dir = tempfile::tempdir().unwrap();
        let cap = DirectoryCap::open_root(dir.path()).unwrap();
        let mut file = cap.create_new(OsStr::new("sink")).unwrap();
        let empty = empty_receipt(&file);
        let receipt = append_exact(&mut file, &empty, b"first\n", 1, &|| Ok(())).unwrap();
        let checks = std::cell::Cell::new(0);
        let authority = || {
            checks.set(checks.get() + 1);
            if checks.get() >= 2 {
                Err(sink_error("workflow.items_authority"))
            } else {
                Ok(())
            }
        };
        assert!(append_exact(&mut file, &receipt, b"second\n", 1, &authority).is_err());
        drop(file);
        assert_eq!(std::fs::read(dir.path().join("sink")).unwrap(), b"first\n");
    }
    #[test]
    fn item_late_admission_after_cancel_preserves_its_metadata_once() {
        let dir = tempfile::tempdir().unwrap();
        let (service, mut row, intent) = fixture(dir.path());
        let run = row.snapshot.report.run_id;
        row.pending.push(intent);
        row.retire_pending();
        row.snapshot.report.status = RunStatus::Canceled;
        service.store.save_run(&row).unwrap();
        let job = zeus_types::IdGenerator::new().job_id();
        service
            .store
            .mutate_run(run, |r| {
                r.record_admission("wf:item:1", job);
                r.record_admission("wf:item:1", job);
                Ok(())
            })
            .unwrap();
        let row = service.store.run(run).unwrap().unwrap();
        assert_eq!(row.items.pending.len(), 1);
        assert_eq!(row.items.pending[0].job_id, Some(job));
        assert!(row.items.retired.is_empty());
        assert!(service.flush_items(run).is_err());
        assert!(!dir.path().join("exports").exists());
    }
    #[test]
    fn item_export_failure_does_not_override_concurrent_human_control() {
        for status in [RunStatus::Canceled, RunStatus::Paused, RunStatus::Running] {
            let dir = tempfile::tempdir().unwrap();
            let (service, mut row, intent) = fixture(dir.path());
            let run = row.snapshot.report.run_id;
            row.snapshot.report.status = status;
            row.items.record(&intent, None);
            service.store.save_run(&row).unwrap();
            assert_eq!(
                service
                    .hold_item_export(run, "workflow.items_changed")
                    .unwrap(),
                status == RunStatus::Running
            );
            let after = service.store.run(run).unwrap().unwrap();
            assert_eq!(
                after.snapshot.report.status,
                if status == RunStatus::Running {
                    RunStatus::Paused
                } else {
                    status
                }
            );
            assert_eq!(after.items.pending.len(), 1);
        }
    }
    #[test]
    fn item_sink_pause_cancel_and_changed_owner_preserve_pending_without_creating_output() {
        for reason in ["paused", "canceled", "owner", "plan", "kill"] {
            let dir = tempfile::tempdir().unwrap();
            let (service, mut row, intent) = fixture(dir.path());
            let run = row.snapshot.report.run_id;
            row.items.record(&intent, None);
            match reason {
                "paused" => row.snapshot.report.status = RunStatus::Paused,
                "canceled" => row.snapshot.report.status = RunStatus::Canceled,
                "owner" => row.owner = "revoked-or-missing".into(),
                "kill" => service
                    .journal
                    .transact(|tx| tx.set_kill_switch(true))
                    .unwrap(),
                _ => row.snapshot.report.plan_hash = "0".repeat(64),
            }
            service.store.save_run(&row).unwrap();
            assert!(service.flush_items(run).is_err(), "{reason}");
            assert!(!dir.path().join("exports").exists());
            assert_eq!(
                service.store.run(run).unwrap().unwrap().items.pending.len(),
                1
            );
        }
    }
    fn fixture(path: &Path) -> (Arc<WorkflowService>, StoredRun, JobIntent) {
        use zeus_api::auth::{Credential, CredentialId, MemoryCredentialStore, Token};
        let journal = zeus_journal::Journal::open(
            &path.join("journal.redb"),
            &Default::default(),
            Arc::new(zeus_control::SystemClock::new()),
        )
        .unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let credential_id = CredentialId::generate();
        credentials.insert(
            Credential::new(
                credential_id.clone(),
                ClientKind::Agent,
                zeus_api::ClientRole::Mcp,
                Token::generate().auth_key(),
                Timestamp::now(),
            )
            .unwrap(),
        );
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
            credentials,
        ));
        let service = WorkflowService::open(
            &path.join("workflow.redb"),
            journal,
            consent,
            audit,
            vec![path.to_owned()],
        )
        .unwrap();
        let mut row = super::super::tests::row();
        row.owner = credential_id.to_string();
        row.plan.sinks.dest_dir = path.join("exports").to_string_lossy().into_owned();
        row.plan.sinks.items = Some(ItemsSink {
            format: ItemsFormat::Jsonl,
        });
        let id = row.snapshot.report.run_id;
        row.snapshot = WorkflowRuntime::new(row.plan.clone(), id)
            .unwrap()
            .snapshot()
            .clone();
        row.snapshot.report.status = RunStatus::Running;
        let approval_id = zeus_types::IdGenerator::new().approval_id();
        row.approval_id = Some(approval_id);
        let now = Timestamp::now();
        let approval:zeus_types::Approval=serde_json::from_value(serde_json::json!({"id":approval_id,"kind":"workflow_plan","state":"approved","run_id":id,"allowed_scopes":[],"reason_codes":[],"summary":{"title":"Fixture"},"created_at":now,"expires_at":now.saturating_add_ms(86400000),"facts":{"plan_blake3":row.snapshot.report.plan_hash}})).unwrap();
        service
            .journal
            .transact(|tx| tx.put_approval(&approval))
            .unwrap();
        let mut intent = super::super::tests::report_intent(id, "wf:item:1", 1);
        let roots = zeus_fs::WriteRoots::open(&[path.to_path_buf()]).unwrap();
        intent.spec.dest_dir = Some(
            roots
                .resolve_dest_dir(Path::new(&row.plan.sinks.dest_dir))
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        intent.spec.filename = Some("one.pdf".into());
        intent.item = Some(ItemDraft {
            page_url: "https://example.org/list".into(),
            url: "https://example.org/file?token=REDACTED".into(),
            url_redacted: true,
            fields: BTreeMap::from([("title".into(), "Untrusted site title".into())]),
        });
        (service, row, intent)
    }
    #[test]
    fn item_outcome_and_admission_commit_once_and_sink_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (service, mut row, intent) = fixture(dir.path());
        let run = row.snapshot.report.run_id;
        row.pending.push(intent.clone());
        service.store.save_run(&row).unwrap();
        let job = zeus_types::IdGenerator::new().job_id();
        service
            .store
            .mutate_run(run, |r| {
                r.record_admission("wf:item:1", job);
                r.record_admission("wf:item:1", job);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            service.store.run(run).unwrap().unwrap().items.pending.len(),
            1
        );
        let journal = service.journal.clone();
        let consent = service.consent.clone();
        let audit = service.audit.clone();
        drop(service);
        let service = WorkflowService::open(
            &dir.path().join("workflow.redb"),
            journal,
            consent,
            audit,
            vec![dir.path().to_owned()],
        )
        .unwrap();
        service.flush_items(run).unwrap();
        service.flush_items(run).unwrap();
        let path = dir
            .path()
            .join("exports")
            .join(format!("zeus-run-{run}.items.jsonl"));
        let line = std::fs::read_to_string(&path).unwrap();
        assert_eq!(line.lines().count(), 1);
        let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["action"], "enqueued");
        assert_eq!(record["path"], "exports/one.pdf");
        assert_eq!(record["job_id"], job.to_string());
        let mut second = intent;
        second.index = 2;
        second.spec.idempotency_key = Some("wf:item:2".into());
        service
            .store
            .mutate_run(run, |r| {
                r.pending.push(second.clone());
                assert!(r.skip_existing_intent(&second));
                assert!(!r.skip_existing_intent(&second));
                Ok(())
            })
            .unwrap();
        service.flush_items(run).unwrap();
        let lines = std::fs::read_to_string(path).unwrap();
        assert_eq!(lines.lines().count(), 2);
        let last: serde_json::Value = serde_json::from_str(lines.lines().last().unwrap()).unwrap();
        assert_eq!(last["index"], 2);
        assert_eq!(last["action"], "skipped_existing");
        assert!(last["job_id"].is_null());
        let row = service.store.run(run).unwrap().unwrap();
        assert_eq!(row.items.receipt.unwrap().records, 2);
        assert!(row.items.pending.is_empty());
    }
    #[tokio::test]
    async fn item_drain_recovers_admission_before_existing_skip_and_flushes_both_outcomes() {
        for already_admitted in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (service, mut row, mut intent) = fixture(dir.path());
            let run = row.snapshot.report.run_id;
            let destination = dir.path().join("exports");
            std::fs::create_dir(&destination).unwrap();
            std::fs::write(destination.join("one.pdf"), b"original").unwrap();
            intent.spec.source.client_id = Some(row.owner.clone());
            if already_admitted {
                let mut job = super::super::tests::report_job(run, JobState::Completed, 8);
                job.job.spec = intent.spec.clone();
                let now = Timestamp::now();
                service
                    .journal
                    .transact(|tx| {
                        tx.put_job(&job)?;
                        tx.put_idempotency(&zeus_journal::IdempotencyRow {
                            client_id: row.owner.clone(),
                            key: "wf:item:1".into(),
                            request_hash: zeus_types::Hex256::from_bytes([0; 32]),
                            job_id: job.id(),
                            method: Some("jobs.enqueue".into()),
                            result: None,
                            created_at: now,
                            expires_at: now.saturating_add_ms(86400000),
                        })
                    })
                    .unwrap();
            }
            row.pending.push(intent);
            row.snapshot.report.files_enqueued = 1;
            service.store.save_run(&row).unwrap();
            service.drain(run).await.unwrap();
            service.drain(run).await.unwrap();
            let row = service.store.run(run).unwrap().unwrap();
            assert!(row.pending.is_empty());
            assert!(row.items.pending.is_empty());
            assert_eq!(row.items.receipt.unwrap().records, 1);
            let line =
                std::fs::read_to_string(destination.join(format!("zeus-run-{run}.items.jsonl")))
                    .unwrap();
            let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(
                record["action"],
                if already_admitted {
                    "enqueued"
                } else {
                    "skipped_existing"
                }
            );
            assert_eq!(
                std::fs::read(destination.join("one.pdf")).unwrap(),
                b"original"
            );
        }
    }
    #[test]
    fn item_sink_never_adopts_existing_file_or_replaced_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (service, mut row, intent) = fixture(dir.path());
        let run = row.snapshot.report.run_id;
        row.items.record(&intent, None);
        service.store.save_run(&row).unwrap();
        let destination = dir.path().join("exports");
        std::fs::create_dir(&destination).unwrap();
        let name = format!("zeus-run-{run}.items.jsonl");
        let path = destination.join(&name);
        std::fs::write(&path, b"unrelated").unwrap();
        assert!(service.flush_items(run).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unrelated");
        std::fs::rename(&path, destination.join("unrelated-preserved")).unwrap();
        service.flush_items(run).unwrap();
        let original = std::fs::read(&path).unwrap();
        service
            .store
            .mutate_run(run, |r| {
                r.items.record(&intent, None);
                Ok(())
            })
            .unwrap();
        std::fs::rename(&destination, dir.path().join("moved")).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(&path, &original).unwrap();
        assert!(service.flush_items(run).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}
