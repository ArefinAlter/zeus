//! Workflow commands: sample pages, validate a plan file, dry-run it and start an approval-bound run.
//!
//! Every step is a daemon call (05 section 9.7, spec 08). The CLI never fills in a plan or approves one: a run
//! starts in `awaiting_approval` and a person approves it with `zeus approvals approve`.
use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};
use zeus_api::methods::workflows::{
    RunControl, WorkflowsControlParams, WorkflowsDryRunParams, WorkflowsGetParams,
    WorkflowsListParams, WorkflowsManifestParams, WorkflowsPlanParams, WorkflowsRunParams,
};
use zeus_api::methods::{
    WorkflowsControl, WorkflowsDryRun, WorkflowsGet, WorkflowsList, WorkflowsManifest,
    WorkflowsPlan, WorkflowsRun,
};
use zeus_api::{ApiError, ApiResult, Client, PageRequest, call};
use zeus_workflow::{Diagnostic, ManifestFormat, Plan, PlanId, RunReport};

use crate::commands::CmdOutput;
use crate::output::sanitize;

/// Largest plan file the CLI reads (the daemon's request limit is 1 MiB).
const MAX_PLAN_FILE_BYTES: u64 = 512 * 1024;

/// Manifest export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FormatOpt {
    /// One JSON object per line.
    Jsonl,
    /// Comma-separated values.
    Csv,
    /// Croissant metadata.
    Croissant,
}

/// `zeus workflow`.
#[derive(Debug, Clone, Subcommand)]
pub enum WorkflowCommand {
    /// Fetch bounded page samples, or validate a plan file against them. Never downloads anything.
    Plan {
        /// What you want, in words. Recorded, never interpreted.
        #[arg(long)]
        goal: String,
        /// Start URLs to sample (1 to 3).
        #[arg(long = "url", required = true)]
        urls: Vec<String>,
        /// A plan JSON file to validate against the samples (spec 08).
        #[arg(long)]
        candidate: Option<std::path::PathBuf>,
        /// Continue an existing draft (`plan_...`).
        #[arg(long)]
        plan_id: Option<String>,
        /// Ask the configured model to propose the selectors from text-free page outlines (a verified terminal).
        #[arg(long, conflicts_with = "candidate")]
        draft: bool,
        /// Most the draft may cost, in micro-USD; 0 allows only free local models.
        #[arg(long, default_value_t = 0, requires = "draft")]
        max_cost_micro_usd: u64,
    },
    /// Show what a validated plan would download: names, collisions and estimates.
    DryRun {
        /// The plan (`plan_...`).
        plan_id: String,
        /// Items to list (1 to 100).
        #[arg(long, default_value_t = 20)]
        items: u32,
    },
    /// Create a run bound to the reviewed plan hash; it waits for a person to approve it.
    Run {
        /// The plan (`plan_...`).
        plan_id: String,
        /// The plan hash shown by `workflow plan`; a changed plan is refused.
        #[arg(long)]
        plan_hash: String,
        /// Make a retried command safe: the same key returns the first result.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Show one run.
    Get {
        /// The run (`run_...`).
        run_id: String,
    },
    /// List runs.
    Ls {
        /// How many (1 to 100).
        #[arg(short, long, default_value_t = 20)]
        limit: u32,
    },
    /// Pause a run; running downloads finish their commit.
    Pause {
        /// The run.
        run_id: String,
    },
    /// Resume a paused run from its checkpoint.
    Resume {
        /// The run.
        run_id: String,
    },
    /// Cancel a run and its unfinished jobs.
    Cancel {
        /// The run.
        run_id: String,
    },
    /// Write the run's provenance manifest inside its write root.
    Manifest {
        /// The run.
        run_id: String,
        /// Output format.
        #[arg(long, value_enum, default_value = "jsonl")]
        format: FormatOpt,
    },
}

fn plan_id(text: &str) -> ApiResult<PlanId> {
    PlanId::parse(text).map_err(|_| {
        ApiError::invalid_params("a plan id looks like plan_ followed by 26 characters")
    })
}

fn run_id(text: &str) -> ApiResult<zeus_types::RunId> {
    text.parse()
        .map_err(|_| ApiError::invalid_params("a run id looks like run_ followed by 26 characters"))
}

fn read_plan(path: &std::path::Path) -> ApiResult<Plan> {
    let unreadable = || ApiError::invalid_params("the plan file cannot be read");
    let metadata = std::fs::metadata(path).map_err(|_| unreadable())?;
    if !metadata.is_file() || metadata.len() > MAX_PLAN_FILE_BYTES {
        return Err(ApiError::invalid_params(
            "the plan file must be a regular file of at most 512 KiB",
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|_| unreadable())?;
    serde_json::from_str(&text).map_err(|e| {
        ApiError::invalid_params(format!(
            "the plan file is not a valid plan: line {}, column {}",
            e.line(),
            e.column()
        ))
    })
}

fn diagnostics(list: &[Diagnostic]) -> String {
    list.iter()
        .map(|d| {
            format!(
                "  {:?} {} at {}: {}",
                d.severity,
                sanitize(&d.code),
                sanitize(&d.path),
                sanitize(&d.message)
            )
            .to_lowercase()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn report_line(run: &RunReport) -> String {
    format!(
        "{}  {}  pages {}  files {}/{} done, {} failed  {} bytes{}",
        run.run_id,
        serde_json::to_value(run.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        run.pages_fetched,
        run.files_completed,
        run.files_enqueued,
        run.files_failed,
        run.bytes_done,
        run.stop_reason
            .as_deref()
            .map(|r| format!("  stopped: {}", sanitize(r)))
            .unwrap_or_default()
    )
}

fn value(v: &impl serde::Serialize) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// Run a workflow command against the daemon.
pub async fn run<C: Client + ?Sized>(
    command: &WorkflowCommand,
    client: &C,
    dry_run: bool,
) -> ApiResult<CmdOutput> {
    if dry_run
        && matches!(
            command,
            WorkflowCommand::Run { .. }
                | WorkflowCommand::Pause { .. }
                | WorkflowCommand::Resume { .. }
                | WorkflowCommand::Cancel { .. }
                | WorkflowCommand::Manifest { .. }
        )
    {
        return Err(ApiError::invalid_params(
            "Use workflow dry-run to preview a plan; nothing was changed",
        ));
    }
    match command {
        WorkflowCommand::Plan {
            goal,
            urls,
            candidate,
            plan_id: draft_id,
            draft,
            max_cost_micro_usd,
        } => {
            let candidate = candidate.as_deref().map(read_plan).transpose()?;
            let validating = candidate.is_some() || *draft;
            let result = call::<WorkflowsPlan, _>(
                client,
                WorkflowsPlanParams {
                    idempotency_key: None,
                    goal: goal.clone(),
                    start_urls: urls.clone(),
                    session_origin: None,
                    candidate,
                    plan_id: draft_id.as_deref().map(plan_id).transpose()?,
                    draft: draft.then_some(zeus_api::methods::workflows::WorkflowDraftRequest {
                        max_cost_micro_usd: *max_cost_micro_usd,
                    }),
                },
            )
            .await?;
            let mut text = format!(
                "plan {}  {} page sample(s)",
                result.plan_id,
                result.samples.len()
            );
            for sample in &result.samples {
                text.push_str(&format!(
                    "\n  {}  {} bytes",
                    sanitize(&sample.url_redacted),
                    sample.body.len()
                ));
            }
            if let Some(info) = &result.draft {
                text.push_str(&format!(
                    "\nselectors proposed by {} {} ({} in, {} out tokens{}); review the plan before running it",
                    value(&info.provider).as_str().unwrap_or("model"),
                    sanitize(&info.model),
                    info.input_tokens,
                    info.output_tokens,
                    info.cost_micro_usd
                        .map(|c| format!(", {c} micro-USD"))
                        .unwrap_or_default()
                ));
            }
            if !result.diagnostics.is_empty() {
                text.push_str("\ndiagnostics:\n");
                text.push_str(&diagnostics(&result.diagnostics));
            }
            match (&result.plan_hash, validating) {
                (Some(hash), _) => text.push_str(&format!(
                    "\naccepted; plan hash {hash}\nnext: zeus workflow dry-run {} then zeus workflow run {} --plan-hash {hash}",
                    result.plan_id, result.plan_id
                )),
                (None, true) => text.push_str("\nthe plan has errors and cannot be run"),
                (None, false) => text.push_str(
                    "\nwrite a plan against these samples (zeus --json workflow plan ... prints them and the plan schema), then validate it with --candidate",
                ),
            }
            let exit = if validating && result.plan_hash.is_none() {
                crate::exit::Exit::Usage
            } else {
                crate::exit::Exit::Success
            };
            Ok(CmdOutput::new(text, &result).with_exit(exit))
        }
        WorkflowCommand::DryRun { plan_id: id, items } => {
            let report = call::<WorkflowsDryRun, _>(
                client,
                WorkflowsDryRunParams {
                    plan_id: plan_id(id)?,
                    sample_items: (*items).clamp(1, 100),
                },
            )
            .await?;
            let estimate = |e: &Option<zeus_workflow::Estimate>| {
                e.as_ref()
                    .map(|e| format!("{} ({}-{})", e.point, e.low, e.high))
                    .unwrap_or_else(|| "unknown".into())
            };
            let mut text = format!(
                "dry run of {}{}  pages sampled {}  files {}  bytes {}  collisions {}",
                report.plan_hash,
                if report.complete { "" } else { " (incomplete)" },
                report.sampled_pages,
                estimate(&report.estimate_files),
                estimate(&report.estimate_bytes),
                report.name_collisions
            );
            for item in &report.sample {
                text.push_str(&format!(
                    "\n  {:>3}  {}  {}{}",
                    item.index,
                    sanitize(&item.file_name),
                    sanitize(&item.url_redacted),
                    item.collides_with
                        .map(|other| format!("  collides with {other}"))
                        .unwrap_or_default()
                ));
            }
            if !report.warnings.is_empty() {
                text.push_str("\nwarnings:\n");
                text.push_str(&diagnostics(&report.warnings));
            }
            let items = report.sample.iter().map(value).collect();
            Ok(CmdOutput::new(text, &report).with_items(items))
        }
        WorkflowCommand::Run {
            plan_id: id,
            plan_hash,
            idempotency_key,
        } => {
            let result = call::<WorkflowsRun, _>(
                client,
                WorkflowsRunParams {
                    plan_id: plan_id(id)?,
                    plan_hash: plan_hash.clone(),
                    idempotency_key: idempotency_key.clone(),
                },
            )
            .await?;
            let text = match result.approval_id {
                Some(approval) => format!(
                    "run {} is waiting for a person to approve it: zeus approvals approve {approval}",
                    result.run_id
                ),
                None => format!("run {} {}", result.run_id, value(&result.status)),
            };
            Ok(CmdOutput::new(text, &result))
        }
        WorkflowCommand::Get { run_id: id } => {
            let report = call::<WorkflowsGet, _>(
                client,
                WorkflowsGetParams {
                    run_id: run_id(id)?,
                },
            )
            .await?;
            let mut text = report_line(&report);
            if !report.drift.is_empty() {
                text.push_str("\ndrift:\n");
                text.push_str(&diagnostics(&report.drift));
            }
            Ok(CmdOutput::new(text, &report))
        }
        WorkflowCommand::Ls { limit } => {
            let page = call::<WorkflowsList, _>(
                client,
                WorkflowsListParams {
                    page: PageRequest {
                        limit: Some((*limit).clamp(1, 100)),
                        cursor: None,
                    },
                },
            )
            .await?;
            let text = page
                .items
                .iter()
                .map(report_line)
                .collect::<Vec<_>>()
                .join("\n");
            let items = page.items.iter().map(value).collect();
            Ok(CmdOutput::new(text, &page).with_items(items))
        }
        WorkflowCommand::Pause { run_id: id }
        | WorkflowCommand::Resume { run_id: id }
        | WorkflowCommand::Cancel { run_id: id } => {
            let action = match command {
                WorkflowCommand::Pause { .. } => RunControl::Pause,
                WorkflowCommand::Resume { .. } => RunControl::Resume,
                _ => RunControl::Cancel,
            };
            let result = call::<WorkflowsControl, _>(
                client,
                WorkflowsControlParams {
                    idempotency_key: None,
                    run_id: run_id(id)?,
                    action,
                },
            )
            .await?;
            Ok(CmdOutput::new(report_line(&result.run), &result))
        }
        WorkflowCommand::Manifest { run_id: id, format } => {
            let result = call::<WorkflowsManifest, _>(
                client,
                WorkflowsManifestParams {
                    idempotency_key: None,
                    run_id: run_id(id)?,
                    format: match format {
                        FormatOpt::Jsonl => ManifestFormat::Jsonl,
                        FormatOpt::Csv => ManifestFormat::Csv,
                        FormatOpt::Croissant => ManifestFormat::Croissant,
                    },
                },
            )
            .await?;
            Ok(CmdOutput::new(
                format!(
                    "{} record(s) written to {}",
                    result.records,
                    sanitize(&result.path)
                ),
                &json!({"path": result.path, "records": result.records, "sha256": result.sha256, "replayed": result.replayed}),
            ))
        }
    }
}
