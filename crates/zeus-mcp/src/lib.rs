//! The Zeus MCP server library: the `zeus_<verb>` tools agents use to download and verify files.
//!
//! MCP is the primary agent interface (ADR 0003). This crate is a **client of the daemon API** with an `agent`
//! credential: it holds no state, enforces nothing the daemon does not also enforce, and can never approve a
//! request, create a grant or see session material. Design: `docs/ARCHITECTURE.md` section 6 (input for spec 06).
//!
//! Layout:
//!
//! | Module | What |
//! |---|---|
//! | [`args`] | Tool inputs (compact, agent-facing). |
//! | [`output`] | Tool outputs (small by construction). Strings that come from remote servers or files live in fields named `untrusted_*`. |
//! | [`catalogue`] | Fourteen implemented tools from the generated normative catalogue; conversion to `rmcp` definitions. |
//! | [`server`] | [`ZeusTools`]: dispatch of a tool call to the daemon through any [`zeus_api::Client`], with error mapping and recovery hints. |
//! | [`resources`] | `zeus://` resources and the `rescue-failed-download` prompt. |
//! | [`transport`] | `rmcp` stdio and optional authenticated loopback Streamable HTTP. |
//!
//! Tool names use only `[a-z0-9_]` and are at most 64 characters (00 section 3).

#![forbid(unsafe_code)]

pub mod args;
pub mod apps;
pub mod catalogue;
#[cfg(feature = "http")]
pub mod http_bridge;
pub mod cloud;
pub mod output;
pub mod inbox;
pub mod resources;
pub mod server;
pub mod transport;
pub mod workspace;

pub use catalogue::{ToolDef, catalogue};
pub use server::{ToolOutcome, ZeusTools};
