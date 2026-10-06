//! Tool inputs. Small, explicit and strict: no "do what the page says" parameters exist, mutating tools require an
//! `idempotency_key`, and every field an agent supplies is validated before anything reaches the daemon.

use schemars::JsonSchema;
use serde::Deserialize;

/// `zeus_probe` input.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProbeArgs {
    /// The http(s) URL to check.
    pub url: String,
    /// Origin (`https://host`) whose logged-in session might be needed, if you already know it.
    #[serde(default)]
    pub session_origin: Option<String>,
}

/// Priority of a download.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PriorityArg {
    /// Background.
    Low,
    /// Default.
    Normal,
    /// Soon.
    High,
}

/// `zeus_enqueue` input.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EnqueueArgs {
    /// One to 50 http(s) URLs to download.
    pub urls: Vec<String>,
    /// Folder to save into, inside the user's download folders. Default: the main Zeus folder.
    #[serde(default)]
    pub dest_dir: Option<String>,
    /// File name hint (only with exactly one URL). The server's name wins; names are sanitised.
    #[serde(default)]
    pub filename: Option<String>,
    /// Expected SHA-256 (64 hex digits), only with exactly one URL. A mismatch fails the job.
    #[serde(default)]
    pub expected_sha256: Option<String>,
    /// Origin whose logged-in session the download needs. Without a grant this raises a request for the user.
    #[serde(default)]
    pub session_origin: Option<String>,
    /// How soon.
    #[serde(default)]
    pub priority: Option<PriorityArg>,
    /// Preview names and destinations without creating jobs.
    #[serde(default)]
    pub dry_run: bool,
    /// Any string unique to this intent (1 to 128 characters). Repeating the call with the same key returns the
    /// same jobs instead of downloading twice.
    pub idempotency_key: String,
}

/// `zeus_status` input.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct StatusArgs {
    /// One job. Absent: the most recent jobs.
    #[serde(default)]
    pub job_id: Option<String>,
    /// How many recent jobs when `job_id` is absent, 1 to 50 (default 10).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// What `zeus_wait` stops for.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitUntilArg {
    /// Any job finishes, fails, or needs a person or the browser (default).
    #[default]
    SettledOrBlocked,
    /// Every job reaches a final state.
    AllFinal,
    /// Any change.
    AnyChange,
}

/// `zeus_wait` input.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct WaitArgs {
    /// One to 20 job ids.
    pub job_ids: Vec<String>,
    /// Seconds to wait, 1 to 60 (default 30).
    #[serde(default)]
    pub timeout_s: Option<u32>,
    /// What to wait for.
    #[serde(default)]
    pub until: WaitUntilArg,
}

/// Control action.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlActionArg {
    /// Pause.
    Pause,
    /// Resume a paused job.
    Resume,
    /// Cancel and delete partial data.
    Cancel,
    /// Retry a failed job.
    Retry,
    /// Retry a job that is waiting to retry, now.
    RetryNow,
    /// Change the priority.
    Reprioritize,
}

/// `zeus_control` input.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ControlArgs {
    /// The job.
    pub job_id: String,
    /// What to do.
    pub action: ControlActionArg,
    /// New priority, only for `reprioritize`.
    #[serde(default)]
    pub priority: Option<PriorityArg>,
    /// Any string unique to this intent (1 to 128 characters).
    pub idempotency_key: String,
}

/// `zeus_verify` input.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct VerifyArgs {
    /// A finished job.
    pub job_id: String,
    /// Expected SHA-256 (64 hex digits).
    #[serde(default)]
    pub expected_sha256: Option<String>,
}

/// `zeus_session_list` input.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct SessionListArgs {
    /// Only this origin.
    #[serde(default)]
    pub origin: Option<String>,
    /// Include expired and revoked grants.
    #[serde(default)]
    pub include_inactive: bool,
}

/// Scope an agent may suggest. (The person chooses; `never` cannot be requested.)
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopeArg {
    /// This one job.
    Once,
    /// This site for 24 hours.
    Site,
    /// A multi-step workflow for up to 7 days.
    Workflow,
}

/// `zeus_session_request` input.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SessionRequestArgs {
    /// The site (`https://host`) whose logged-in session is needed.
    pub origin: String,
    /// The scope you suggest; the person chooses.
    pub scope: ScopeArg,
    /// Why, in one sentence (at most 280 characters). It is shown to the person as "the agent says".
    pub reason: String,
    /// The job that needs it.
    #[serde(default)]
    pub job_id: Option<String>,
    /// Any string unique to this intent (1 to 128 characters).
    pub idempotency_key: String,
}

/// `zeus_files_list` input.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct FilesListArgs {
    /// Folder inside the download folders. Default: the main Zeus folder.
    #[serde(default)]
    pub dir: Option<String>,
    /// Include sub-folders.
    #[serde(default)]
    pub recursive: bool,
    /// How many files, 1 to 100 (default 50).
    #[serde(default)]
    pub limit: Option<u32>,
}
