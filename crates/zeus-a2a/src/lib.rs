//! Zeus and the Agent2Agent (A2A) protocol: the Agent Card, the job to task mapping and peer trust levels.
//!
//! The A2A endpoint is **phase 2**, **off by default**, and runs as a bridge process that is an ordinary `agent`
//! client of the daemon (ADR 0010); this crate holds the types and the pure mapping the bridge and the daemon share.
//! Wire details follow the A2A specification 1.0.0 as read on 2026-10-05 (task states `TASK_STATE_*`, Agent Card
//! fields `supportedInterfaces`, `skills`, ...). The JSON member spelling (camelCase, per the protobuf JSON
//! mapping) and the well-known card path are to be confirmed when spec 10 is written; both are isolated here.
//!
//! Design: `docs/ARCHITECTURE.md` section 10.

#![forbid(unsafe_code)]

pub mod card;
pub mod peer;
pub mod task;

pub use card::{
    AgentCapabilities, AgentCard, AgentInterface, AgentProvider, AgentSkill, SecurityScheme,
    WELL_KNOWN_PATH, zeus_agent_card,
};
pub use peer::{PeerPermissions, PeerTrust};
pub use task::{
    Artifact, Part, Task, TaskState, TaskStatus, artifacts_for, task_state_for, task_status_for,
};
