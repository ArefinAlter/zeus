//! Native messaging connection routing and memory-only session leases.
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use zeus_api::{ApiError, ApiResult, CallContext};
use zeus_journal::Journal;
use zeus_session::{
    correlator::RequestCorrelator,
    error::{SessionError, SessionResult},
    protocol::*,
    provider::*,
};
use zeus_types::{ConnectionId, Timestamp};
#[path = "browser_pages.rs"]
mod pages;

/// One negotiated browser profile. Pending responses cannot cross connections.
pub struct BrowserConnection {
    /// Authenticated host connection and notifier.
    pub ctx: CallContext,
    /// Browser capabilities and profile identity.
    pub hello: HelloBody,
    /// Whether the registered host attested its browser parent.
    pub trusted: bool,
    pending: RequestCorrelator<ExtensionBody>,
    pub(crate) proxy: crate::browser_proxy::ProxyStreams,
}
impl BrowserConnection {
    /// Send one protocol response or notification through the relay.
    pub fn send(&self, re: Option<&MessageId>, body: impl Into<DaemonBody>) -> ApiResult<()> {
        let message = DaemonMessage {
            v: 1,
            id: self.pending.next_id(),
            re: re.cloned(),
            body: body.into(),
        };
        self.ctx.notifier.notify("nm.frame",serde_json::json!({"frame":message.to_json().map_err(|_|ApiError::internal("cannot encode browser message"))?})).map_err(|_|ApiError::new("api.unavailable","native output queue unavailable"))
    }
    async fn request(
        &self,
        body: impl Into<DaemonBody>,
        timeout: Duration,
    ) -> SessionResult<ExtensionBody> {
        let id = self.pending.next_id();
        self.request_with_id(id, body, timeout).await
    }
    async fn request_with_id(
        &self,
        id: MessageId,
        body: impl Into<DaemonBody>,
        timeout: Duration,
    ) -> SessionResult<ExtensionBody> {
        let waiting = self.pending.register(id.clone(), timeout)?;
        let message = DaemonMessage::new(id, body);
        self.ctx.notifier.notify("nm.frame",serde_json::json!({"frame":message.to_json().map_err(|_|SessionError::Protocol("cannot encode request"))?})).map_err(|_|SessionError::ConnectionLost)?;
        waiting.wait().await
    }
}

/// Browser broker shared with the engine; no cookies are written to the journal.
pub struct BrowserBroker {
    connections: Mutex<HashMap<ConnectionId, Arc<BrowserConnection>>>,
    pub(crate) journal: Journal,
}
impl BrowserBroker {
    /// Connect the broker to the daemon's durable consent metadata and monotonic clock.
    pub fn new(journal: Journal, _clock: zeus_control::SharedClock) -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            journal,
        }
    }
    /// Register a profile only after the host has authenticated and the hello frame is validated.
    pub fn attach(
        &self,
        ctx: CallContext,
        hello: HelloBody,
        trusted: bool,
    ) -> ApiResult<Arc<BrowserConnection>> {
        if !trusted {
            return Err(ApiError::human_required("verified browser parent"));
        }
        if hello.profile_id.len() != 32
            || !hello
                .profile_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || hello.client != "zeus-extension"
        {
            return Err(ApiError::invalid_params("invalid browser hello"));
        }
        let attach = ctx
            .attach
            .as_ref()
            .ok_or_else(|| ApiError::human_required("native host attachment"))?;
        if attach.attested_extension != hello.extension_id || attach.browser != hello.browser.name {
            return Err(ApiError::new(
                "api.permission_denied",
                "browser identity differs from host attestation",
            ));
        }
        let mut connections = self
            .connections
            .lock()
            .map_err(|_| ApiError::internal("browser registry unavailable"))?;
        if connections.len() >= 8 || connections.contains_key(&ctx.connection_id) {
            return Err(ApiError::new(
                "api.rate_limited",
                "browser connection limit reached",
            ));
        }
        // A reconnect replaces only the same authenticated browser profile and cancels its pending leases.
        connections.retain(|_, old| {
            let replace = old.hello.profile_id == hello.profile_id
                && old.hello.extension_id == hello.extension_id;
            if replace {
                old.pending.fail_all(|| SessionError::ConnectionLost);
                old.proxy.disconnect();
            }
            !replace
        });
        let connection = Arc::new(BrowserConnection {
            ctx,
            hello,
            trusted,
            pending: RequestCorrelator::new('d'),
            proxy: crate::browser_proxy::ProxyStreams::new(),
        });
        connections.insert(connection.ctx.connection_id, connection.clone());
        Ok(connection)
    }
    /// Get the negotiated connection for a frame.
    pub fn connection(&self, id: ConnectionId) -> ApiResult<Arc<BrowserConnection>> {
        self.connections
            .lock()
            .map_err(|_| ApiError::internal("browser registry unavailable"))?
            .get(&id)
            .cloned()
            .ok_or_else(|| ApiError::new("nm.handshake_required", "browser hello required"))
    }
    /// Deliver a response on its owning connection; unsolicited and late answers are discarded.
    pub fn response(&self, id: ConnectionId, re: &MessageId, body: ExtensionBody) -> ApiResult<()> {
        self.connection(id)?.pending.resolve(re, Ok(body));
        Ok(())
    }
    /// Release all pending material on browser disconnect.
    pub fn disconnect(&self, id: ConnectionId) {
        if let Ok(mut connections) = self.connections.lock()
            && let Some(connection) = connections.remove(&id)
        {
            connection.pending.fail_all(|| SessionError::ConnectionLost);
            connection.proxy.disconnect();
        }
    }
    pub(crate) fn select(
        &self,
        profile: Option<&str>,
        capability: &str,
    ) -> SessionResult<Arc<BrowserConnection>> {
        let connections = self
            .connections
            .lock()
            .map_err(|_| SessionError::ConnectionLost)?;
        let mut matches = connections.values().filter(|c| {
            c.trusted
                && !c.ctx.notifier.is_closed()
                && profile.is_none_or(|p| p == c.hello.profile_id)
                && c.hello.capabilities.iter().any(|cap| cap == capability)
        });
        let connection = matches
            .next()
            .cloned()
            .ok_or(SessionError::ExtensionUnavailable)?;
        if matches.next().is_some() {
            return Err(SessionError::Protocol(
                "choose a browser profile before releasing a session",
            ));
        }
        Ok(connection)
    }
    /// Prefer the browser profile whose human decision created this grant; never switch profiles implicitly.
    pub(crate) fn grant_profile(
        &self,
        grant_id: zeus_types::GrantId,
        requested: Option<&str>,
    ) -> SessionResult<Option<String>> {
        let recorded = self
            .journal
            .read(|tx| {
                let grant = tx
                    .grant(grant_id)?
                    .ok_or(zeus_journal::JournalError::Invariant("grant missing"))?;
                let decided_profile = tx.approval_decision(grant.approval_id)?.and_then(|r| {
                    r["evidence"]["extension"]["profile_id"]
                        .as_str()
                        .map(str::to_owned)
                });
                let reviewed_profile = tx.approval(grant.approval_id)?.and_then(|a| {
                    a.facts.and_then(|f| {
                        (f["browser_document_mode"] == "browser_origin")
                            .then(|| f["browser_profile_id"].as_str().map(str::to_owned))
                            .flatten()
                    })
                });
                if decided_profile
                    .as_ref()
                    .zip(reviewed_profile.as_ref())
                    .is_some_and(|(a, b)| a != b)
                {
                    return Err(zeus_journal::JournalError::Invariant(
                        "reviewed browser profile mismatch",
                    ));
                }
                Ok(reviewed_profile.or(decided_profile))
            })
            .map_err(|_| SessionError::Protocol("grant decision unavailable"))?;
        if recorded
            .as_deref()
            .zip(requested)
            .is_some_and(|(approved, wanted)| approved != wanted)
        {
            return Err(SessionError::Protocol(
                "grant was decided in a different browser profile",
            ));
        }
        Ok(recorded.or_else(|| requested.map(str::to_owned)))
    }
    fn authorize_refresh(&self, r: &UrlRefreshRequest) -> SessionResult<zeus_types::GrantId> {
        self.journal
            .read(|tx| {
                if tx.meta()?.kill_switch_engaged() {
                    return Ok(None);
                }
                let Some(job) = tx.job(r.job_id)? else {
                    return Ok(None);
                };
                if job.job.spec.url != r.origin_url
                    || job.job.spec.session_origin.as_ref() != Some(&r.origin)
                    || r.origin_url.origin() != r.origin
                {
                    return Ok(None);
                }
                let grants = tx.grants()?;
                Ok(
                    match zeus_types::grant::resolve_grant(
                        grants.iter(),
                        &r.origin,
                        Some(r.job_id),
                        job.job.spec.source.run_id,
                        Timestamp::now(),
                    ) {
                        zeus_types::grant::GrantResolution::Allowed(grant) => Some(grant.id),
                        _ => None,
                    },
                )
            })
            .map_err(|_| SessionError::Protocol("journal unavailable"))?
            .ok_or(SessionError::Protocol("URL refresh lacks current consent"))
    }
}
#[async_trait]
impl SessionProvider for BrowserBroker {
    async fn lease(&self, _request: LeaseRequest) -> SessionResult<SessionLease> {
        Err(SessionError::Protocol(
            "cookie export is disabled; use browser fetch",
        ))
    }
    async fn refresh_url(&self, r: UrlRefreshRequest) -> SessionResult<RefreshedUrl> {
        let grant_id = self.authorize_refresh(&r)?;
        let profile = self.grant_profile(grant_id, r.profile_id.as_deref())?;
        let connection = self.select(profile.as_deref(), capability::URL_REFRESH_REFETCH)?;
        let result = connection
            .request(
                UrlRefreshRequestBody {
                    job_id: r.job_id,
                    origin: r.origin.clone(),
                    origin_url: r.origin_url.clone(),
                    stale_url: r.stale_url.clone(),
                    reason: r.reason,
                    methods: r.methods.clone(),
                    expected: Some(RefreshExpected {
                        filename: None,
                        total_bytes: r.expected_total_bytes,
                        etag: r.expected_etag.clone(),
                    }),
                    deadline_ms: r.deadline.as_millis().clamp(1000, 60_000) as u64,
                },
                r.deadline.min(Duration::from_secs(60)),
            )
            .await?;
        if self.authorize_refresh(&r)? != grant_id {
            return Err(SessionError::Protocol("grant changed during URL refresh"));
        }
        match result {
            ExtensionBody::UrlRefreshResponse(UrlRefreshResponseBody::Refreshed {
                url,
                final_url,
                method,
                validators,
                total_bytes,
            }) => Ok(RefreshedUrl {
                url,
                final_url,
                method,
                validators,
                total_bytes,
            }),
            ExtensionBody::UrlRefreshResponse(UrlRefreshResponseBody::CannotRefresh { error }) => {
                Err(SessionError::Remote(error))
            }
            _ => Err(SessionError::Protocol("URL refresh produced no usable URL")),
        }
    }
    fn extension_connected(&self, profile: Option<&str>) -> bool {
        self.select(profile, capability::FETCH_PROXY).is_ok()
    }
}
