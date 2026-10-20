//! TUI state and its pure reducer.

use std::collections::VecDeque;

use zeus_api::methods::jobs::ControlAction;
use zeus_api::methods::sessions::ApprovalView;
use zeus_types::{JobId, JobState, JobSummary};

/// Samples kept for the throughput sparkline (one per second).
pub const SPARKLINE_SAMPLES: usize = 60;

/// Which pane has the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The jobs table.
    Jobs,
    /// The approvals list (a trust surface).
    Approvals,
}

/// A key, already decoded from the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Unconditional quit (Ctrl-C), including during search.
    Quit,
    /// Delete the previous search character.
    Backspace,
    /// Up arrow or `k`.
    Up,
    /// Down arrow or `j`.
    Down,
    /// Tab.
    Tab,
    /// Enter.
    Enter,
    /// Escape.
    Esc,
    /// A character.
    Char(char),
}

/// Something that happened.
#[derive(Debug, Clone)]
pub enum Action {
    /// A key press.
    Key(Key),
    /// One second elapsed (drives the sparkline; ignored in no-animation mode).
    Tick {
        /// Total bytes per second across jobs at this tick.
        bytes_per_second: u64,
    },
    /// A fresh job list.
    JobsUpdated(Vec<JobSummary>),
    /// A fresh approval list.
    ApprovalsUpdated(Vec<ApprovalView>),
}

/// A side effect the event loop must perform.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Leave the interface.
    Quit,
    /// Call `jobs.control`.
    Control(JobId, ControlAction),
    /// Show the review screen for an approval and, if the person chooses, decide it.
    ReviewApproval(zeus_types::ApprovalId),
}

/// The state of the interface.
#[derive(Debug, Clone)]
pub struct App {
    /// Case-insensitive filter over job names, redacted sources and states.
    pub query: String,
    /// Characters go to the search box instead of triggering job commands.
    pub searching: bool,
    /// Jobs shown.
    pub jobs: Vec<JobSummary>,
    /// Approvals waiting for a person.
    pub approvals: Vec<ApprovalView>,
    /// Focused pane.
    pub pane: Pane,
    /// Selected row in the focused pane.
    pub selected: usize,
    /// Bytes per second samples, oldest first.
    pub throughput: VecDeque<u64>,
    /// Disable animation (spinner, redraw only on change).
    pub no_animation: bool,
    /// One-line message in the footer.
    pub message: String,
}

impl App {
    /// A new, empty interface.
    pub fn new(no_animation: bool) -> Self {
        Self {
            query: String::new(),
            searching: false,
            jobs: Vec::new(),
            approvals: Vec::new(),
            pane: Pane::Jobs,
            selected: 0,
            throughput: VecDeque::with_capacity(SPARKLINE_SAMPLES),
            no_animation,
            message: String::new(),
        }
    }

    fn rows(&self) -> usize {
        match self.pane {
            Pane::Jobs => self.visible_jobs().len(),
            Pane::Approvals => self.approvals.len(),
        }
    }

    /// The selected job, when the jobs pane has the focus.
    pub fn selected_job(&self) -> Option<&JobSummary> {
        (self.pane == Pane::Jobs)
            .then(|| self.visible_jobs().get(self.selected).copied())
            .flatten()
    }

    /// Jobs matching the current filter, in daemon order. This filters a bounded snapshot; it is not a disk index.
    pub fn visible_jobs(&self) -> Vec<&JobSummary> {
        let query = self.query.to_lowercase();
        self.jobs
            .iter()
            .filter(|job| {
                query.is_empty()
                    || job
                        .filename
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
                    || job.url_redacted.to_string().to_lowercase().contains(&query)
                    || serde_json::to_value(job.state)
                        .ok()
                        .and_then(|v| v.as_str().map(|s| s.contains(&query)))
                        .unwrap_or(false)
            })
            .collect()
    }

    /// Apply an action and return the side effect to perform, if any.
    pub fn update(&mut self, action: Action) -> Option<Effect> {
        match action {
            Action::Tick { bytes_per_second } => {
                if self.throughput.len() == SPARKLINE_SAMPLES {
                    self.throughput.pop_front();
                }
                self.throughput.push_back(bytes_per_second);
                None
            }
            Action::JobsUpdated(jobs) => {
                let selected_id = self.selected_job().map(|job| job.id);
                self.jobs = jobs;
                if self.pane == Pane::Jobs {
                    // Rows may reorder on every snapshot. Never retarget a hotkey to the row that took
                    // the old index while the selected job still exists.
                    self.selected = selected_id
                        .and_then(|id| self.visible_jobs().iter().position(|job| job.id == id))
                        .unwrap_or(0);
                }
                self.clamp();
                None
            }
            Action::ApprovalsUpdated(approvals) => {
                let selected_id = (self.pane == Pane::Approvals)
                    .then(|| self.approvals.get(self.selected).map(|a| a.approval.id))
                    .flatten();
                self.approvals = approvals;
                if self.pane == Pane::Approvals {
                    self.selected = selected_id
                        .and_then(|id| self.approvals.iter().position(|a| a.approval.id == id))
                        .unwrap_or(0);
                }
                self.clamp();
                None
            }
            Action::Key(key) => self.key(key),
        }
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.rows().saturating_sub(1));
    }

    fn key(&mut self, key: Key) -> Option<Effect> {
        if key == Key::Quit {
            return Some(Effect::Quit);
        }
        if self.searching {
            match key {
                Key::Enter => self.searching = false,
                Key::Esc => {
                    self.searching = false;
                    self.query.clear();
                    self.selected = 0;
                }
                Key::Backspace => {
                    self.query.pop();
                    self.selected = 0;
                }
                Key::Char(c) if !c.is_control() && self.query.len() < 256 => {
                    self.query.push(c);
                    self.selected = 0;
                }
                _ => {}
            }
            self.clamp();
            return None;
        }
        match key {
            Key::Char('/') => {
                self.pane = Pane::Jobs;
                self.searching = true;
                self.selected = 0;
                None
            }
            Key::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.selected = 0;
                None
            }
            Key::Char('q') | Key::Esc => Some(Effect::Quit),
            Key::Tab => {
                self.pane = if self.pane == Pane::Jobs {
                    Pane::Approvals
                } else {
                    Pane::Jobs
                };
                self.selected = 0;
                None
            }
            Key::Up | Key::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            Key::Down | Key::Char('j') => {
                self.selected = (self.selected + 1).min(self.rows().saturating_sub(1));
                None
            }
            Key::Char(c @ ('p' | 'r' | 'c' | 'x')) => {
                let job = self.selected_job()?;
                let action = match (c, job.state) {
                    ('p', state) if state.can_pause() => ControlAction::Pause,
                    ('r', JobState::Paused) => ControlAction::Resume,
                    ('r', JobState::Failed) => ControlAction::Retry,
                    ('x', JobState::RetryScheduled) => ControlAction::RetryNow,
                    ('c', state) if state.can_cancel() => ControlAction::Cancel,
                    _ => {
                        self.message = "that does not apply to this job's state".into();
                        return None;
                    }
                };
                Some(Effect::Control(job.id, action))
            }
            Key::Enter | Key::Char('a') if self.pane == Pane::Approvals => self
                .approvals
                .get(self.selected)
                .map(|a| Effect::ReviewApproval(a.approval.id)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(state: &str) -> JobSummary {
        serde_json::from_value(serde_json::json!({
            "id": zeus_types::IdGenerator::new().job_id().to_string(), "state": state, "quarantined": false,
            "source_kind": "cli", "url_redacted": "https://a.example/x", "bytes_done": 1, "bytes_total": 2,
            "created_at": "2026-10-05T09:15:30.250Z", "updated_at": "2026-10-05T09:15:30.250Z"
        }))
        .unwrap()
    }

    #[test]
    fn navigation_is_clamped() {
        let mut app = App::new(false);
        app.update(Action::JobsUpdated(vec![job("downloading"), job("paused")]));
        app.update(Action::Key(Key::Down));
        app.update(Action::Key(Key::Down));
        assert_eq!(app.selected, 1);
        app.update(Action::Key(Key::Up));
        app.update(Action::Key(Key::Up));
        assert_eq!(app.selected, 0);
        app.update(Action::JobsUpdated(vec![]));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn controls_follow_the_state_machine_rules() {
        let mut app = App::new(false);
        app.update(Action::JobsUpdated(vec![job("downloading")]));
        assert!(matches!(
            app.update(Action::Key(Key::Char('p'))),
            Some(Effect::Control(_, ControlAction::Pause))
        ));
        assert!(matches!(
            app.update(Action::Key(Key::Char('c'))),
            Some(Effect::Control(_, ControlAction::Cancel))
        ));
        assert!(app.update(Action::Key(Key::Char('r'))).is_none());
        assert!(app.message.contains("does not apply"));

        let mut paused = App::new(false);
        paused.update(Action::JobsUpdated(vec![job("paused")]));
        assert!(matches!(
            paused.update(Action::Key(Key::Char('r'))),
            Some(Effect::Control(_, ControlAction::Resume))
        ));

        let mut finalizing = App::new(false);
        finalizing.update(Action::JobsUpdated(vec![job("finalizing")]));
        assert!(
            finalizing.update(Action::Key(Key::Char('c'))).is_none(),
            "finalizing cannot be canceled (01 section 6 rule 6)"
        );
    }

    #[test]
    fn quit_and_pane_switch() {
        let mut app = App::new(true);
        assert_eq!(app.update(Action::Key(Key::Char('q'))), Some(Effect::Quit));
        app.update(Action::Key(Key::Tab));
        assert_eq!(app.pane, Pane::Approvals);
        assert!(
            app.update(Action::Key(Key::Enter)).is_none(),
            "nothing to review"
        );
    }

    #[test]
    fn the_sparkline_keeps_a_bounded_window() {
        let mut app = App::new(false);
        for i in 0..(SPARKLINE_SAMPLES as u64 + 10) {
            app.update(Action::Tick {
                bytes_per_second: i,
            });
        }
        assert_eq!(app.throughput.len(), SPARKLINE_SAMPLES);
        assert_eq!(app.throughput.front().copied(), Some(10));
    }

    #[test]
    fn refreshing_preserves_the_selected_identity_after_reordering() {
        let mut app = App::new(false);
        let a = job("downloading");
        let b = job("paused");
        app.update(Action::JobsUpdated(vec![a.clone(), b.clone()]));
        app.update(Action::Key(Key::Down));
        app.update(Action::JobsUpdated(vec![b.clone(), a]));
        assert_eq!(app.selected_job().unwrap().id, b.id);
        assert_eq!(
            app.update(Action::Key(Key::Char('r'))),
            Some(Effect::Control(b.id, ControlAction::Resume))
        );
    }

    #[test]
    fn search_typing_never_executes_job_shortcuts_and_controls_use_filtered_identity() {
        let mut app = App::new(false);
        let a = job("downloading");
        let mut b = job("paused");
        b.filename = Some("PQR.csv".into());
        app.update(Action::JobsUpdated(vec![a, b.clone()]));
        app.update(Action::Key(Key::Char('/')));
        for c in "pqr".chars() {
            assert_eq!(app.update(Action::Key(Key::Char(c))), None);
        }
        assert_eq!(app.visible_jobs().len(), 1);
        app.update(Action::Key(Key::Enter));
        assert_eq!(
            app.update(Action::Key(Key::Char('r'))),
            Some(Effect::Control(b.id, ControlAction::Resume))
        );
        assert_eq!(app.update(Action::Key(Key::Esc)), None);
        assert!(app.query.is_empty());
        app.update(Action::Key(Key::Char('/')));
        assert_eq!(app.update(Action::Key(Key::Quit)), Some(Effect::Quit));
    }
}
