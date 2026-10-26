//! `queues.*`: download queues, their schedules and the speed limiter (05 section 9.14).
//!
//! A queue holds downloads until it runs. It runs when a person starts it or when its schedule says so, releases at
//! most `max_concurrent` of its downloads at a time in its stored order, retries failures that are worth retrying,
//! and optionally stops at a time of day. The speed limiter bounds the transfer rate of everything the daemon
//! downloads, and each queue may bound its own share further. Membership is chosen at enqueue time with the label
//! [`QUEUE_LABEL`] or later with `queues.control`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::JobId;

/// The job label that names the queue a download joins when it is enqueued.
pub const QUEUE_LABEL: &str = "zeus.queue";

/// Id of the built-in queue, which always exists and cannot be deleted.
pub const MAIN_QUEUE: &str = "main";

/// Most queues the daemon stores.
pub const MAX_QUEUES: usize = 32;

/// Most queued downloads (waiting, held or running) the daemon tracks across all queues.
pub const MAX_QUEUED_JOBS: usize = 5_000;

/// Largest speed limit accepted, in KiB/s (about 10 GiB/s).
pub const MAX_LIMIT_KIB_S: u32 = 10_000_000;

/// What a queue does. Only downloads exist today.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueueKind {
    /// Downloads each file once.
    #[default]
    Download,
}

/// How a start time repeats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    /// Starts once, on `start_date`.
    Once,
    /// Starts on each listed weekday.
    #[default]
    Days,
}

/// When a queue starts and stops. Times are wall-clock `HH:MM` in the configured time zone; days are 0 (Sunday)
/// to 6. A stop time earlier than the start time means the window runs past midnight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueueSchedule {
    /// Start at `start_time`.
    #[serde(default)]
    pub start_enabled: bool,
    /// `HH:MM`.
    #[serde(default = "default_start_time")]
    pub start_time: String,
    /// `once` runs on `start_date`; `days` repeats on `days`.
    #[serde(default)]
    pub repeat: Repeat,
    /// `YYYY-MM-DD`, used when `repeat` is `once`.
    #[serde(default)]
    pub start_date: String,
    /// Weekdays for `repeat: days`, 0 = Sunday.
    #[serde(default)]
    pub days: Vec<u8>,
    /// Stop at `stop_time`.
    #[serde(default)]
    pub stop_enabled: bool,
    /// `HH:MM`.
    #[serde(default = "default_stop_time")]
    pub stop_time: String,
}

fn default_start_time() -> String {
    "02:00".into()
}

fn default_stop_time() -> String {
    "06:00".into()
}

impl Default for QueueSchedule {
    fn default() -> Self {
        Self {
            start_enabled: false,
            start_time: default_start_time(),
            repeat: Repeat::Days,
            start_date: String::new(),
            days: Vec::new(),
            stop_enabled: false,
            stop_time: default_stop_time(),
        }
    }
}

/// What happens when a queue has no work left. The daemon performs `stop_limiter` itself; the application that
/// started the daemon performs the others when it sees `finish_action_pending`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnFinish {
    /// Nothing.
    #[default]
    Nothing,
    /// Tell the person.
    Notify,
    /// Turn the speed limiter off.
    StopLimiter,
    /// Close the application.
    CloseApp,
    /// Put the computer to sleep.
    Sleep,
}

fn default_max_concurrent() -> u32 {
    3
}

fn default_retry_delay_s() -> u32 {
    30
}

/// A queue's settings, as stored and as sent to `queues.put`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueueDef {
    /// `[a-z0-9_-]{1,32}`.
    pub id: String,
    /// Display name, 1 to 80 characters.
    pub name: String,
    /// What the queue does.
    #[serde(default)]
    pub kind: QueueKind,
    /// Downloads released at once, 1 to 16.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    /// Extra automatic retries of a failed download when its error is retryable, 0 to 50.
    #[serde(default)]
    pub retries: u32,
    /// Seconds to wait before each of those retries, 1 to 3600.
    #[serde(default = "default_retry_delay_s")]
    pub retry_delay_s: u32,
    /// Start and stop times.
    #[serde(default)]
    pub schedule: QueueSchedule,
    /// What happens when the queue has no work left.
    #[serde(default)]
    pub on_finish: OnFinish,
    /// KiB/s for the whole queue; `null` means no queue limit.
    #[serde(default)]
    pub limit_kib_s: Option<u32>,
}

/// A queue with its live state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueueView {
    /// The stored settings.
    #[serde(flatten)]
    pub def: QueueDef,
    /// True for the queue that cannot be deleted.
    pub builtin: bool,
    /// The queue is running: it releases downloads.
    pub running: bool,
    /// When it last started (milliseconds since the Unix epoch).
    pub last_started_ms: Option<u64>,
    /// The next scheduled start, if the schedule has one.
    pub next_start_ms: Option<u64>,
    /// The next scheduled stop of the current run, if there is one.
    pub next_stop_ms: Option<u64>,
    /// Members waiting for a turn (held by the daemon).
    pub waiting: u32,
    /// Members transferring or about to.
    pub active: u32,
    /// Member downloads that are not finished, in release order.
    pub order: Vec<JobId>,
    /// The members the daemon holds back (paused until the queue releases them).
    pub held_job_ids: Vec<JobId>,
    /// When the queue last ran out of work after running.
    pub finished_at_ms: Option<u64>,
    /// `on_finish` has not been acknowledged yet (`queues.control` `ack_finish`).
    pub finish_action_pending: bool,
}

/// The global speed limiter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LimiterDef {
    /// The limiter applies.
    #[serde(default)]
    pub enabled: bool,
    /// KiB/s for everything Zeus downloads, 1 to 10 000 000.
    #[serde(default = "default_limit_kib_s")]
    pub limit_kib_s: u32,
    /// Apply only inside the window below.
    #[serde(default)]
    pub schedule_enabled: bool,
    /// Window start, `HH:MM`.
    #[serde(default = "default_from")]
    pub from: String,
    /// Window end, `HH:MM`; earlier than `from` means past midnight.
    #[serde(default = "default_to")]
    pub to: String,
    /// Weekdays of the window, 0 = Sunday.
    #[serde(default = "all_days")]
    pub days: Vec<u8>,
}

fn default_limit_kib_s() -> u32 {
    1024
}

fn default_from() -> String {
    "08:00".into()
}

fn default_to() -> String {
    "18:00".into()
}

fn all_days() -> Vec<u8> {
    (0..=6).collect()
}

impl Default for LimiterDef {
    fn default() -> Self {
        Self {
            enabled: false,
            limit_kib_s: default_limit_kib_s(),
            schedule_enabled: false,
            from: default_from(),
            to: default_to(),
            days: all_days(),
        }
    }
}

/// The limiter with what it does right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LimiterView {
    /// The stored settings.
    #[serde(flatten)]
    pub def: LimiterDef,
    /// The limit applies at this moment (enabled, and inside the window when there is one).
    pub active_now: bool,
}

/// Which clock queue schedules and the limiter window follow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Timezone {
    /// The computer's local time.
    #[default]
    Local,
    /// Coordinated universal time.
    Utc,
}

/// A start that was missed because the daemon was not running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MissedWindow {
    /// Wait for the next start.
    #[default]
    Skip,
    /// Start once as soon as the daemon runs again.
    RunOnce,
}

/// What a scheduled stop does to downloads that are running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopBehaviour {
    /// Pause them (saved bytes stay).
    #[default]
    Pause,
    /// Let the ones already running finish and release no more.
    FinishCurrent,
}

/// Defaults that apply to every queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueueDefaults {
    /// Clock for schedules and the limiter window.
    #[serde(default)]
    pub timezone: Timezone,
    /// What to do about a start missed while the daemon was off.
    #[serde(default)]
    pub missed_window: MissedWindow,
    /// What a scheduled stop does to running downloads.
    #[serde(default)]
    pub stop_behaviour: StopBehaviour,
    /// The queue a download joins when its sender names none and queueing is wanted.
    #[serde(default = "default_queue")]
    pub default_queue: String,
}

fn default_queue() -> String {
    MAIN_QUEUE.into()
}

impl Default for QueueDefaults {
    fn default() -> Self {
        Self {
            timezone: Timezone::Local,
            missed_window: MissedWindow::Skip,
            stop_behaviour: StopBehaviour::Pause,
            default_queue: default_queue(),
        }
    }
}

/// `queues.list` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuesListParams {}

/// `queues.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuesListResult {
    /// Every queue, the built-in one first.
    pub queues: Vec<QueueView>,
    /// The global speed limiter.
    pub limiter: LimiterView,
    /// Defaults for all queues.
    pub defaults: QueueDefaults,
    /// The daemon's clock (milliseconds since the Unix epoch).
    pub now_ms: u64,
    /// Offset of the schedule clock from UTC in minutes right now (0 for UTC).
    pub utc_offset_min: i32,
    /// The kill switch is engaged: no queue releases anything until a person releases it.
    pub kill_switch_engaged: bool,
}

/// `queues.put` parameters: create a queue or replace its settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuesPutParams {
    /// The queue. The built-in queue can be changed but not renamed to another id.
    pub queue: QueueDef,
    /// Client-chosen key making the call safe to repeat (00 section 5.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `queues.put` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuesPutResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The queue as stored.
    pub queue: QueueView,
}

/// `queues.delete` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuesDeleteParams {
    /// The queue; the built-in queue cannot be deleted.
    pub id: String,
    /// Client-chosen key making the call safe to repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `queues.delete` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuesDeleteResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// Members that left the queue. Held ones stay paused; resume them with `jobs.control`.
    pub released_job_ids: Vec<JobId>,
}

/// What `queues.control` does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum QueueAction {
    /// Run the queue now (a person pressing Start).
    Start {
        /// The queue.
        id: String,
    },
    /// Stop the queue now: running members pause, or finish first per `stop_behaviour`.
    Stop {
        /// The queue.
        id: String,
    },
    /// Add jobs to the end of a queue (moving them if they belong to another one).
    Assign {
        /// The queue.
        id: String,
        /// 1 to 100 jobs.
        job_ids: Vec<JobId>,
    },
    /// Remove jobs from whatever queue holds them. Held jobs stay paused.
    Unassign {
        /// 1 to 100 jobs.
        job_ids: Vec<JobId>,
    },
    /// Move one job within its queue by `offset` places (negative is earlier).
    Move {
        /// The job.
        job_id: JobId,
        /// Places to move.
        offset: i32,
    },
    /// Acknowledge that the application performed `on_finish`.
    AckFinish {
        /// The queue.
        id: String,
    },
}

/// `queues.control` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuesControlParams {
    /// What to do.
    #[serde(flatten)]
    pub action: QueueAction,
    /// Client-chosen key making the call safe to repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `queues.control` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuesControlResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The queue the action concerned, when it concerned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<QueueView>,
    /// Jobs the action changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_job_ids: Vec<JobId>,
}

/// `queues.configure` parameters: the speed limiter and the defaults. Absent parts stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuesConfigureParams {
    /// The global speed limiter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limiter: Option<LimiterDef>,
    /// Defaults for all queues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<QueueDefaults>,
    /// Client-chosen key making the call safe to repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `queues.configure` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueuesConfigureResult {
    /// True when this response replays a durable mutation receipt.
    #[serde(default)]
    pub replayed: bool,
    /// The limiter as stored.
    pub limiter: LimiterView,
    /// The defaults as stored.
    pub defaults: QueueDefaults,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_queue_gets_the_documented_defaults() {
        let def: QueueDef =
            serde_json::from_value(serde_json::json!({"id": "night", "name": "Night"})).unwrap();
        assert_eq!(def.max_concurrent, 3);
        assert_eq!(def.retry_delay_s, 30);
        assert_eq!(def.kind, QueueKind::Download);
        assert_eq!(def.limit_kib_s, None);
        assert!(!def.schedule.start_enabled);
        assert_eq!(def.on_finish, OnFinish::Nothing);
    }

    #[test]
    fn control_actions_are_internally_tagged_beside_the_retry_key() {
        let params: QueuesControlParams = serde_json::from_value(serde_json::json!({
            "action": "start", "id": "main", "idempotency_key": "k1"
        }))
        .unwrap();
        assert_eq!(params.action, QueueAction::Start { id: "main".into() });
        assert_eq!(params.idempotency_key.as_deref(), Some("k1"));
        let back = serde_json::to_value(&params).unwrap();
        assert_eq!(back["action"], "start");
        assert_eq!(back["id"], "main");
        let unassign: QueuesControlParams = serde_json::from_value(serde_json::json!({
            "action": "unassign", "job_ids": []
        }))
        .unwrap();
        assert!(matches!(unassign.action, QueueAction::Unassign { .. }));
    }

    #[test]
    fn a_queue_view_flattens_its_settings() {
        let view = QueueView {
            def: serde_json::from_value(serde_json::json!({"id": "main", "name": "Main"})).unwrap(),
            builtin: true,
            running: false,
            last_started_ms: None,
            next_start_ms: None,
            next_stop_ms: None,
            waiting: 0,
            active: 0,
            order: Vec::new(),
            held_job_ids: Vec::new(),
            finished_at_ms: None,
            finish_action_pending: false,
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["id"], "main");
        assert_eq!(json["limit_kib_s"], serde_json::Value::Null);
        assert_eq!(json["schedule"]["start_time"], "02:00");
        let back: QueueView = serde_json::from_value(json).unwrap();
        assert_eq!(back, view);
    }
}
