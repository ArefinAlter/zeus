//! `workflows.*`: plan, dry run, run (later phase; the types are the contract).
//!
//! Lifecycle (ARCHITECTURE section 8): `workflows.plan` returns page samples and the plan JSON Schema; the host
//! agent returns a candidate plan; the daemon validates it **against the samples** before any approval;
//! `workflows.dry_run` shows what would happen; `workflows.run` needs a human approval of kind `workflow_plan`
//! bound to the plan hash.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::{ApprovalId, Origin, RunId};
use zeus_workflow::{Diagnostic, DryRunReport, PageSample, Plan, PlanId, RunReport, RunStatus};

use crate::paging::{Page, PageRequest};

/// `workflows.plan` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsPlanParams {
    /// Retry key for sampling or candidate validation; model drafting does not yet support keyed retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The goal in words. Untrusted text; recorded, never interpreted.
    pub goal: String,
    /// Start URLs to sample.
    pub start_urls: Vec<String>,
    /// Origin whose session the sampling may use (an effective grant must exist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_origin: Option<Origin>,
    /// A candidate plan to validate against the samples. Absent on the first call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<Plan>,
    /// Continue an existing draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<PlanId>,
    /// Ask the configured model to propose the selectors (human-only, verified terminal; ignored with `candidate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<WorkflowDraftRequest>,
}

/// A model-drafting request. The model sees text-free page outlines (`metadata_only`, 09) and proposes only the
/// `extract` member; domains, budgets, pace and destination are daemon defaults the person can edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowDraftRequest {
    /// Most this one request may cost, in micro-USD; zero allows only free local models.
    pub max_cost_micro_usd: u64,
}

/// Accounting of a model draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowDraftInfo {
    /// The provider that answered.
    pub provider: zeus_llm::ProviderKind,
    /// Model identifier as reported.
    pub model: String,
    /// Prompt tokens.
    pub input_tokens: u64,
    /// Generated tokens.
    pub output_tokens: u64,
    /// Cost in micro-USD when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micro_usd: Option<u64>,
}

/// `workflows.plan` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsPlanResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The stored draft.
    pub plan_id: PlanId,
    /// Page samples for the agent to write selectors against (bounded, untrusted content).
    pub samples: Vec<PageSample>,
    /// The JSON Schema of a plan.
    pub plan_schema: serde_json::Value,
    /// Findings about the candidate; a plan with errors cannot be approved.
    pub diagnostics: Vec<Diagnostic>,
    /// The accepted plan (normalized) when the candidate had no errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Plan>,
    /// Hash the approval will bind to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<String>,
    /// Present when the candidate was drafted by the configured model: a proposal, never an approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<WorkflowDraftInfo>,
}

/// `workflows.dry_run` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsDryRunParams {
    /// The plan.
    pub plan_id: PlanId,
    /// Items to list, 1 to 100.
    pub sample_items: u32,
}

/// `workflows.dry_run` result.
pub type WorkflowsDryRunResult = DryRunReport;

/// `workflows.run` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsRunParams {
    /// The plan.
    pub plan_id: PlanId,
    /// The hash the agent reviewed; a different current hash is refused.
    pub plan_hash: String,
    /// Idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `workflows.run` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsRunResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The run, created immediately (state `awaiting_approval` until a human approves).
    pub run_id: RunId,
    /// State.
    pub status: RunStatus,
    /// The approval of kind `workflow_plan`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<ApprovalId>,
}

/// `workflows.get` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsGetParams {
    /// The run.
    pub run_id: RunId,
}

/// `workflows.get` result.
pub type WorkflowsGetResult = RunReport;

/// `workflows.list` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsListParams {
    /// Paging: `limit` and `cursor` sit beside the filters (00 section 10).
    #[serde(flatten)]
    pub page: PageRequest,
}

/// `workflows.list` result.
pub type WorkflowsListResult = Page<RunReport>;

/// Control actions on a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunControl {
    /// Pause fetching; running downloads finish their commit.
    Pause,
    /// Resume from the checkpoint.
    Resume,
    /// Cancel the run and its unfinished jobs.
    Cancel,
}

/// `workflows.control` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsControlParams {
    /// Retry key scoped to this caller and method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The run.
    pub run_id: RunId,
    /// The action.
    pub action: RunControl,
}

/// `workflows.control` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsControlResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The run after the action.
    pub run: RunReport,
}

/// `workflows.manifest` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsManifestParams {
    /// Retry key for one immutable export snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The run.
    pub run_id: RunId,
    /// Output format.
    pub format: zeus_workflow::ManifestFormat,
}

/// `workflows.manifest` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowsManifestResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// Path of the manifest inside a write root.
    pub path: String,
    /// Records written.
    pub records: u64,
    /// Redacted export content when at most 512 KiB; larger exports remain on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Digest of the complete exported file, allowing safe inline resource verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<zeus_types::Hex256>,
}
