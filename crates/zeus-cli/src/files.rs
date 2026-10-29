//! File commands: list the write roots, organise a directory from a rule file, undo an applied operation.
//!
//! Organising is a dry run unless `--apply` is given, and applying only raises a `destructive_op` approval: files
//! move after a person approves it with `zeus approvals approve` (05 section 9.6, 07 section 4.1).
use clap::Subcommand;
use zeus_api::methods::files::FilesOperationGetParams;
use zeus_api::methods::files::{FileStep, FilesListParams, FilesOrganizeParams, FilesUndoParams};
use zeus_api::methods::{FilesList, FilesOperationGet, FilesOrganize, FilesUndo};
use zeus_api::{ApiError, ApiResult, Client, PageRequest, call};

use crate::commands::CmdOutput;
use crate::output::sanitize;

/// Largest rule file the CLI reads.
const MAX_RULE_FILE_BYTES: u64 = 64 * 1024;

/// `zeus files`.
#[derive(Debug, Clone, Subcommand)]
pub enum FilesCommand {
    /// List files under a write root (Zeus downloads show their job and SHA-256).
    List {
        /// A directory inside a write root; default the first root.
        dir: Option<String>,
        /// Recurse into sub-directories.
        #[arg(long)]
        recursive: bool,
        /// Include the `.zpart` files of unfinished downloads.
        #[arg(long)]
        partial: bool,
        /// How many (1 to 100).
        #[arg(short, long, default_value_t = 50)]
        limit: u32,
    },
    /// Plan moves, renames and trash operations for one directory from a JSON rule file.
    Organize {
        /// The directory, absolute or relative to the first write root.
        dir: String,
        /// Rule set: {"recursive"?, "rules": [{"when": {...}, "subdir"?, "rename"?, "trash"?}]}.
        #[arg(long)]
        rules: std::path::PathBuf,
        /// Ask for approval to apply the plan (without it, nothing is stored or changed).
        #[arg(long)]
        apply: bool,
        /// Make a retried command safe: the same key returns the first result.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Read current durable organization progress without performing an effect.
    Status {
        /// Operation returned by organize or undo.
        operation_id: String,
    },
    /// Plan the reversal of an applied operation; it runs after a person approves it.
    Undo {
        /// The operation (`fop_...`) printed by `files organize --apply`.
        operation_id: String,
    },
}

fn steps_text(steps: &[FileStep]) -> String {
    steps
        .iter()
        .map(|s| {
            format!(
                "  {:<6} {}{}",
                s.op,
                sanitize(&s.from),
                s.to.as_deref()
                    .map(|to| format!(" -> {}", sanitize(to)))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_rules(path: &std::path::Path) -> ApiResult<serde_json::Value> {
    let unreadable = || ApiError::invalid_params("the rule file cannot be read");
    let metadata = std::fs::metadata(path).map_err(|_| unreadable())?;
    if !metadata.is_file() || metadata.len() > MAX_RULE_FILE_BYTES {
        return Err(ApiError::invalid_params(
            "the rule file must be a regular file of at most 64 KiB",
        ));
    }
    serde_json::from_str(&std::fs::read_to_string(path).map_err(|_| unreadable())?).map_err(|e| {
        ApiError::invalid_params(format!("the rule file is not JSON: line {}", e.line()))
    })
}

/// Run a file command against the daemon.
pub async fn run<C: Client + ?Sized>(
    command: &FilesCommand,
    client: &C,
    dry_run: bool,
) -> ApiResult<CmdOutput> {
    match command {
        FilesCommand::List {
            dir,
            recursive,
            partial,
            limit,
        } => {
            let page = call::<FilesList, _>(
                client,
                FilesListParams {
                    page: PageRequest {
                        limit: Some((*limit).clamp(1, 100)),
                        cursor: None,
                    },
                    dir: dir.clone(),
                    recursive: *recursive,
                    include_partial: *partial,
                },
            )
            .await?;
            let text = page
                .items
                .iter()
                .map(|f| {
                    format!(
                        "{}  {} bytes{}{}{}",
                        sanitize(&f.path),
                        f.size_bytes,
                        f.job_id.map(|j| format!("  {j}")).unwrap_or_default(),
                        if f.quarantined { "  quarantined" } else { "" },
                        if f.partial { "  partial" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let items = page
                .items
                .iter()
                .map(|f| serde_json::to_value(f).unwrap_or_default())
                .collect();
            Ok(CmdOutput::new(text, &page).with_items(items))
        }
        FilesCommand::Status { operation_id } => {
            let result = call::<FilesOperationGet, _>(
                client,
                FilesOperationGetParams {
                    operation_id: operation_id.clone(),
                },
            )
            .await?;
            let text = format!(
                "{}: {} ({} done, {} skipped, {} pending)",
                sanitize(&result.operation_id),
                sanitize(&result.state),
                result.completed,
                result.skipped,
                result.pending
            );
            Ok(CmdOutput::new(text, &result))
        }
        FilesCommand::Organize {
            dir,
            rules,
            apply,
            idempotency_key,
        } => {
            if *apply && dry_run {
                return Err(ApiError::invalid_params(
                    "--apply and --dry-run contradict each other; nothing was changed",
                ));
            }
            let result = call::<FilesOrganize, _>(
                client,
                FilesOrganizeParams {
                    dir: dir.clone(),
                    rules: read_rules(rules)?,
                    dry_run: !*apply,
                    idempotency_key: idempotency_key.clone(),
                },
            )
            .await?;
            let mut text = if result.steps.is_empty() {
                "nothing to change".to_owned()
            } else {
                format!(
                    "{} step(s):\n{}",
                    result.steps.len(),
                    steps_text(&result.steps)
                )
            };
            match (&result.approval_id, &result.operation_id) {
                (Some(approval), Some(operation)) => text.push_str(&format!(
                    "\noperation {operation} waits for a person: zeus approvals approve {approval}"
                )),
                _ if !*apply && !result.steps.is_empty() => {
                    text.push_str("\ndry run: add --apply to ask for approval")
                }
                _ => {}
            }
            Ok(CmdOutput::new(text, &result))
        }
        FilesCommand::Undo { operation_id } => {
            if dry_run {
                return Err(ApiError::invalid_params(
                    "files undo always asks for approval first; nothing was changed",
                ));
            }
            let result = call::<FilesUndo, _>(
                client,
                FilesUndoParams {
                    operation_id: operation_id.clone(),
                    idempotency_key: None,
                },
            )
            .await?;
            let mut text = if result.steps.is_empty() {
                "nothing left to revert".to_owned()
            } else {
                format!(
                    "{} step(s):\n{}",
                    result.steps.len(),
                    steps_text(&result.steps)
                )
            };
            if let Some(approval) = result.approval_id {
                text.push_str(&format!(
                    "\nthe undo waits for a person: zeus approvals approve {approval}"
                ));
            }
            Ok(CmdOutput::new(text, &result))
        }
    }
}
