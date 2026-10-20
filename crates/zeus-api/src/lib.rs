//! The Zeus daemon API: the single contract between `zeusd` and every front end (CLI, TUI, MCP server,
//! native-messaging host, bridges, future GUI).
//!
//! JSON-RPC 2.0 over a **local** socket, one message per line (ADR 0002). This crate is the draft of record for
//! the planned spec 05; `docs/ARCHITECTURE.md` section 5 explains the design.
//!
//! Map of the crate:
//!
//! | Module | What |
//! |---|---|
//! | [`jsonrpc`] | Envelope types: requests, responses, notifications, error objects. |
//! | [`framing`] | Newline-delimited JSON codec with a hard line limit. |
//! | [`error`] | `api.*` error codes, [`ApiError`], mapping to the 00 error envelope and JSON-RPC codes. |
//! | [`kind`] | [`ClientKind`], [`ClientRole`] and [`Permission`]. |
//! | [`policy`] | The method table: who may call what ([`policy::METHODS`]). |
//! | [`methods`] | Typed parameters and results per method ([`methods::RpcMethod`]). |
//! | [`view`] | Redacted projections of core entities that are safe to return to agents. |
//! | [`schema`] | The draft JSON Schema of the whole API, generated from these types and checked in. |
//! | [`auth`] | Tokens, credentials and the mutual challenge-response handshake. |
//! | [`endpoint`] | Endpoint naming, discovery file, state and config paths. |
//! | [`transport`] | Local socket connect and bind (platform specific). |
//! | [`client`] | The [`Client`] trait and a typed call helper. |
//! | [`server`] | The [`Router`] (authorising dispatcher), notifier and server loop. |
//!
//! Anything in this crate that is not yet implemented returns a typed `api.not_implemented` error and is listed in
//! the repository's known gaps; the placeholder macros are forbidden.
//!
//! `unsafe` is denied crate-wide (workspace lint). The platform FFI needed for pipe security descriptors and peer
//! credentials belongs in isolated, commented modules under [`transport`] that opt in with `#[allow(unsafe_code)]`.

pub mod auth;
pub mod client;
pub mod endpoint;
pub mod error;
pub mod framing;
pub mod jsonrpc;
pub mod kind;
pub mod methods;
pub mod paging;
pub mod policy;
pub mod rate;
pub mod schema;
pub mod server;
pub mod transport;
pub mod view;

pub use client::{Client, IpcClient, call};
pub use error::{ApiError, ApiResult};
pub use jsonrpc::{ErrorObject, Message, Notification, Request, RequestId, Response};
pub use kind::{ClientIdentity, ClientKind, ClientRole, Permission};
pub use methods::{RpcMethod, names};
pub use paging::{Page, PageRequest};
pub use server::{CallContext, Notifier, Router};

/// The only protocol version this build speaks (00 section 4.2 registry entry "daemon api").
pub const PROTOCOL_VERSION: u16 = 1;

/// Optional behaviour negotiated in `daemon.hello` (00 section 4: capabilities).
pub mod capabilities {
    /// `events.subscribe`, `events.unsubscribe` and the `events.event` notification.
    pub const EVENTS: &str = "events";
    /// Durable event replay from `since_seq`.
    pub const EVENTS_REPLAY: &str = "events.replay";
    /// `jobs.wait` long polling.
    pub const JOBS_WAIT: &str = "jobs.wait";
    /// Idempotency keys on mutating methods.
    pub const IDEMPOTENCY: &str = "idempotency";
    /// The `nm.frame` relay for the native-messaging host.
    pub const NM_RELAY: &str = "nm.relay";
    /// `clients.list`, `clients.add`, `clients.revoke`.
    pub const CLIENTS: &str = "clients";
    /// `files.roots.register`.
    pub const FILES_ROOTS: &str = "files.roots";
    /// `files.organize` and `files.undo`.
    pub const FILES_ORGANIZE: &str = "files.organize";
    /// Workflow methods (later phase).
    pub const WORKFLOWS: &str = "workflows";
    /// LLM provider methods (later phase).
    pub const LLM: &str = "llm";

    /// Everything this build can offer, before intersection with the client's list.
    pub const ALL: &[&str] = &[
        EVENTS,
        EVENTS_REPLAY,
        JOBS_WAIT,
        IDEMPOTENCY,
        NM_RELAY,
        CLIENTS,
        FILES_ROOTS,
        FILES_ORGANIZE,
        WORKFLOWS,
        LLM,
    ];
}
