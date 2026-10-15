//! The Zeus daemon library: everything `zeusd` does, as testable parts.
//!
//! The daemon owns all durable state (journal, grants, approvals, audit log) and exposes exactly one listener,
//! the local IPC endpoint of [`zeus_api`] (ADR 0002, ADR 0010). This crate wires four things together:
//!
//! * [`backend`]: the [`backend::Backend`] trait, one method per MVP API method, with the engine adapter as its
//!   real implementation (a later task, because `zeus-engine` is still being written). Defaults return
//!   `api.not_implemented`.
//! * [`routes`]: registers the backend on a [`zeus_api::Router`], which enforces authorisation from
//!   [`zeus_api::policy::METHODS`] before any handler runs.
//! * [`approvals`]: the security logic of human decisions (request-hash binding, scope rules, expiry, rate limit),
//!   implemented and tested here because it is the heart of ADR 0007.
//! * [`provision`] and [`config`]: first-run state (IPC secret, client credentials) and the TOML configuration.
//!
//! Design: `docs/ARCHITECTURE.md` sections 1 to 7.

#![forbid(unsafe_code)]

pub mod approvals;
pub mod backend;
pub mod config;
pub mod kill_switch;
pub mod nm;
pub mod provision;
pub mod routes;

pub use backend::{Backend, NotImplementedBackend};
pub use config::DaemonConfig;
pub use routes::build_router;
