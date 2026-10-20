//! Artifact inspection and confined ingestion through the daemon API.

use clap::{Subcommand, ValueEnum};
use std::path::PathBuf;
use zeus_api::methods::artifacts::*;
use zeus_api::methods::{
    ArtifactsGet, ArtifactsIngest, ArtifactsLifecycleApply, ArtifactsLifecyclePreview,
    ArtifactsLineage, ArtifactsList,
};
use zeus_api::{ApiError, ApiResult, Client, PageRequest, call};
use zeus_types::{ArtifactId, Hex256, JobId, RunId, Timestamp};

use crate::commands::CmdOutput;
use crate::output::sanitize;

/// Artifact state filter.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StateOpt {
    /// Active records.
    Active,
    /// Deleted record tombstones.
    Deleted,
}

/// Lineage traversal direction.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DirectionOpt {
    /// Parents and earlier inputs.
    Ancestors,
    /// Derived artifacts.
    Descendants,
}

/// Stored-content protection policy.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RetentionOpt {
    /// Protect bytes while the artifact record is active.
    Keep,
    /// Permit later collection of otherwise unprotected bytes.
    Evictable,
}

/// `zeus artifacts`: records, provenance and ingestion.
#[derive(Debug, Clone, Subcommand)]
pub enum ArtifactsCommand {
    /// List visible artifacts with optional digest or producer filters.
    Ls {
        /// Maximum records, 1–100.
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
        /// Cursor returned by a previous page.
        #[arg(long)]
        cursor: Option<String>,
        /// BLAKE3 content digest.
        #[arg(long)]
        blake3: Option<Hex256>,
        /// SHA-256 digest.
        #[arg(long)]
        sha256: Option<Hex256>,
        /// Producing job.
        #[arg(long)]
        job_id: Option<JobId>,
        /// Producing workflow run.
        #[arg(long)]
        run_id: Option<RunId>,
        /// Record state; absent includes tombstones.
        #[arg(long, value_enum)]
        state: Option<StateOpt>,
    },
    /// Inspect one artifact's portable record.
    Get {
        /// Artifact ID.
        artifact_id: ArtifactId,
    },
    /// Trace a bounded provenance graph.
    Lineage {
        /// Starting artifact ID.
        artifact_id: ArtifactId,
        /// Traversal direction.
        #[arg(long, value_enum, default_value = "ancestors")]
        direction: DirectionOpt,
        /// Maximum depth, 1–64.
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..=64))]
        max_depth: u32,
        /// Maximum nodes, 1–500.
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
        max_nodes: u32,
    },
    /// Hash and register a regular file in a permitted write root. Does not move or copy it.
    Ingest {
        /// Absolute file path; the daemon validates containment and hashes the file.
        path: String,
        /// Optional display name.
        #[arg(long)]
        name: Option<String>,
        /// Claimed media type.
        #[arg(long)]
        media_type: Option<String>,
        /// Retry key; repeating identical input returns its first receipt.
        #[arg(long)]
        idempotency_key: String,
    },
    /// Preview an audited record tombstone. User files and shared CAS bytes are retained.
    Delete {
        /// Record to tombstone after human approval.
        artifact_id: ArtifactId,
    },
    /// Preview an artifact retention change; weakening protection requires human approval.
    Retention {
        /// Active record.
        artifact_id: ArtifactId,
        /// Protection policy.
        #[arg(value_enum)]
        retention: RetentionOpt,
        /// Additional protection deadline (RFC 3339); absent removes that deadline.
        #[arg(long)]
        retain_until: Option<Timestamp>,
    },
    /// Preview a verified copy to a new file in a permitted write root.
    Materialize {
        /// Active, available, non-quarantined record.
        artifact_id: ArtifactId,
        /// Absolute destination. Existing files and partial files are never replaced.
        path: String,
    },
    /// Apply a saved --json lifecycle preview, or raise its required human approval.
    Apply {
        /// JSON receipt from artifacts delete, retention or materialize (at most 64 KiB).
        preview: PathBuf,
        /// Retry key; repeat the exact same file and key after a person approves the request.
        #[arg(long)]
        idempotency_key: String,
    },
}

fn display(record: &serde_json::Value) -> String {
    // Portable records evolve independently. JSON mode retains all fields; terminal mode remains escaped.
    sanitize(&serde_json::to_string_pretty(record).unwrap_or_default())
}

fn key_valid(key: &str) -> ApiResult<()> {
    if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
        Err(ApiError::invalid_params(
            "idempotency_key must be 1–128 UTF-8 bytes without controls",
        ))
    } else {
        Ok(())
    }
}

fn read_preview(path: &std::path::Path) -> ApiResult<ArtifactsLifecyclePreviewResult> {
    use std::io::Read;
    const MAX: u64 = 64 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|_| ApiError::invalid_params("cannot read lifecycle preview file"))?;
    if !file.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX) {
        return Err(ApiError::invalid_params(
            "lifecycle preview must be a regular JSON file at most 64 KiB",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ApiError::invalid_params("cannot read lifecycle preview file"))?;
    if bytes.len() > MAX as usize {
        return Err(ApiError::invalid_params("lifecycle preview exceeds 64 KiB"));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::invalid_params(
            "invalid lifecycle preview JSON; save the full --json preview receipt",
        )
    })
}

async fn preview<C: Client + ?Sized>(
    client: &C,
    artifact_id: ArtifactId,
    operation: ArtifactLifecycleOperation,
) -> ApiResult<CmdOutput> {
    let result = call::<ArtifactsLifecyclePreview, _>(
        client,
        ArtifactsLifecyclePreviewParams {
            artifact_id,
            operation,
        },
    )
    .await?;
    let text = format!(
        "{}\nPreview only. Save the full --json receipt, then use artifacts apply with a retry key.{}",
        display(
            &serde_json::to_value(&result)
                .map_err(|_| ApiError::internal("cannot serialize preview"))?
        ),
        if result.plan.requires_approval {
            " A person must approve this exact request before it executes."
        } else {
            ""
        },
    );
    Ok(CmdOutput::new(text, &result))
}

/// Execute artifact commands with the caller's existing daemon authority.
pub async fn run<C: Client + ?Sized>(
    command: &ArtifactsCommand,
    client: &C,
    dry_run: bool,
) -> ApiResult<CmdOutput> {
    match command {
        ArtifactsCommand::Ls {
            limit,
            cursor,
            blake3,
            sha256,
            job_id,
            run_id,
            state,
        } => {
            let page = call::<ArtifactsList, _>(
                client,
                ArtifactsListParams {
                    page: PageRequest {
                        limit: Some(*limit),
                        cursor: cursor.clone(),
                    },
                    blake3: *blake3,
                    sha256: *sha256,
                    job_id: *job_id,
                    run_id: *run_id,
                    state: state.map(|s| match s {
                        StateOpt::Active => ArtifactStateFilter::Active,
                        StateOpt::Deleted => ArtifactStateFilter::Deleted,
                    }),
                },
            )
            .await?;
            let mut text = page
                .items
                .iter()
                .map(display)
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(cursor) = &page.next_cursor {
                text.push_str(&format!("\nnext cursor: {}", sanitize(cursor)));
            }
            Ok(CmdOutput::new(text, &page).with_items(page.items))
        }
        ArtifactsCommand::Get { artifact_id } => {
            let result = call::<ArtifactsGet, _>(
                client,
                ArtifactsGetParams {
                    artifact_id: *artifact_id,
                },
            )
            .await?;
            Ok(CmdOutput::new(display(&result.artifact), &result))
        }
        ArtifactsCommand::Lineage {
            artifact_id,
            direction,
            max_depth,
            max_nodes,
        } => {
            let result = call::<ArtifactsLineage, _>(
                client,
                ArtifactsLineageParams {
                    artifact_id: *artifact_id,
                    direction: match direction {
                        DirectionOpt::Ancestors => LineageDirection::Ancestors,
                        DirectionOpt::Descendants => LineageDirection::Descendants,
                    },
                    max_depth: Some(*max_depth),
                    max_nodes: Some(*max_nodes),
                },
            )
            .await?;
            let text = display(
                &serde_json::to_value(&result)
                    .map_err(|_| ApiError::internal("cannot serialize lineage"))?,
            );
            Ok(CmdOutput::new(text, &result).with_items(
                result
                    .nodes
                    .iter()
                    .map(|n| serde_json::to_value(n).unwrap_or_default())
                    .collect(),
            ))
        }
        ArtifactsCommand::Ingest {
            path,
            name,
            media_type,
            idempotency_key,
        } => {
            if dry_run {
                return Err(ApiError::invalid_params(
                    "Artifact ingestion has no preview; nothing was changed",
                ));
            }
            key_valid(idempotency_key)?;
            let result = call::<ArtifactsIngest, _>(
                client,
                ArtifactsIngestParams {
                    path: path.clone(),
                    name: name.clone(),
                    media_type: media_type.clone(),
                    parents: Vec::new(),
                    operation: None,
                    metadata: None,
                    idempotency_key: Some(idempotency_key.clone()),
                },
            )
            .await?;
            Ok(CmdOutput::new(display(&result.artifact), &result))
        }
        ArtifactsCommand::Delete { artifact_id } => {
            preview(client, *artifact_id, ArtifactLifecycleOperation::Delete).await
        }
        ArtifactsCommand::Retention {
            artifact_id,
            retention,
            retain_until,
        } => {
            preview(
                client,
                *artifact_id,
                ArtifactLifecycleOperation::SetRetention {
                    retention: match retention {
                        RetentionOpt::Keep => ArtifactRetention::Keep,
                        RetentionOpt::Evictable => ArtifactRetention::Evictable,
                    },
                    retain_until: *retain_until,
                },
            )
            .await
        }
        ArtifactsCommand::Materialize { artifact_id, path } => {
            preview(
                client,
                *artifact_id,
                ArtifactLifecycleOperation::Materialize { path: path.clone() },
            )
            .await
        }
        ArtifactsCommand::Apply {
            preview,
            idempotency_key,
        } => {
            key_valid(idempotency_key)?;
            let saved = read_preview(preview)?;
            if dry_run {
                return Ok(CmdOutput::new(
                    "Lifecycle preview loaded; --dry-run made no API mutation.",
                    &saved,
                ));
            }
            let result = call::<ArtifactsLifecycleApply, _>(
                client,
                ArtifactsLifecycleApplyParams {
                    plan: saved.plan,
                    plan_hash: saved.plan_hash,
                    idempotency_key: idempotency_key.clone(),
                },
            )
            .await?;
            let text = if let Some(id) = result.approval_id.filter(|_| !result.applied) {
                format!(
                    "Approval {id} is pending. Review it with zeus approvals approve {id}, then repeat this exact apply command."
                )
            } else {
                display(
                    &serde_json::to_value(&result)
                        .map_err(|_| ApiError::internal("cannot serialize lifecycle receipt"))?,
                )
            };
            Ok(CmdOutput::new(text, &result))
        }
    }
}
