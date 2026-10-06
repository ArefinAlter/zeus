//! `events.*`: subscriptions to the event catalogue (01 section 9).
//!
//! Delivery follows 01 section 9.2: durable events are totally ordered by `seq` and replayable from `since_seq`;
//! `job.progress` is ephemeral and coalesced. A subscriber that cannot keep up receives `events.lagged` and
//! continues from `resume_from_seq`; the daemon never blocks the engine on a subscriber.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::{Event, JobId, RunId};

/// `events.subscribe` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeParams {
    /// Replay durable events with a larger `seq`, then go live. Absent: live only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
    /// Only these event names (for example `job.completed`). Absent: all events the credential may see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
    /// Only events of these jobs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub job_ids: Vec<JobId>,
    /// Only events of this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Deliver the ephemeral `job.progress` events.
    #[serde(default)]
    pub include_progress: bool,
    /// Progress interval, at least 1000 ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_interval_ms: Option<u64>,
}

/// `events.subscribe` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeResult {
    /// Handle used in notifications and `events.unsubscribe`.
    pub subscription_id: String,
    /// Newest durable sequence at subscription time.
    pub head_seq: u64,
    /// Events that will be replayed before live delivery.
    pub replay_count: u64,
    /// True when `since_seq` is older than retention; re-read state with `jobs.list`.
    pub replay_truncated: bool,
}

/// `events.unsubscribe` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnsubscribeParams {
    /// The subscription.
    pub subscription_id: String,
}

/// `events.unsubscribe` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnsubscribeResult {
    /// False when the subscription did not exist (the call is idempotent).
    pub removed: bool,
}

/// `params` of the `events.event` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventNotification {
    /// The subscription.
    pub subscription_id: String,
    /// The event (already redacted at its source).
    pub event: Event,
}

/// `params` of the `events.lagged` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LaggedNotification {
    /// The subscription.
    pub subscription_id: String,
    /// Events dropped for this subscriber.
    pub dropped: u64,
    /// Subscribe again with this `since_seq` to continue without gaps (durable events only).
    pub resume_from_seq: u64,
}
