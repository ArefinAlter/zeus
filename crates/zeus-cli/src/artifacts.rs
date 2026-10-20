//! Artifact inspection and confined ingestion through the daemon API.

use clap::{Subcommand, ValueEnum};
use zeus_api::methods::artifacts::*;
use zeus_api::methods::{ArtifactsGet, ArtifactsIngest, ArtifactsLineage, ArtifactsList};
use zeus_api::{ApiError, ApiResult, Client, PageRequest, call};
use zeus_types::{ArtifactId, Hex256, JobId, RunId};

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
}

fn display(record: &serde_json::Value) -> String {
    // Portable records evolve independently. JSON mode retains all fields; terminal mode remains escaped.
    sanitize(&serde_json::to_string_pretty(record).unwrap_or_default())
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
            if idempotency_key.is_empty()
                || idempotency_key.len() > 128
                || idempotency_key.chars().any(char::is_control)
            {
                return Err(ApiError::invalid_params(
                    "idempotency_key must be 1–128 UTF-8 bytes without controls",
                ));
            }
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
    }
}
