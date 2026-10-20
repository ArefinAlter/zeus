//! `artifacts.*`: the content-addressed artifact store of spec 13, read through its export form (05 section 9.13).
//!
//! Every record is the store's portable export (artifact.v1 `Artifact`), privacy-reduced for the API: URL
//! queries dropped and session origins withheld. Confined ingestion registers a caller-produced local file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::{ArtifactId, Hex256, JobId, RunId};

use crate::paging::{Page, PageRequest};

/// `artifacts.get` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactsGetParams {
    /// The artifact (`art_...`).
    pub artifact_id: ArtifactId,
}

/// `artifacts.get` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactsGetResult {
    /// The export form (artifact.v1 `Artifact`, 13 section 10.1).
    pub artifact: serde_json::Value,
}

/// Record state filter of `artifacts.list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStateFilter {
    /// Active records.
    Active,
    /// Tombstones of deleted artifacts.
    Deleted,
}

/// `artifacts.list` parameters. Without `state`, active records and tombstones are both listed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactsListParams {
    /// Paging: `limit` and `cursor` sit beside the filters (00 section 10).
    #[serde(flatten)]
    pub page: PageRequest,
    /// Only artifacts with this BLAKE3 content identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<Hex256>,
    /// Only artifacts with this SHA-256 digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Hex256>,
    /// Only the artifact a job produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    /// Only artifacts of a workflow run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Only records in this state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ArtifactStateFilter>,
}

/// `artifacts.list` result: export forms, newest first.
pub type ArtifactsListResult = Page<serde_json::Value>;

/// Which way `artifacts.lineage` walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LineageDirection {
    /// Parents, grandparents, ...
    Ancestors,
    /// Artifacts derived from this one.
    Descendants,
}

/// `artifacts.lineage` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactsLineageParams {
    /// The start record.
    pub artifact_id: ArtifactId,
    /// Direction of the walk.
    pub direction: LineageDirection,
    /// Levels to follow, 1 to 64 (default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Nodes to return, 1 to 500 (default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<u32>,
}

/// An edge of a lineage node: the index of a parent (ancestors) or child (descendants) in `nodes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LineageLink {
    /// Index into `nodes`.
    pub node: u32,
    /// The parent role (`input`, `config`, `manifest` or a producer role).
    pub role: String,
}

/// One node of a lineage walk (13 section 4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LineageNode {
    /// The record; null for a content-only node (a parent known only by its digests).
    pub artifact_id: Option<ArtifactId>,
    /// BLAKE3 content identity.
    pub blake3: Hex256,
    /// Length.
    pub size_bytes: u64,
    /// Levels from the start record (0).
    pub depth: u32,
    /// Parents (ancestors) or children (descendants) of this node.
    pub links: Vec<LineageLink>,
}

/// `artifacts.lineage` result. Full records are read with `artifacts.get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactsLineageResult {
    /// Node 0 is the start record; records outside the caller's view are omitted.
    pub nodes: Vec<LineageNode>,
    /// A depth or node bound stopped the walk, or records were omitted.
    pub truncated: bool,
}

/// `artifacts.ingest` parameters: register a local file the caller produced (13 section 7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactsIngestParams {
    /// Absolute path of a regular file inside a write root the caller may use (a service: its roots).
    pub path: String,
    /// Name hint, 1 to 255 characters; the file name when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Claimed media type, 1 to 255 characters, recorded as `declared_media_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Inputs (artifact.v1 `ParentRef`), at most 1024. With parents the record is `derived`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<serde_json::Value>,
    /// The producing operation (artifact.v1 `Operation`); `zeus.ingest` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<serde_json::Value>,
    /// Caller metadata (artifact.v1 `Metadata`): at most 32 members. Untrusted data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
    /// Retry key (00 section 5.4); the ingest key is derived from it (05 section 9.13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `artifacts.ingest` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactsIngestResult {
    /// Export form of the new or existing record.
    pub artifact: serde_json::Value,
    /// This call committed the record.
    pub created: bool,
    /// The result came from the idempotency store (05 section 12).
    pub replayed: bool,
}
