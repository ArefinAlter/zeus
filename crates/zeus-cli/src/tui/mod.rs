//! The terminal UI (ratatui).
//!
//! Layout (80x24 minimum): a jobs table with a segment bar per job, a throughput sparkline, an approvals pane that
//! is a **trust surface** (decisions go through [`crate::trust`], never directly from a keypress without the review
//! screen), and a key-binding footer. Render budget: 10 frames per second ([`FRAME_BUDGET_MS`]); a no-animation
//! mode disables the spinner and redraws only on change. Windows terminal caveats (ConPTY, key-release events) are
//! handled in the event loop, which is not implemented yet.
//!
//! The state and its reducer ([`app`]) are pure and unit-tested; rendering ([`view`]) is tested against ratatui's
//! `TestBackend`; the crossterm event loop ([`run`]) is the part still to build.

pub mod app;
pub mod view;

use zeus_api::{ApiError, ApiResult};

/// Redraw at most this often (10 frames per second).
pub const FRAME_BUDGET_MS: u64 = 100;

/// Run the interface until the user quits. Not implemented in the skeleton: it needs the raw-mode event loop, the
/// event subscription (`events.subscribe` with resume) and the periodic `jobs.list` refresh.
pub async fn run() -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "zeus tui event loop (crossterm raw mode, events.subscribe)",
    ))
}
