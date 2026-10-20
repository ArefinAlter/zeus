//! What each connection may observe (05 section 7.5).
//!
//! Human-capable connections see everything. Agents see every job and grant but only the approvals they requested.
//! Services see only their own jobs, the approvals they requested, grants those approvals created or that are bound
//! to their jobs, their roots and the audit records they caused. An entity outside the view is reported exactly as a
//! missing one (`api.not_found`), so identifiers cannot be probed.
use zeus_api::{ApiError, ClientIdentity, ClientKind};
use zeus_journal::Journal;
use zeus_types::{Approval, Event, EventPayload, Job, SessionGrant};

/// The view of one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Human and admin connections.
    All,
    /// An agent credential id.
    Agent(String),
    /// A service credential id.
    Service(String),
}

impl Scope {
    /// The scope of an authenticated identity (its effective kind).
    pub(crate) fn of(identity: &ClientIdentity) -> Self {
        match identity.kind {
            ClientKind::Human | ClientKind::Admin => Self::All,
            ClientKind::Agent => Self::Agent(identity.credential_id.to_string()),
            ClientKind::Service => Self::Service(identity.credential_id.to_string()),
        }
    }

    /// The service credential id, when the scope is a service.
    pub(crate) fn service(&self) -> Option<&str> {
        match self {
            Self::Service(id) => Some(id),
            _ => None,
        }
    }

    /// Jobs: services see the jobs whose source names their credential.
    pub(crate) fn job(&self, job: &Job) -> bool {
        match self {
            Self::Service(id) => job.spec.source.client_id.as_deref() == Some(id.as_str()),
            _ => true,
        }
    }

    /// Fail a lookup of an invisible job exactly like a missing one.
    pub(crate) fn require_job(&self, job: &Job) -> Result<(), ApiError> {
        if self.job(job) {
            Ok(())
        } else {
            Err(ApiError::new("api.not_found", "job not found"))
        }
    }

    /// Approvals: unattended clients see the ones they requested.
    pub(crate) fn approval(&self, approval: &Approval) -> bool {
        match self {
            Self::All => true,
            Self::Agent(id) | Self::Service(id) => {
                approval.requested_by.as_deref() == Some(id.as_str())
            }
        }
    }

    /// Grants: services see grants created by approvals they requested or bound to their jobs.
    pub(crate) fn grant(&self, grant: &SessionGrant, journal: &Journal) -> bool {
        match self {
            Self::Service(_) => {
                journal
                    .read(|tx| tx.approval(grant.approval_id))
                    .ok()
                    .flatten()
                    .is_some_and(|a| self.approval(&a))
                    || grant.job_id.is_some_and(|id| self.job_id(id, journal))
            }
            _ => true,
        }
    }

    /// A job id inside the view (an unknown id is outside a service view).
    pub(crate) fn job_id(&self, id: zeus_types::JobId, journal: &Journal) -> bool {
        match self {
            Self::Service(_) => journal
                .read(|tx| tx.job(id))
                .ok()
                .flatten()
                .is_some_and(|r| self.job(&r.job)),
            _ => true,
        }
    }

    /// Audit records: services see the records whose actor is their credential.
    pub(crate) fn audit(&self, actor_client_id: Option<&str>) -> bool {
        match self {
            Self::Service(id) => actor_client_id == Some(id.as_str()),
            _ => true,
        }
    }

    /// Events follow the entity they describe; `daemon.*` events are visible to every kind.
    pub(crate) fn event(&self, event: &Event, journal: &Journal) -> bool {
        if *self == Self::All {
            return true;
        }
        match &event.payload {
            EventPayload::ApprovalRequested(data) => self.approval(&data.approval),
            EventPayload::ApprovalDecided(data) => journal
                .read(|tx| tx.approval(data.approval_id))
                .ok()
                .flatten()
                .is_some_and(|a| self.approval(&a)),
            EventPayload::GrantCreated(data) => self.grant(&data.grant, journal),
            EventPayload::GrantRevoked(data) => self.grant_id(data.grant_id, journal),
            EventPayload::GrantExpired(data) => self.grant_id(data.grant_id, journal),
            EventPayload::GrantRenewed(data) => self.grant(&data.grant, journal),
            _ => match event.job_id {
                Some(id) => self.job_id(id, journal),
                // Run-level events without a job are scoped by the workflow service; daemon events are public.
                None => self.service().is_none() || event.run_id.is_none(),
            },
        }
    }

    fn grant_id(&self, id: zeus_types::GrantId, journal: &Journal) -> bool {
        match self {
            Self::Service(_) => journal
                .read(|tx| tx.grant(id))
                .ok()
                .flatten()
                .is_some_and(|g| self.grant(&g, journal)),
            _ => true,
        }
    }
}
