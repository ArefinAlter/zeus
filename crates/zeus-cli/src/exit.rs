//! Stable exit codes (ARCHITECTURE section 11).
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | success |
//! | 1 | internal error |
//! | 2 | usage error (bad arguments) |
//! | 3 | the daemon is unavailable or not ready |
//! | 4 | not authenticated, or a human is required |
//! | 5 | not found |
//! | 6 | conflict (illegal state transition, idempotency clash) |
//! | 7 | timeout (the job may still be running) |
//! | 8 | unsupported (version or capability) |
//! | 10 to 18 | a job ended `failed`: transient_network, throttled, auth_expired, url_expired, blocked, permanent, disk, integrity, policy_denied |
//! | 20 | completed but quarantined |
//! | 130 | interrupted (Ctrl-C) |

use zeus_types::ErrorClass;

/// A process exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Success.
    Success,
    /// Internal error.
    Internal,
    /// Usage error.
    Usage,
    /// Daemon unavailable.
    Unavailable,
    /// Unauthenticated or human required.
    Denied,
    /// Not found.
    NotFound,
    /// Conflict.
    Conflict,
    /// Timeout.
    Timeout,
    /// Unsupported.
    Unsupported,
    /// A job failed with the given class (see [`Exit::for_class`]).
    Failed(ErrorClass),
    /// Completed but quarantined.
    Quarantined,
    /// Interrupted.
    Interrupted,
}

impl Exit {
    /// The numeric status.
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Internal => 1,
            Self::Usage => 2,
            Self::Unavailable => 3,
            Self::Denied => 4,
            Self::NotFound => 5,
            Self::Conflict => 6,
            Self::Timeout => 7,
            Self::Unsupported => 8,
            Self::Failed(class) => Self::class_code(class),
            Self::Quarantined => 20,
            Self::Interrupted => 130,
        }
    }

    const fn class_code(class: ErrorClass) -> u8 {
        match class {
            ErrorClass::TransientNetwork => 10,
            ErrorClass::Throttled => 11,
            ErrorClass::AuthExpired => 12,
            ErrorClass::UrlExpired => 13,
            ErrorClass::Blocked => 14,
            ErrorClass::Permanent => 15,
            ErrorClass::Disk => 16,
            ErrorClass::Integrity => 17,
            ErrorClass::PolicyDenied => 18,
            ErrorClass::InvalidRequest => 2,
            ErrorClass::NotFound => 5,
            ErrorClass::Conflict => 6,
            ErrorClass::Unauthenticated => 4,
            ErrorClass::Unsupported => 8,
            ErrorClass::Unavailable => 3,
            ErrorClass::Timeout => 7,
            ErrorClass::Internal => 1,
        }
    }

    /// The exit status for an error of `class`.
    pub const fn for_class(class: ErrorClass) -> Self {
        match class {
            ErrorClass::InvalidRequest => Self::Usage,
            ErrorClass::NotFound => Self::NotFound,
            ErrorClass::Conflict => Self::Conflict,
            ErrorClass::Unauthenticated => Self::Denied,
            ErrorClass::Unsupported => Self::Unsupported,
            ErrorClass::Unavailable => Self::Unavailable,
            ErrorClass::Timeout => Self::Timeout,
            ErrorClass::Internal => Self::Internal,
            other => Self::Failed(other),
        }
    }
}

impl From<Exit> for std::process::ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_class_maps_to_a_documented_code() {
        let mut job_codes = BTreeSet::new();
        for class in ErrorClass::ALL {
            let code = Exit::for_class(class).code();
            assert_eq!(code, Exit::class_code(class), "{class:?}");
            assert!(
                matches!(code, 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 10..=18),
                "{class:?} -> {code}"
            );
            if (10..=18).contains(&code) {
                assert!(
                    job_codes.insert(code),
                    "job classes must have distinct codes"
                );
            }
        }
        assert_eq!(job_codes.len(), 9, "nine job classes, nine codes");
    }

    #[test]
    fn the_table_in_the_docs_is_what_the_code_does() {
        assert_eq!(Exit::Success.code(), 0);
        assert_eq!(Exit::for_class(ErrorClass::Unavailable).code(), 3);
        assert_eq!(Exit::for_class(ErrorClass::Unauthenticated).code(), 4);
        assert_eq!(Exit::for_class(ErrorClass::Blocked).code(), 14);
        assert_eq!(Exit::for_class(ErrorClass::PolicyDenied).code(), 18);
        assert_eq!(Exit::Quarantined.code(), 20);
        assert_eq!(Exit::Interrupted.code(), 130);
    }
}
