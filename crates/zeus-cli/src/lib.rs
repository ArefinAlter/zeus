//! The `zeus` command line and terminal UI (design: `docs/ARCHITECTURE.md` section 11, input for spec 11).
//!
//! `zeus` is a **human trust surface**: it holds the `cli` credential (kind `admin`, role `cli`) and may carry an
//! approval decision, but only from the controlling terminal ([`trust`]) and never because of `--yes`.
//!
//! | Module | What |
//! |---|---|
//! | [`cli`] | The clap command tree and global flags. |
//! | [`exit`] | Stable exit codes by error class. |
//! | [`output`] | Human, JSON and NDJSON rendering; terminal-safe text. |
//! | [`trust`] | The approval prompt on `CONIN$` or `/dev/tty`. |
//! | [`connect`] | Credential and endpoint resolution, then an authenticated [`zeus_api::IpcClient`]. |
//! | [`commands`] | One function per command, generic over [`zeus_api::Client`] so tests use a fake daemon. |
//! | [`tui`] | The ratatui interface: pure state and reducer first, rendering second. |

#![deny(unsafe_code)]

pub mod auth;
pub mod cli;
pub mod cloud;
pub mod commands;
pub mod connect;
pub mod exit;
pub mod files;
pub mod output;
mod terminal_os;
pub mod trust;
pub mod tui;
pub mod workflow;

pub use cli::{Cli, Command, GlobalArgs};
pub use exit::Exit;
pub use output::OutputMode;
