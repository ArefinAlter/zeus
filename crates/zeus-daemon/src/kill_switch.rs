//! The kill switch latch (spec 07 section 8).
//!
//! One human action revokes every grant and pauses every job, and the switch stays engaged until a human releases
//! it. This module holds only the latch: the state that the refusals of spec 07 section 8 are checked against.
//! Revoking grants, pausing jobs and the refusal errors (`policy.kill_switch_engaged`, not yet in `zeus-types`) are
//! wired by the daemon backend. Persisting the latch in `meta` (03 section 4.1) is also still to do.

/// The kill switch latch. Starts released.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KillSwitch {
    engaged: bool,
}

impl KillSwitch {
    /// Whether the switch is engaged: grants are revoked and new approvals, jobs and sessions are refused.
    pub fn is_engaged(&self) -> bool {
        self.engaged
    }

    /// Engage the switch. Returns `true` when this call changed the state, `false` when it was already engaged.
    pub fn engage(&mut self) -> bool {
        let changed = !self.engaged;
        self.engaged = true;
        changed
    }

    /// Release the switch. Returns `true` when this call changed the state, `false` when it was already released.
    ///
    /// Release only clears the latch. It restores nothing: grants stay revoked and paused jobs stay paused.
    pub fn release(&mut self) -> bool {
        let changed = self.engaged;
        self.engaged = false;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::KillSwitch;

    #[test]
    fn the_latch_starts_released() {
        assert!(!KillSwitch::default().is_engaged());
    }

    #[test]
    fn engaging_twice_changes_the_state_once() {
        let mut switch = KillSwitch::default();
        assert!(switch.engage());
        assert!(switch.is_engaged());
        assert!(!switch.engage());
        assert!(switch.is_engaged());
    }

    #[test]
    fn release_reports_only_a_real_change() {
        let mut switch = KillSwitch::default();
        assert!(!switch.release());
        switch.engage();
        assert!(switch.release());
        assert!(!switch.is_engaged());
        assert!(!switch.release());
    }

    #[test]
    fn an_engaged_latch_can_be_released_and_engaged_again() {
        let mut switch = KillSwitch::default();
        switch.engage();
        switch.release();
        assert!(switch.engage());
        assert!(switch.is_engaged());
    }
}
