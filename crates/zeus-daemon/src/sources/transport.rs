use super::*;
use zeus_engine::JobTransportFactory;
use zeus_http::HttpTransport;
use zeus_types::{ByteRange, ErrorEnvelope, JobSpec};
fn envelope() -> ErrorEnvelope {
    ApiError::new("sources.denied", "immutable source binding unavailable")
        .envelope()
        .clone()
}
fn private(spec: &JobSpec) -> bool {
    spec.url.url().host_str() == Some("sources.zeus.invalid")
}
pub(super) fn virtual_url(operation: &str) -> ApiResult<url::Url> {
    url::Url::parse(&format!("https://sources.zeus.invalid/{operation}")).map_err(|_| invalid())
}
struct Authority {
    service: Arc<SourceService>,
    operation: String,
}
impl SourceAuthority for Authority {
    fn remaining(&self) -> SourceResult<Duration> {
        let row = self
            .service
            .store
            .get::<Operation>(&format!("operation:{}", self.operation))
            .map_err(|_| SourceError::Storage)?
            .ok_or(SourceError::Denied)?;
        self.service
            .operation_scope(&row)
            .map_err(|_| SourceError::Denied)?;
        if row.view.state != "running" {
            return Err(SourceError::Denied);
        }
        row.deadline
            .duration_since(self.service.now())
            .filter(|d| !d.is_zero())
            .ok_or(SourceError::Budget)
    }
    fn charge(&self, bytes: u64) -> SourceResult<()> {
        self.remaining()?;
        self.service
            .store
            .mutate::<Operation, _>(&format!("operation:{}", self.operation), |row| {
                if row.view.state != "running" || row.deadline <= self.service.now() {
                    return Err(denied());
                }
                let next = row
                    .view
                    .received_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| source_error(SourceError::Budget))?;
                if next > row.view.budget.max_received_bytes {
                    return Err(source_error(SourceError::Budget));
                }
                row.view.received_bytes = next;
                Ok(())
            })
            .map_err(|_| SourceError::Budget)
    }
}
pub(super) fn authority(service: &Arc<SourceService>, operation: &str) -> Arc<dyn SourceAuthority> {
    Arc::new(Authority {
        service: service.clone(),
        operation: operation.into(),
    })
}
pub(super) struct InspectAuthority {
    pub service: Arc<SourceService>,
    pub owner: String,
    pub account: String,
    pub revision: u64,
    pub deadline: Instant,
}
impl SourceAuthority for InspectAuthority {
    fn remaining(&self) -> SourceResult<Duration> {
        self.service
            .account(&self.owner, &self.account, self.revision)
            .map_err(|_| SourceError::Denied)?;
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(SourceError::Budget)
    }
    fn charge(&self, bytes: u64) -> SourceResult<()> {
        self.remaining()?;
        if bytes == 0 {
            Ok(())
        } else {
            Err(SourceError::Budget)
        }
    }
}
struct Factory {
    service: std::sync::Weak<SourceService>,
    next: Arc<dyn JobTransportFactory>,
}
impl Factory {
    fn binding(
        &self,
        id: JobId,
        spec: &JobSpec,
    ) -> Result<(Arc<SourceService>, Operation), ErrorEnvelope> {
        let service = self.service.upgrade().ok_or_else(envelope)?;
        let op_id = spec
            .url
            .url()
            .path()
            .strip_prefix('/')
            .ok_or_else(envelope)?;
        let row = service
            .store
            .get::<Operation>(&format!("operation:{op_id}"))
            .map_err(|_| envelope())?
            .ok_or_else(envelope)?;
        if row.view.job_id != Some(id)
            || row.owner != spec.source.client_id.as_deref().unwrap_or("")
            || spec.source.kind != row.source.kind
            || spec.source.via != row.source.via
            || spec.source.run_id.is_some()
            || spec.source.browser.is_some()
            || spec.url.url() != &virtual_url(op_id).map_err(|_| envelope())?
            || spec.dest_dir.as_deref() != Some(row.dest_dir.as_str())
            || spec.filename.as_deref() != Some(row.filename.as_str())
            || spec.max_connections != Some(row.request.max_connections)
            || spec.overwrite != Some(zeus_types::Overwrite::Fail)
            || spec.expected_size != Some(row.view.metadata.bytes)
            || spec.expected_sha256.as_ref().map(ToString::to_string) != row.view.metadata.sha256
            || spec.session_origin.is_some()
            || !spec.extra_headers.is_empty()
            || spec.hint_url.is_some()
        {
            return Err(envelope());
        }
        authority(&service, op_id)
            .remaining()
            .map_err(|_| envelope())?;
        service
            .destination(&row.owner, Some(&row.dest_dir))
            .map_err(|_| envelope())?;
        Ok((service, row))
    }
}
impl JobTransportFactory for Factory {
    fn for_job(&self, spec: &JobSpec) -> Result<Option<Arc<dyn HttpTransport>>, ErrorEnvelope> {
        if private(spec) {
            Err(envelope())
        } else {
            self.next.for_job(spec)
        }
    }
    fn for_job_id(
        &self,
        id: JobId,
        spec: &JobSpec,
    ) -> Result<Option<Arc<dyn HttpTransport>>, ErrorEnvelope> {
        if !private(spec) {
            return self.next.for_job_id(id, spec);
        }
        let (service, row) = self.binding(id, spec)?;
        let account = service
            .account(&row.owner, &row.view.account_id, row.view.account_revision)
            .map_err(|_| envelope())?;
        let secret = service.secret(&account).map_err(|_| envelope())?;
        let bound = BoundSource::prepare(&account.view.config, &row.target, secret.as_ref())
            .and_then(|s| s.pin(row.view.metadata.clone()))
            .map_err(|_| envelope())?;
        Ok(Some(Arc::new(
            SourceTransport::new(
                bound,
                virtual_url(&row.view.operation_id).map_err(|_| envelope())?,
                service.network.clone(),
                authority(&service, &row.view.operation_id),
            )
            .map_err(|_| envelope())?,
        )))
    }
    fn validate_commit(
        &self,
        spec: &JobSpec,
        txn: &dyn zeus_journal::ReadTxn,
        range: ByteRange,
    ) -> Result<(), ErrorEnvelope> {
        if private(spec) {
            Err(envelope())
        } else {
            self.next.validate_commit(spec, txn, range)
        }
    }
    fn validate_job_commit(
        &self,
        id: JobId,
        spec: &JobSpec,
        txn: &dyn zeus_journal::ReadTxn,
        range: ByteRange,
    ) -> Result<(), ErrorEnvelope> {
        if private(spec) {
            self.binding(id, spec)?;
            Ok(())
        } else {
            self.next.validate_job_commit(id, spec, txn, range)
        }
    }
}
impl SourceService {
    /// Compose identity-bound sources with the unchanged workflow/ordinary transport behavior.
    pub fn transport_factory(
        self: &Arc<Self>,
        next: Arc<dyn JobTransportFactory>,
    ) -> Arc<dyn JobTransportFactory> {
        Arc::new(Factory {
            service: Arc::downgrade(self),
            next,
        })
    }
}
