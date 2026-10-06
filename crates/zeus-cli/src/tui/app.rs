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
            Pane::Jobs => self.jobs.len(),
            Pane::Approvals => self.approvals.len(),
        }
    }

    /// The selected job, when the jobs pane has the focus.
    pub fn selected_job(&self) -> Option<&JobSummary> {
        (self.pane == Pane::Jobs)
            .then(|| self.jobs.get(self.selected))
            .flatten()
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
                self.jobs = jobs;
                self.clamp();
                None
            }
            Action::ApprovalsUpdated(approvals) => {
                self.approvals = approvals;
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
        match key {
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
}
