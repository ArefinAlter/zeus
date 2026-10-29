//! Bounded workflow execution with explicit authority, networking and atomic-storage ports.
//!
//! The embedding owns connected-peer checks, robots rules, session grants, trusted approvals and
//! the job/checkpoint transaction. This module never turns page text into instructions or authority.
use crate::{
    Diagnostic, Filter, Plan, RunReport, RunStatus, Template,
    extract::{self, Fields},
    has_errors,
    pagination::{self, Chain},
    plan::{OnDrift, OnExisting, Selector, UnknownMime},
    validate_plan,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
use url::Url;
use zeus_types::{
    HttpUrl, JobSpec, Overwrite, RedactMode, RunId, Source as JobSource, SourceKind, SourceVia,
    Timestamp,
};

/// Failures contain codes or trusted diagnostics, never document content.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// Candidate is invalid or requires an unavailable capability.
    #[error("workflow plan is invalid")]
    Invalid(Vec<Diagnostic>),
    /// Trusted policy gate refused the operation.
    #[error("workflow policy refused: {0}")]
    Policy(&'static str),
    /// Port error; implementations must return a stable code, never response content or a URL.
    #[error("workflow port failed: {0}")]
    Port(&'static str),
    /// Another writer advanced the run, or persisted state was tampered with.
    #[error("workflow checkpoint does not match the plan or expected revision")]
    CheckpointConflict,
}
/// Result of a runtime operation.
pub type RuntimeResult<T> = Result<T, RuntimeError>;
/// Policy context for a document fetch, including every redirect hop.
#[derive(Clone)]
pub struct FetchRequest {
    /// Bound run.
    pub run_id: RunId,
    /// Full URL; never log it.
    pub url: String,
    /// Approved host boundary.
    pub allowed_domains: Vec<String>,
    /// Minimum host pacing.
    pub min_interval_ms: u64,
    /// Maximum host concurrency, shared with file jobs.
    pub max_concurrency_per_host: u8,
    /// Session origin, only when an effective workflow grant exists.
    pub session_origin: Option<zeus_types::Origin>,
    /// Bounded decoded body bytes.
    pub max_body_bytes: usize,
    /// Expected Accept header.
    pub accept: String,
}
/// Decoded, bounded document. No Debug implementation exposes body or full URLs.
#[derive(Clone)]
pub struct Document {
    /// Actual final URL after policy-checked redirects.
    pub final_url: String,
    /// HTTP status; synthetic 404/410 end numeric chains.
    pub status: u16,
    /// Declared media type.
    pub content_type: String,
    /// Decoded text.
    pub body: String,
    /// Headers needed by pagination, normally only Link.
    pub headers: BTreeMap<String, String>,
}
/// Counters read from engine jobs, not estimated from discovery.
#[derive(Debug, Clone, Copy, Default)]
pub struct JobTotals {
    /// Nonterminal jobs including paused ones.
    pub active: u32,
    /// Verified completions.
    pub completed: u32,
    /// Failed jobs.
    pub failed: u32,
    /// Committed bytes, including completed files.
    pub bytes_done: u64,
    /// Uncommitted declared-size reservations of existing jobs.
    pub bytes_reserved: u64,
}
/// One job to create in the same commit as its URL and dedupe keys.
#[derive(Clone, Serialize, Deserialize)]
pub struct JobIntent {
    /// Fully derived engine job spec.
    pub spec: JobSpec,
    /// Position within this run, starting at one.
    pub index: u32,
    /// Original-file handling; replace only stages a human-reviewed operation.
    pub on_existing: OnExisting,
    /// Shared remaining byte budget for the batch and outstanding jobs.
    pub byte_budget_remaining: u64,
    /// Listing declaration is a reservation estimate, never an engine integrity assertion.
    #[serde(default)]
    pub declared_size: Option<u64>,
}
/// Run control is forwarded to jobs in the same ownership scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAction {
    /// Pause discovery and interruptible jobs.
    Pause,
    /// Resume after exact approval and grant checks.
    Resume,
    /// Cancel partial jobs; preserve completed files.
    Cancel,
}
/// Durable private state. Full URLs and keys are never returned by report().
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSnapshot {
    /// Optimistic transaction revision.
    pub checkpoint_seq: u64,
    /// Public, redacted counters.
    pub report: RunReport,
    /// Full frontier, in discovery order.
    pub chains: Vec<Chain>,
    /// BLAKE3 URL/dedupe keys, at most two million.
    pub keys: BTreeSet<String>,
    /// Digest over the sorted key set, detecting incomplete restore.
    pub keys_digest: String,
    /// Time spent running, excluding pauses and approval waits.
    pub elapsed_ms: u64,
    /// Filenames assigned to earlier items of this run.
    pub assigned_paths: BTreeSet<String>,
    /// Durable active document errors; no body or URL values.
    pub last_error: Option<String>,
}
/// Trusted embedding adapter. Every method is mandatory so an incomplete policy implementation
/// cannot silently authorize work. `commit` MUST atomically persist job reservations, keys, counters and frontier.
/// Reservations may be durable outbox intents; enqueue must use deterministic idempotency keys.
#[async_trait]
pub trait RuntimePort: Send + Sync {
    /// Check durable human approval for this exact hash, effective workflow grant and kill switch.
    async fn authorize(&self, run: RunId, plan_hash: &str) -> RuntimeResult<()>;
    /// Enforce robots.txt, peer-address policy, cancellable host pacing, redirects, grant scope and body limits.
    async fn fetch(&self, request: FetchRequest) -> RuntimeResult<Document>;
    /// Current durable job counters and byte reservations.
    async fn job_totals(&self, run: RunId) -> RuntimeResult<JobTotals>;
    /// Apply job reservations and checkpoint as one durable transaction. A stale revision changes nothing.
    async fn commit(
        &self,
        expected_seq: u64,
        next: &RunSnapshot,
        jobs: &[JobIntent],
    ) -> RuntimeResult<()>;
    /// Propagate run control to jobs. Verifying/finalizing jobs follow engine interruptibility rules.
    async fn control_jobs(&self, run: RunId, action: RunAction) -> RuntimeResult<()>;
    /// Write requested exports and post-processing results idempotently for this run.
    async fn finish(&self, plan: &Plan, snapshot: &RunSnapshot) -> RuntimeResult<()>;
    /// Verify destinations, requested conversion capabilities and output support before any fetch.
    async fn validate_environment(&self, plan: &Plan) -> RuntimeResult<()>;
}
fn digest_keys(keys: &BTreeSet<String>) -> String {
    let mut hash = blake3::Hasher::new();
    for k in keys {
        hash.update(k.as_bytes());
        hash.update(b"\n");
    }
    hash.finalize().to_hex().to_string()
}
fn key(prefix: &str, text: &str) -> String {
    blake3::hash(format!("{prefix}:{text}").as_bytes())
        .to_hex()
        .to_string()
}
fn allowed(plan: &Plan, url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.username().is_empty()
            && u.password().is_none()
            && u.host_str()
                .is_some_and(|h| crate::validate::host_allowed(h, &plan.allowed_domains))
    })
}
fn invalid(code: &str, message: &str) -> RuntimeError {
    RuntimeError::Invalid(vec![Diagnostic::error(code, "/", message)])
}
/// Stateful runner. Call step repeatedly; each successful step durably advances one document or status.
pub struct WorkflowRuntime {
    plan: Plan,
    snapshot: RunSnapshot,
}
impl WorkflowRuntime {
    /// Build an unapproved run. No I/O occurs and no job can start before the authorization port agrees.
    pub fn new(plan: Plan, run_id: RunId) -> RuntimeResult<Self> {
        let diagnostics = validate_plan(&plan);
        if has_errors(&diagnostics) {
            return Err(RuntimeError::Invalid(diagnostics));
        }
        let chains = pagination::frontier(&plan).map_err(|d| RuntimeError::Invalid(vec![d]))?;
        let hash = plan
            .plan_hash()
            .map_err(|_| invalid("workflow.plan_invalid", "plan cannot be canonicalized"))?;
        let keys = BTreeSet::new();
        let snapshot = RunSnapshot {
            checkpoint_seq: 0,
            report: RunReport {
                report_version: 1,
                plan_id: None,
                approval_id: None,
                grant_id: None,
                browser_document_mode: plan.session.as_ref().and_then(|s| s.document_mode),
                created_at: Some(Timestamp::now()),
                updated_at: Some(Timestamp::now()),
                elapsed_ms: 0,
                counts: Some(Default::default()),
                pause_reason: None,
                possibly_incomplete: false,
                last_error: None,
                run_id,
                plan_hash: hash,
                status: RunStatus::AwaitingApproval,
                pages_fetched: 0,
                files_enqueued: 0,
                files_completed: 0,
                files_failed: 0,
                bytes_done: 0,
                started_at: None,
                finished_at: None,
                stop_reason: None,
                drift: vec![],
            },
            chains,
            keys_digest: digest_keys(&keys),
            keys,
            elapsed_ms: 0,
            assigned_paths: BTreeSet::new(),
            last_error: None,
        };
        Ok(Self { plan, snapshot })
    }
    /// Restore an exact plan and complete key set; never reinterpret a checkpoint under a changed plan.
    pub fn restore(plan: Plan, snapshot: RunSnapshot) -> RuntimeResult<Self> {
        let validated = Self::new(plan.clone(), snapshot.report.run_id)?;
        if validated.snapshot.report.plan_hash != snapshot.report.plan_hash
            || snapshot.report.report_version > 1
            || (snapshot.report.report_version == 1 && snapshot.report.counts.is_none())
            || digest_keys(&snapshot.keys) != snapshot.keys_digest
            || snapshot.keys.len() > 2_000_000
            || snapshot.chains.len() > 100_000
            || snapshot.chains.iter().any(|chain| {
                chain.source_index >= plan.sources.len()
                    || chain.sitemap_depth.is_some_and(|depth| {
                        depth > plan.budgets.max_depth
                            || chain.pagination.is_some()
                            || !matches!(
                                plan.sources[chain.source_index],
                                crate::Source::Sitemap { .. }
                            )
                    })
            })
        {
            return Err(RuntimeError::CheckpointConflict);
        }
        Ok(Self { plan, snapshot })
    }
    /// Public counters; contains no full URL or extracted field values.
    pub fn report(&self) -> &RunReport {
        &self.snapshot.report
    }
    /// Private checkpoint for persistence only.
    pub fn snapshot(&self) -> &RunSnapshot {
        &self.snapshot
    }
    async fn save(
        &mut self,
        port: &dyn RuntimePort,
        mut next: RunSnapshot,
        jobs: &[JobIntent],
    ) -> RuntimeResult<()> {
        let pause_jobs = next.report.status == RunStatus::Paused
            && self.snapshot.report.status != RunStatus::Paused;
        next.checkpoint_seq = self
            .snapshot
            .checkpoint_seq
            .checked_add(1)
            .ok_or(RuntimeError::CheckpointConflict)?;
        next.keys_digest = digest_keys(&next.keys);
        next.report.updated_at = Some(Timestamp::now());
        next.report.elapsed_ms = next.elapsed_ms;
        if next.report.status == RunStatus::Running && next.report.report_version > 0 {
            next.report.started_at.get_or_insert_with(Timestamp::now);
        }
        next.report.possibly_incomplete = matches!(
            next.report.status,
            RunStatus::BudgetExhausted
                | RunStatus::Stopped
                | RunStatus::Failed
                | RunStatus::Canceled
        ) || next
            .report
            .stop_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("budget_") || reason == "drift");
        if let Some(counts) = &mut next.report.counts {
            counts.pages_fetched = next.report.pages_fetched;
            counts.files_enqueued = next.report.files_enqueued;
            counts.files_completed = next.report.files_completed;
            counts.files_failed = next.report.files_failed;
            counts.bytes_done = next.report.bytes_done;
        }
        port.commit(self.snapshot.checkpoint_seq, &next, jobs)
            .await?;
        self.snapshot = next;
        // Publish the durable restriction before waiting for workers to stop. A stale
        // concurrent discovery commit cannot restore authority after this checkpoint.
        if pause_jobs {
            port.control_jobs(self.snapshot.report.run_id, RunAction::Pause)
                .await?;
        }
        Ok(())
    }
    /// Apply explicit control; resume rechecks the exact approval and grant instead of extending budgets.
    pub async fn control(
        &mut self,
        port: &dyn RuntimePort,
        action: RunAction,
    ) -> RuntimeResult<()> {
        let mut next = self.snapshot.clone();
        match action {
            RunAction::Resume => {
                if next.report.status != RunStatus::Paused {
                    return Err(RuntimeError::Policy("workflow.invalid_transition"));
                }
                port.authorize(next.report.run_id, &next.report.plan_hash)
                    .await?;
                next.report.status = RunStatus::Running;
                next.last_error = None;
                next.report.last_error = None;
                next.report.pause_reason = None;
            }
            RunAction::Pause => {
                if !matches!(
                    next.report.status,
                    RunStatus::Running | RunStatus::AwaitingApproval
                ) {
                    return Err(RuntimeError::Policy("workflow.invalid_transition"));
                }
                next.report.status = RunStatus::Paused;
                next.report.pause_reason = Some("client".into());
            }
            RunAction::Cancel => {
                next.report.status = RunStatus::Canceled;
                next.report.stop_reason = Some("canceled".into());
                next.report.finished_at = Some(Timestamp::now());
            }
        }
        let run = next.report.run_id;
        self.save(port, next, &[]).await?;
        if action != RunAction::Pause {
            port.control_jobs(run, action).await?;
        }
        Ok(())
    }
    /// Execute at most one listing document and its bounded detail documents, then commit.
    pub async fn step(&mut self, port: &dyn RuntimePort) -> RuntimeResult<()> {
        if !matches!(
            self.snapshot.report.status,
            RunStatus::Running | RunStatus::AwaitingApproval
        ) {
            return Ok(());
        }
        port.authorize(self.snapshot.report.run_id, &self.snapshot.report.plan_hash)
            .await?;
        port.validate_environment(&self.plan).await?;
        let started = Instant::now();
        let mut next = self.snapshot.clone();
        next.report.status = RunStatus::Running;
        let totals = port.job_totals(next.report.run_id).await?;
        next.report.files_completed = totals.completed;
        next.report.files_failed = totals.failed;
        next.report.bytes_done = totals.bytes_done;
        if totals.bytes_done >= self.plan.budgets.max_bytes
            || next.elapsed_ms >= self.plan.budgets.max_duration_ms
        {
            next.report.status = RunStatus::BudgetExhausted;
            next.report.stop_reason = Some(
                if totals.bytes_done >= self.plan.budgets.max_bytes {
                    "budget_bytes"
                } else {
                    "budget_duration"
                }
                .into(),
            );
            next.report.finished_at = Some(Timestamp::now());
            port.control_jobs(next.report.run_id, RunAction::Cancel)
                .await?;
            port.finish(&self.plan, &next).await?;
            return self.save(port, next, &[]).await;
        }
        let Some(chain_index) = next.chains.iter().position(|c| c.next_url.is_some()) else {
            if totals.active == 0 {
                next.report.status = match next.report.stop_reason.as_deref() {
                    Some("budget_pages" | "budget_files" | "budget_bytes" | "budget_duration") => {
                        RunStatus::BudgetExhausted
                    }
                    Some("drift") => RunStatus::Stopped,
                    _ => RunStatus::Completed,
                };
                next.report.finished_at = Some(Timestamp::now());
                port.finish(&self.plan, &next).await?;
            }
            return self.save(port, next, &[]).await;
        };
        if next.report.pages_fetched >= self.plan.budgets.max_pages {
            end_discovery(&mut next, "budget_pages");
            return self.save(port, next, &[]).await;
        }
        let url = next.chains[chain_index]
            .next_url
            .clone()
            .ok_or(RuntimeError::CheckpointConflict)?;
        if !allowed(&self.plan, &url) {
            return Err(RuntimeError::Policy("workflow.domain_not_allowed"));
        }
        let document_key = key("p", &url);
        if next.chains[chain_index].visited.contains(&document_key) {
            next.chains[chain_index].next_url = None;
            next.report.stop_reason = Some("duplicate_page".into());
            return self.save(port, next, &[]).await;
        }
        let sitemap_depth = next.chains[chain_index].sitemap_depth;
        let document = match self.fetch_kind(port, &url, sitemap_depth.is_some()).await {
            Ok(d) => d,
            Err(error) => {
                next.report.status = RunStatus::Paused;
                next.last_error = Some("workflow.page_error".into());
                record_page_error(&mut next, &error);
                next.elapsed_ms = next
                    .elapsed_ms
                    .saturating_add(started.elapsed().as_millis() as u64);
                self.save(port, next, &[]).await?;
                return Err(error);
            }
        };
        if matches!(document.status, 404 | 410)
            && matches!(
                next.chains[chain_index].pagination,
                Some(crate::Pagination::PageParam { .. } | crate::Pagination::OffsetLimit { .. })
            )
        {
            next.chains[chain_index].next_url = None;
            next.report.pages_fetched += 1;
            return self.save(port, next, &[]).await;
        }
        if !(200..300).contains(&document.status) {
            next.report.status = RunStatus::Paused;
            next.last_error = Some("workflow.page_error".into());
            record_page_error(&mut next, &RuntimeError::Port("workflow.page_error"));
            return self.save(port, next, &[]).await;
        }
        if let Some(depth) = sitemap_depth {
            let discovery = self.sitemap_frontier(&next, chain_index, depth, &document);
            let chains = match discovery {
                Ok(chains) => chains,
                Err(error) => {
                    next.report.status = RunStatus::Paused;
                    next.last_error = Some("workflow.sitemap_error".into());
                    record_page_error(&mut next, &error);
                    self.save(port, next, &[]).await?;
                    return Err(error);
                }
            };
            next.report.pages_fetched += 1;
            next.chains[chain_index].pages += 1;
            next.chains[chain_index].visited.insert(document_key);
            next.chains[chain_index].next_url = None;
            next.chains.extend(chains);
            next.elapsed_ms = next
                .elapsed_ms
                .saturating_add(started.elapsed().as_millis() as u64);
            return self.save(port, next, &[]).await;
        }
        let extracted = match extract::extract(
            &document.body,
            &document.final_url,
            &self.plan.extract.items,
            &self.plan.extract.fields,
        ) {
            Ok(e) => e,
            Err(d) => {
                drift(&self.plan, &mut next, d);
                return self.save(port, next, &[]).await;
            }
        };
        next.report.pages_fetched += 1;
        next.chains[chain_index].pages += 1;
        next.chains[chain_index].visited.insert(document_key);
        next.chains[chain_index].items_seen = next.chains[chain_index]
            .items_seen
            .saturating_add(extracted.total as u64);
        let next_url = pagination::next(
            &next.chains[chain_index],
            &document.body,
            &extracted.base,
            &document.headers,
            extracted.total,
        );
        if (extracted.total == 0 && (next.chains[chain_index].pages == 1 || next_url.is_some()))
            || (extracted.total > 0 && extracted.invalid * 2 >= extracted.total)
        {
            drift(
                &self.plan,
                &mut next,
                Diagnostic::warning(
                    if extracted.total == 0 {
                        "plan.drift_no_items"
                    } else {
                        "plan.drift_field_missing"
                    },
                    "/extract",
                    "document no longer matches the approved extraction baseline",
                ),
            );
            if next.report.status != RunStatus::Running {
                next.chains[chain_index].visited.remove(&key("p", &url));
                next.chains[chain_index].pages = next.chains[chain_index].pages.saturating_sub(1);
                next.report.pages_fetched = next.report.pages_fetched.saturating_sub(1);
                return self.save(port, next, &[]).await;
            }
        }
        if extracted.total > 10_000 {
            record_drift(
                &mut next,
                Diagnostic::warning(
                    "plan.drift_too_many_items",
                    "/extract/items",
                    "document exceeds the 10000-item bound",
                ),
            );
        }
        if let Some(counts) = &mut next.report.counts {
            counts.items_seen = counts.items_seen.saturating_add(extracted.total as u64);
            counts.items_invalid = counts
                .items_invalid
                .saturating_add(extracted.invalid as u64);
        }
        if next.chains[chain_index].pages == 1
            && let Some(selector) = &self.plan.stop.count_hint
        {
            next.chains[chain_index].count_hint =
                extract::document_value(&document.body, selector, false).and_then(|s| {
                    let digits = s
                        .chars()
                        .filter(|c| !matches!(c, ',' | '.' | '\'' | '\u{00a0}' | '\u{202f}' | ' '))
                        .collect::<String>();
                    if (1..=15).contains(&digits.len())
                        && digits.bytes().all(|b| b.is_ascii_digit())
                    {
                        digits.parse().ok()
                    } else {
                        None
                    }
                });
        }
        let mut intents = Vec::new();
        let mut newly_seen = 0u32;
        let mut reserved = totals.bytes_reserved;
        let mut fingerprint = blake3::Hasher::new();
        let mut off_domain = 0usize;
        for mut fields in extracted.items {
            if self.plan.extract.follow.is_some() {
                match self
                    .add_detail(port, &mut next, &mut fields, &extracted.base)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        // Retry the same listing after resume. Partial item work never committed.
                        let mut failed = self.snapshot.clone();
                        failed.report.status = RunStatus::Paused;
                        failed.last_error = Some("workflow.page_error".into());
                        record_page_error(&mut failed, &error);
                        failed.elapsed_ms = failed
                            .elapsed_ms
                            .saturating_add(started.elapsed().as_millis() as u64);
                        self.save(port, failed, &[]).await?;
                        return Err(error);
                    }
                }
            }
            let Some(item_url) = fields.get("url").cloned() else {
                continue;
            };
            fingerprint.update(item_url.as_bytes());
            fingerprint.update(b"\n");
            if !allowed(&self.plan, &item_url) {
                off_domain += 1;
                if let Some(counts) = &mut next.report.counts {
                    counts.items_out_of_domain = counts.items_out_of_domain.saturating_add(1);
                }
                continue;
            }
            if !next.keys.insert(key("u", &item_url)) {
                if let Some(counts) = &mut next.report.counts {
                    counts.items_duplicate = counts.items_duplicate.saturating_add(1);
                }
                continue;
            }
            newly_seen += 1;
            if !self.filters(&mut next, &fields)? {
                // Dedupe filters account for themselves; all other filters increment ordinary rejection.
                continue;
            }
            if next.report.files_enqueued >= self.plan.budgets.max_files {
                end_discovery(&mut next, "budget_files");
                break;
            }
            let size = fields.get("size_bytes").and_then(|s| s.parse::<u64>().ok());
            let remaining = self
                .plan
                .budgets
                .max_bytes
                .saturating_sub(totals.bytes_done)
                .saturating_sub(reserved);
            if size.is_some_and(|s| s > remaining) || remaining == 0 {
                end_discovery(&mut next, "budget_bytes");
                break;
            }
            if next.chains[chain_index].depth.saturating_add(
                if self.plan.extract.follow.is_some() {
                    2
                } else {
                    1
                },
            ) > self.plan.budgets.max_depth
            {
                continue;
            }
            let index = next.report.files_enqueued + 1;
            let mut spec = self.job_spec(
                &fields,
                &document.final_url,
                index,
                next.report.pages_fetched,
            )?;
            let original = format!(
                "{}/{}",
                spec.dest_dir.as_deref().unwrap_or_default(),
                spec.filename.as_deref().unwrap_or("download")
            );
            if next.assigned_paths.contains(&original) {
                let filename = spec.filename.clone().unwrap_or_else(|| "download".into());
                for suffix in 2..=1_000_001u32 {
                    let candidate = unique_name(&filename, suffix);
                    let path = format!(
                        "{}/{}",
                        spec.dest_dir.as_deref().unwrap_or_default(),
                        candidate
                    );
                    if !next.assigned_paths.contains(&path) {
                        spec.filename = Some(candidate);
                        break;
                    }
                }
            }
            next.assigned_paths.insert(format!(
                "{}/{}",
                spec.dest_dir.as_deref().unwrap_or_default(),
                spec.filename.as_deref().unwrap_or("download")
            ));
            reserved = reserved.saturating_add(size.unwrap_or(0));
            next.report.files_enqueued += 1;
            intents.push(JobIntent {
                spec,
                index,
                on_existing: self.plan.sinks.on_existing,
                byte_budget_remaining: remaining,
                declared_size: size,
            });
            if next.keys.len() > 2_000_000 {
                return Err(RuntimeError::Policy("workflow.key_limit"));
            }
            if next
                .elapsed_ms
                .saturating_add(started.elapsed().as_millis() as u64)
                >= self.plan.budgets.max_duration_ms
            {
                end_discovery(&mut next, "budget_duration");
                break;
            }
        }
        if off_domain * 2 > extracted.total {
            drift(
                &self.plan,
                &mut next,
                Diagnostic::warning(
                    "plan.drift_off_domain",
                    "/extract",
                    "most document items leave approved domains",
                ),
            );
        }
        let chain = &mut next.chains[chain_index];
        chain.empty_pages = if newly_seen == 0 {
            chain.empty_pages.saturating_add(1)
        } else {
            0
        };
        let fingerprint = fingerprint.finalize().to_hex().to_string();
        let duplicate = extracted.total > 0 && chain.fingerprints.contains(&fingerprint);
        chain.fingerprints.push(fingerprint);
        if chain.fingerprints.len() > 64 {
            chain.fingerprints.remove(0);
        }
        if chain.next_url.is_some() {
            chain.next_url = next_url;
        }
        let reason = if self.plan.stop.stop_on_duplicate_page && duplicate {
            Some("duplicate_page")
        } else if chain.count_hint.is_some_and(|n| chain.items_seen >= n) {
            Some("count_reached")
        } else if self.plan.stop.no_new_items_pages > 0
            && chain.empty_pages >= self.plan.stop.no_new_items_pages
        {
            Some("no_new_items")
        } else {
            None
        };
        if let Some(reason) = reason {
            chain.next_url = None;
            if next.report.stop_reason.is_none() {
                next.report.stop_reason = Some(reason.into());
            }
        }
        if chain
            .next_url
            .as_ref()
            .is_some_and(|u| !allowed(&self.plan, u))
        {
            chain.next_url = None;
            drift(
                &self.plan,
                &mut next,
                Diagnostic::warning(
                    "plan.drift_off_domain",
                    "/pagination",
                    "next document leaves approved domains",
                ),
            );
        }
        if next.report.files_enqueued >= self.plan.budgets.max_files {
            end_discovery(&mut next, "budget_files");
        }
        next.elapsed_ms = next
            .elapsed_ms
            .saturating_add(started.elapsed().as_millis() as u64);
        if matches!(
            next.report.stop_reason.as_deref(),
            Some("budget_bytes" | "budget_duration")
        ) {
            port.control_jobs(next.report.run_id, RunAction::Cancel)
                .await?;
            next.report.files_enqueued = next
                .report
                .files_enqueued
                .saturating_sub(intents.len() as u32);
            intents.clear();
            next.report.status = RunStatus::BudgetExhausted;
            next.report.finished_at = Some(Timestamp::now());
        }
        self.save(port, next, &intents).await
    }
    fn sitemap_frontier(
        &self,
        next: &RunSnapshot,
        chain_index: usize,
        depth: u8,
        document: &Document,
    ) -> RuntimeResult<Vec<Chain>> {
        let source_index = next.chains[chain_index].source_index;
        let Some(crate::Source::Sitemap { loc_pattern, .. }) = self.plan.sources.get(source_index)
        else {
            return Err(RuntimeError::CheckpointConflict);
        };
        let map = crate::sitemap::parse(&document.body, &document.final_url)?;
        let pattern = loc_pattern
            .as_deref()
            .map(|p| regex::RegexBuilder::new(p).size_limit(1 << 20).build())
            .transpose()
            .map_err(|_| RuntimeError::Policy("workflow.sitemap_pattern"))?;
        let mut known: BTreeSet<_> = next
            .chains
            .iter()
            .map(|chain| {
                (
                    chain.source_index,
                    chain.sitemap_depth.is_some(),
                    chain.first_url.clone(),
                )
            })
            .collect();
        let mut added = vec![];
        for url in map.locations {
            if !allowed(&self.plan, &url) {
                return Err(RuntimeError::Policy("workflow.domain_not_allowed"));
            }
            let candidates = if map.kind == crate::sitemap::Kind::Index {
                if known.contains(&(source_index, true, url.clone())) {
                    continue;
                }
                if depth >= self.plan.budgets.max_depth {
                    return Err(RuntimeError::Policy("workflow.sitemap_depth"));
                }
                vec![pagination::sitemap_chain(source_index, url, depth + 1)]
            } else {
                if pattern
                    .as_ref()
                    .is_some_and(|pattern| !pattern.is_match(&url))
                {
                    continue;
                }
                pagination::sitemap_pages(&self.plan, source_index, vec![url])
                    .map_err(|d| RuntimeError::Invalid(vec![d]))?
            };
            for candidate in candidates {
                if known.insert((
                    source_index,
                    candidate.sitemap_depth.is_some(),
                    candidate.first_url.clone(),
                )) {
                    if next.chains.len() + added.len() >= 100_000 {
                        return Err(RuntimeError::Policy("workflow.sitemap_limit"));
                    }
                    added.push(candidate);
                }
            }
        }
        Ok(added)
    }
    async fn fetch(&self, port: &dyn RuntimePort, url: &str) -> RuntimeResult<Document> {
        self.fetch_kind(port, url, false).await
    }
    async fn fetch_kind(
        &self,
        port: &dyn RuntimePort,
        url: &str,
        sitemap: bool,
    ) -> RuntimeResult<Document> {
        if !allowed(&self.plan, url) {
            return Err(RuntimeError::Policy("workflow.domain_not_allowed"));
        }
        // The mandatory host gate in the embedding port owns pacing and cancellation.
        // Keeping a second sleep here would block pause until a long crawl delay elapsed.
        let document = port
            .fetch(FetchRequest {
                run_id: self.snapshot.report.run_id,
                url: url.into(),
                allowed_domains: self.plan.allowed_domains.clone(),
                min_interval_ms: self.plan.fetch.min_interval_ms,
                max_concurrency_per_host: self.plan.fetch.max_concurrency_per_host,
                session_origin: self.plan.session.as_ref().map(|s| s.origin.clone()),
                max_body_bytes: 8 * 1024 * 1024,
                accept: if sitemap {
                    "application/xml,text/xml;q=0.9"
                } else {
                    match self.plan.extract.items {
                        Selector::JsonPath { .. } => "application/json",
                        _ => "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1",
                    }
                }
                .into(),
            })
            .await?;
        if document.body.len() > 8 * 1024 * 1024 {
            return Err(RuntimeError::Policy("workflow.page_too_large"));
        }
        if !allowed(&self.plan, &document.final_url) {
            return Err(RuntimeError::Policy("workflow.domain_not_allowed"));
        }
        Ok(document)
    }
    async fn add_detail(
        &self,
        port: &dyn RuntimePort,
        next: &mut RunSnapshot,
        fields: &mut Fields,
        base: &Url,
    ) -> RuntimeResult<bool> {
        let Some(follow) = &self.plan.extract.follow else {
            return Ok(true);
        };
        if next.report.pages_fetched >= self.plan.budgets.max_pages {
            end_discovery(next, "budget_pages");
            return Ok(false);
        }
        let value = Template::parse(&follow.url)
            .and_then(|t| t.render(fields))
            .map_err(|_| {
                invalid(
                    "plan.template_invalid",
                    "detail template could not be rendered",
                )
            })?;
        let Some(url) = extract::resolve(base, &value) else {
            if let Some(counts) = &mut next.report.counts {
                counts.items_invalid = counts.items_invalid.saturating_add(1);
            }
            return Ok(false);
        };
        if !allowed(&self.plan, &url) {
            if let Some(counts) = &mut next.report.counts {
                counts.items_out_of_domain = counts.items_out_of_domain.saturating_add(1);
            }
            return Ok(false);
        }
        let document = self.fetch(port, &url).await?;
        next.report.pages_fetched += 1;
        let detail = extract::detail(
            &document.body,
            &document.final_url,
            &document.content_type,
            &follow.fields,
        )
        .map_err(|d| RuntimeError::Invalid(vec![d]))?;
        if let Some(detail) = detail {
            fields.extend(detail);
            Ok(true)
        } else {
            if let Some(counts) = &mut next.report.counts {
                counts.items_invalid = counts.items_invalid.saturating_add(1);
            }
            Ok(false)
        }
    }
    fn filters(&self, next: &mut RunSnapshot, fields: &Fields) -> RuntimeResult<bool> {
        let url = fields.get("url").map(String::as_str).unwrap_or("");
        let size = fields.get("size_bytes").and_then(|s| s.parse::<u64>().ok());
        let ext = Url::parse(url)
            .ok()
            .and_then(|u| u.path().rsplit('.').next().map(|s| s.to_ascii_lowercase()))
            .unwrap_or_default();
        for filter in &self.plan.filters {
            let keep = match filter {
                Filter::UrlRegex { pattern, invert } => matched(pattern, url)? != *invert,
                Filter::FieldRegex {
                    field,
                    pattern,
                    invert,
                } => fields
                    .get(field)
                    .is_some_and(|v| matched(pattern, v).is_ok_and(|m| m != *invert)),
                Filter::Mime { allow, unknown } => fields
                    .get("media_type")
                    .map(String::as_str)
                    .or_else(|| extract::guessed_mime(&ext))
                    .map_or(*unknown == UnknownMime::Keep, |v| {
                        extract::mime_matches(v, allow)
                    }),
                Filter::MaxSize { bytes } => size.is_none_or(|s| s <= *bytes),
                Filter::MinSize { bytes } => size.is_none_or(|s| s >= *bytes),
                Filter::Dedupe { key: template } => {
                    let text = Template::parse(template)
                        .and_then(|t| t.render(fields))
                        .map_err(|_| {
                            invalid(
                                "plan.template_invalid",
                                "dedupe template could not be rendered",
                            )
                        })?;
                    next.keys.insert(key("d", &text))
                }
            };
            if !keep {
                if let Some(counts) = &mut next.report.counts {
                    if matches!(filter, Filter::Dedupe { .. }) {
                        counts.items_duplicate = counts.items_duplicate.saturating_add(1);
                    } else {
                        counts.items_filtered = counts.items_filtered.saturating_add(1);
                    }
                }
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn job_spec(
        &self,
        fields: &Fields,
        page_url: &str,
        index: u32,
        page: u32,
    ) -> RuntimeResult<JobSpec> {
        let url = HttpUrl::parse(
            fields
                .get("url")
                .ok_or_else(|| invalid("plan.field_url_missing", "item has no download URL"))?,
        )
        .map_err(|_| invalid("plan.url_invalid", "invalid item URL"))?;
        let filename = url
            .url()
            .path_segments()
            .and_then(|mut p| p.next_back())
            .filter(|p| !p.is_empty())
            .unwrap_or("download");
        let mut values = fields.clone();
        values.insert("filename".into(), filename.into());
        values.insert(
            "ext".into(),
            filename
                .rsplit_once('.')
                .map(|(_, v)| v.to_ascii_lowercase())
                .unwrap_or_default(),
        );
        values.insert("index".into(), index.to_string());
        values.insert("page".into(), page.to_string());
        let mut rename = self.plan.post.rename.as_deref();
        let mut subdir = self.plan.post.subdir.as_deref();
        for rule in &self.plan.post.rules {
            let c = &rule.when;
            if (!c.ext.is_empty() && !c.ext.contains(&values["ext"]))
                || (!c.media_type.is_empty()
                    && !fields
                        .get("media_type")
                        .is_some_and(|v| extract::mime_matches(v, &c.media_type)))
                || c.url_regex
                    .as_ref()
                    .is_some_and(|p| !matched(p, url.expose()).unwrap_or(false))
                || c.field_regex.as_ref().is_some_and(|p| {
                    !fields
                        .get(&p.field)
                        .is_some_and(|v| matched(&p.pattern, v).unwrap_or(false))
                })
            {
                continue;
            }
            rename = rule.rename.as_deref().or(rename);
            subdir = rule.subdir.as_deref().or(subdir);
            break;
        }
        let rendered = rename
            .map(|t| Template::parse(t).and_then(|t| t.render(&values)))
            .transpose()
            .map_err(|_| {
                invalid(
                    "plan.template_invalid",
                    "filename template could not be rendered",
                )
            })?
            .unwrap_or_else(|| filename.into());
        let name = zeus_fs::names::sanitize_filename(&rendered, &Default::default())
            .map_err(|_| invalid("plan.filename_invalid", "filename cannot be sanitized"))?
            .name;
        let mut destination = self.plan.sinks.dest_dir.clone();
        if let Some(template) = subdir {
            let safe = values
                .iter()
                .map(|(k, v)| (k.clone(), v.replace(['/', '\\'], "_")))
                .collect();
            let rendered = Template::parse(template)
                .and_then(|t| t.render(&safe))
                .map_err(|_| {
                    invalid(
                        "plan.template_invalid",
                        "subdirectory template could not be rendered",
                    )
                })?;
            let components = rendered
                .split('/')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            if components.len() > 8
                || components
                    .iter()
                    .any(|c| matches!(*c, "." | "..") || c.contains('\\'))
            {
                return Err(invalid(
                    "plan.subdir_invalid",
                    "subdirectory escapes the approved destination",
                ));
            }
            for component in components {
                destination.push('/');
                destination.push_str(
                    &zeus_fs::names::sanitize_filename(component, &Default::default())
                        .map_err(|_| {
                            invalid("plan.subdir_invalid", "invalid subdirectory component")
                        })?
                        .name,
                );
            }
        }
        let mut source = JobSource::new(SourceKind::Workflow);
        source.via = Some(SourceVia::WorkflowRunner);
        source.run_id = Some(self.snapshot.report.run_id);
        let mut spec = JobSpec::new(url, source);
        spec.dest_dir = Some(destination);
        spec.filename = Some(name);
        spec.priority = Some(self.plan.fetch.priority);
        spec.max_connections = Some(self.plan.fetch.max_connections_per_file);
        spec.overwrite = Some(Overwrite::Rename);
        spec.referrer = HttpUrl::parse(page_url).ok();
        spec.expected_sha256 = fields.get("sha256").and_then(|s| {
            s.strip_prefix("sha256:")
                .unwrap_or(s)
                .to_ascii_lowercase()
                .parse()
                .ok()
        });
        spec.idempotency_key = Some(format!(
            "wf:{}:{}",
            self.snapshot.report.run_id,
            &blake3::hash(spec.url.expose().as_bytes()).to_hex().as_str()[..32]
        ));
        spec.labels.insert("run_item".into(), index.to_string());
        if let Some(license) = fields.get("license") {
            spec.labels.insert("license".into(), license.clone());
        }
        if let Some(session) = &self.plan.session
            && spec.origin() == session.origin
        {
            spec.session_origin = Some(session.origin.clone());
            spec.grant_scope_hint = Some(zeus_types::job::GrantScopeHint::Workflow);
        }
        if let Some(accept) = &self.plan.fetch.download_accept {
            spec.extra_headers.insert("Accept".into(), accept.clone());
        }
        Ok(spec)
    }
}
fn matched(pattern: &str, value: &str) -> RuntimeResult<bool> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map(|r| r.is_match(value))
        .map_err(|_| invalid("plan.selector_syntax", "invalid bounded regular expression"))
}
fn record_page_error(snapshot: &mut RunSnapshot, error: &RuntimeError) {
    let (code, class) = match error {
        RuntimeError::Policy(code) => (*code, zeus_types::ErrorClass::PolicyDenied),
        RuntimeError::Port(code) => (*code, zeus_types::ErrorClass::Unavailable),
        RuntimeError::Invalid(_) => (
            "workflow.plan_invalid",
            zeus_types::ErrorClass::InvalidRequest,
        ),
        RuntimeError::CheckpointConflict => ("api.conflict", zeus_types::ErrorClass::Conflict),
    };
    snapshot.report.pause_reason = Some(
        if code.starts_with("session.") {
            "session"
        } else {
            "page_error"
        }
        .into(),
    );
    snapshot.report.last_error = Some(zeus_types::ErrorEnvelope::new(
        zeus_types::ErrorCode::from_static(code),
        class,
        false,
        "Workflow document could not be fetched",
    ));
    if let Some(counts) = &mut snapshot.report.counts {
        if code == "policy.robots_disallowed" {
            counts.pages_skipped_robots = counts.pages_skipped_robots.saturating_add(1);
        } else {
            counts.pages_failed = counts.pages_failed.saturating_add(1);
        }
    }
}
fn record_drift(snapshot: &mut RunSnapshot, diagnostic: Diagnostic) {
    if snapshot.report.drift.len() < 100 {
        snapshot.report.drift.push(diagnostic);
    }
}
fn drift(plan: &Plan, snapshot: &mut RunSnapshot, diagnostic: Diagnostic) {
    record_drift(snapshot, diagnostic);
    match plan.stop.on_drift {
        OnDrift::Pause => {
            snapshot.report.status = RunStatus::Paused;
            snapshot.report.pause_reason = Some("drift".into());
            snapshot.last_error = Some("workflow.drift".into());
        }
        OnDrift::Stop => end_discovery(snapshot, "drift"),
        OnDrift::Continue => {}
    }
}
fn end_discovery(snapshot: &mut RunSnapshot, reason: &str) {
    for c in &mut snapshot.chains {
        c.next_url = None;
    }
    snapshot.report.stop_reason = Some(reason.into());
}
fn unique_name(name: &str, suffix: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem} ({suffix}).{ext}"),
        None => format!("{name} ({suffix})"),
    }
}
/// Redacted display URL helper for planner/report adapters.
pub fn display_url(url: &str) -> String {
    zeus_types::redact_url(url, RedactMode::Log).url
}
