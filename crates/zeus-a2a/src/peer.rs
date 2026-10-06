//! Peer trust levels for A2A callers and outbound delegation.
//!
//! Every A2A caller is an `agent`-kind client of the daemon, whatever it claims; trust levels only narrow what such
//! a peer may do. They never grant approvals, sessions or file access beyond the agent permissions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How much Zeus trusts an A2A peer.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PeerTrust {
    /// A peer not seen before: read-only.
    #[default]
    Unknown,
    /// A peer the user added: may create jobs.
    Known,
    /// A peer the user also allowed Zeus to delegate to.
    Delegate,
}

/// What a trust level allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PeerPermissions {
    /// May read the card and the status of tasks it created.
    pub may_read: bool,
    /// May create tasks (jobs).
    pub may_create_tasks: bool,
    /// Zeus may send tasks to this peer (outbound delegation, opt-in only).
    pub may_delegate_to: bool,
}

impl PeerTrust {
    /// The permissions of this level.
    pub const fn permissions(self) -> PeerPermissions {
        match self {
            Self::Unknown => PeerPermissions {
                may_read: true,
                may_create_tasks: false,
                may_delegate_to: false,
            },
            Self::Known => PeerPermissions {
                may_read: true,
                may_create_tasks: true,
                may_delegate_to: false,
            },
            Self::Delegate => PeerPermissions {
                may_read: true,
                may_create_tasks: true,
                may_delegate_to: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_only_widens_monotonically() {
        let levels = [PeerTrust::Unknown, PeerTrust::Known, PeerTrust::Delegate];
        for pair in levels.windows(2) {
            let (a, b) = (pair[0].permissions(), pair[1].permissions());
            assert!(!a.may_create_tasks || b.may_create_tasks);
            assert!(!a.may_delegate_to || b.may_delegate_to);
        }
    }

    #[test]
    fn unknown_peers_are_read_only_and_never_delegated_to() {
        let p = PeerTrust::default().permissions();
        assert!(p.may_read && !p.may_create_tasks && !p.may_delegate_to);
    }
}
