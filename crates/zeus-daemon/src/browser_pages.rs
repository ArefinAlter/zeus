//! Reviewed document reads; browser cookies and credential stores never cross this port.
use super::*;
use zeus_types::{GrantId, GrantScope, Origin};

struct CancelPage {
    connection: Arc<BrowserConnection>,
    request_id: MessageId,
}

#[cfg(test)]
#[path = "browser_pages_tests.rs"]
mod tests;
impl Drop for CancelPage {
    fn drop(&mut self) {
        let _ = self.connection.send(
            None,
            SessionPageCancelBody {
                request_id: self.request_id.clone(),
            },
        );
    }
}

impl BrowserBroker {
    /// Bind one attested profile to an effective grant; ambiguous connected profiles fail closed.
    pub(crate) fn document_grant(
        &self,
        origin: &Origin,
        subject: &SessionPageSubject,
        requested_profile: Option<&str>,
    ) -> SessionResult<(GrantId, String)> {
        let grants = self
            .journal
            .read(|tx| tx.grants())
            .map_err(|_| SessionError::Protocol("grant store unavailable"))?;
        let now = Timestamp::now();
        if self
            .journal
            .meta()
            .map_err(|_| SessionError::ConnectionLost)?
            .kill_switch_engaged()
            || grants.iter().any(|g| {
                g.origin == *origin && g.scope == GrantScope::Never && g.is_effective_at(now)
            })
        {
            return Err(SessionError::Protocol("browser document consent denied"));
        }
        let connection = self.select(requested_profile, capability::SESSION_PAGE)?;
        let profile = connection.hello.profile_id.clone();
        let candidates = grants.iter().filter(|g| {
            g.origin == *origin
                && g.is_effective_at(now)
                && g.decided_by.is_human_on_trust_surface()
                && match subject {
                    SessionPageSubject::Plan { .. } => g.scope == GrantScope::Site,
                    SessionPageSubject::Run { run_id } => {
                        g.scope == GrantScope::Workflow && g.run_id == Some(*run_id)
                    }
                }
        });
        for grant in candidates {
            let recorded_profile = self.grant_profile(grant.id, None)?;
            let bound = if let Some(recorded) = recorded_profile {
                recorded == profile
            } else {
                self.journal
                    .read(|tx| tx.approval_decision(grant.approval_id))
                    .map_err(|_| SessionError::Protocol("grant decision unavailable"))?
                    .is_some_and(|record| {
                        record["actor"]["kind"] == "human"
                            && record["evidence"]["exe_verified"] == true
                            && matches!(
                                record["evidence"]["surface"].as_str(),
                                Some("cli_interactive" | "tui")
                            )
                    })
            };
            let run_profile_matches = match subject {
                SessionPageSubject::Plan { .. } => true,
                SessionPageSubject::Run { .. } => self
                    .journal
                    .read(|tx| tx.approval(grant.approval_id))
                    .map_err(|_| SessionError::Protocol("grant approval unavailable"))?
                    .is_some_and(|approval| {
                        approval.state == zeus_types::ApprovalState::Approved
                            && approval.facts.as_ref().is_some_and(|facts| {
                                facts.get("browser_document_mode").and_then(|v| v.as_str())
                                    == Some("browser_origin")
                                    && facts.get("browser_profile_id").and_then(|v| v.as_str())
                                        == Some(profile.as_str())
                            })
                    }),
            };
            if bound && run_profile_matches {
                return Ok((grant.id, profile));
            }
        }
        Err(SessionError::Protocol(
            "exact browser document grant required",
        ))
    }

    fn authorize_document(&self, request: &SessionPageRequestBody) -> SessionResult<()> {
        if !request.valid_bounds() || request.incognito || !request.origin.allows_session_release()
        {
            return Err(SessionError::Protocol("invalid browser document request"));
        }
        let (grant, profile) =
            self.document_grant(&request.origin, &request.subject, Some(&request.profile_id))?;
        if grant != request.grant_id || profile != request.profile_id {
            return Err(SessionError::Protocol("browser document authority changed"));
        }
        Ok(())
    }

    /// The caller additionally checks its exact reviewed plan/run and current requester authority.
    pub(crate) async fn document(
        &self,
        request: SessionPageRequestBody,
        authority: &(dyn Fn() -> bool + Sync),
    ) -> SessionResult<SessionPageDocument> {
        self.authorize_document(&request)?;
        if !authority() {
            return Err(SessionError::Protocol(
                "browser document review unavailable",
            ));
        }
        let connection = self.select(Some(&request.profile_id), capability::SESSION_PAGE)?;
        let deadline = Duration::from_millis(u64::from(request.deadline_ms));
        let request_id = connection.pending.next_id();
        let _cancel = CancelPage {
            connection: connection.clone(),
            request_id: request_id.clone(),
        };
        let response = connection.request_with_id(request_id, request.clone(), deadline);
        tokio::pin!(response);
        let response = loop {
            self.authorize_document(&request)?;
            if !authority() {
                return Err(SessionError::Protocol("browser document review revoked"));
            }
            tokio::select! {
                result = &mut response => break result?,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        };
        self.authorize_document(&request)?;
        if !authority() {
            return Err(SessionError::Protocol("browser document review revoked"));
        }
        match response {
            ExtensionBody::SessionPageResponse(SessionPageResponseBody::Fetched { document })
                if document.matches_request(&request, Timestamp::now()) =>
            {
                Ok(*document)
            }
            ExtensionBody::SessionPageResponse(SessionPageResponseBody::Unavailable { error })
            | ExtensionBody::Error(ErrorBody { error }) => Err(SessionError::Remote(error)),
            _ => Err(SessionError::Protocol("invalid browser document response")),
        }
    }
}
