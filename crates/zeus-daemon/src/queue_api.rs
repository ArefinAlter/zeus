//! `queues.*` handlers (05 section 9.14): thin wrappers over [`crate::queues::QueueService`] that add visibility
//! rules, idempotency receipts and the view of the result.
use super::*;
use crate::mutations::{MutationStore, Receipt};
use serde::{Serialize, de::DeserializeOwned};

/// A bound idempotency receipt: its store key and its record, when the caller sent a key.
type Bound = Option<(String, Receipt)>;

impl RuntimeBackend {
    /// Queue settings are the person's: a service credential (an unattended program) may read queues and move its
    /// own jobs between them, nothing else.
    fn queue_settings_allowed(ctx: &CallContext) -> ApiResult<()> {
        if Self::scope(ctx).service().is_some() {
            return Err(ApiError::permission_denied("queue settings"));
        }
        Ok(())
    }

    fn bind_queue_call<P: Serialize>(
        &self,
        ctx: &CallContext,
        method: &str,
        key: Option<&str>,
        params: &P,
    ) -> ApiResult<Bound> {
        let Some(key) = key else {
            return Ok(None);
        };
        let binding = MutationStore::binding(ctx.identity.credential_id.as_str(), method, key)?;
        let receipt = self.receipts.begin(
            &binding,
            serde_json::to_value(params).map_err(|_| internal())?,
        )?;
        Ok(Some((binding, receipt)))
    }

    fn replay_queue_call<T: DeserializeOwned>(bound: &Bound) -> ApiResult<Option<T>> {
        bound
            .as_ref()
            .and_then(|(_, r)| r.result.clone())
            .map(|v| serde_json::from_value(v).map_err(|_| internal()))
            .transpose()
    }

    fn finish_queue_call<T: Serialize>(&self, bound: &mut Bound, result: &T) -> ApiResult<()> {
        if let Some((key, receipt)) = bound {
            receipt.result = Some(serde_json::to_value(result).map_err(|_| internal())?);
            self.receipts.save(key, receipt)?;
        }
        Ok(())
    }

    pub(super) async fn queues_list_impl(
        &self,
        ctx: &CallContext,
        _p: QueuesListParams,
    ) -> ApiResult<QueuesListResult> {
        let scope = Self::scope(ctx);
        let visible = |id: zeus_types::JobId| scope.job_id(id, &self.journal);
        let listing = self.queues.list(&visible).await;
        Ok(QueuesListResult {
            network: listing.network,
            queues: listing.queues,
            limiter: listing.limiter,
            defaults: listing.defaults,
            now_ms: listing.now_ms.max(0) as u64,
            utc_offset_min: listing.utc_offset_min,
            kill_switch_engaged: self.journal.meta().map_err(error)?.kill_switch_engaged(),
        })
    }

    pub(super) async fn queues_put_impl(
        &self,
        ctx: &CallContext,
        p: QueuesPutParams,
    ) -> ApiResult<QueuesPutResult> {
        Self::queue_settings_allowed(ctx)?;
        let mut bound =
            self.bind_queue_call(ctx, "queues.put", p.idempotency_key.as_deref(), &p)?;
        if let Some(mut done) = Self::replay_queue_call::<QueuesPutResult>(&bound)? {
            done.replayed = true;
            return Ok(done);
        }
        let id = p.queue.id.clone();
        self.queues.put(p.queue).await?;
        let queue = self.queues.queue_view(&id, &|_| true).await?;
        let result = QueuesPutResult {
            replayed: false,
            queue,
        };
        self.finish_queue_call(&mut bound, &result)?;
        Ok(result)
    }

    pub(super) async fn queues_delete_impl(
        &self,
        ctx: &CallContext,
        p: QueuesDeleteParams,
    ) -> ApiResult<QueuesDeleteResult> {
        Self::queue_settings_allowed(ctx)?;
        let mut bound =
            self.bind_queue_call(ctx, "queues.delete", p.idempotency_key.as_deref(), &p)?;
        if let Some(mut done) = Self::replay_queue_call::<QueuesDeleteResult>(&bound)? {
            done.replayed = true;
            return Ok(done);
        }
        let released = self.queues.delete(&p.id).await?;
        let result = QueuesDeleteResult {
            replayed: false,
            released_job_ids: released,
        };
        self.finish_queue_call(&mut bound, &result)?;
        Ok(result)
    }

    pub(super) async fn queues_control_impl(
        &self,
        ctx: &CallContext,
        p: QueuesControlParams,
    ) -> ApiResult<QueuesControlResult> {
        let moves_jobs = matches!(
            p.action,
            QueueAction::Assign { .. } | QueueAction::Unassign { .. } | QueueAction::Move { .. }
        );
        if !moves_jobs {
            Self::queue_settings_allowed(ctx)?;
        }
        let mut bound =
            self.bind_queue_call(ctx, "queues.control", p.idempotency_key.as_deref(), &p)?;
        if let Some(mut done) = Self::replay_queue_call::<QueuesControlResult>(&bound)? {
            done.replayed = true;
            return Ok(done);
        }
        if matches!(p.action, QueueAction::Start { .. }) {
            self.admit()?;
        }
        let scope = Self::scope(ctx);
        let visible = |id: zeus_types::JobId| scope.job_id(id, &self.journal);
        let (queue, affected) = self.queues.control(p.action, &visible).await?;
        let queue = match queue {
            Some(id) => Some(self.queues.queue_view(&id, &visible).await?),
            None => None,
        };
        let result = QueuesControlResult {
            replayed: false,
            queue,
            affected_job_ids: affected,
        };
        self.finish_queue_call(&mut bound, &result)?;
        Ok(result)
    }

    pub(super) async fn queues_configure_impl(
        &self,
        ctx: &CallContext,
        p: QueuesConfigureParams,
    ) -> ApiResult<QueuesConfigureResult> {
        Self::queue_settings_allowed(ctx)?;
        let mut bound =
            self.bind_queue_call(ctx, "queues.configure", p.idempotency_key.as_deref(), &p)?;
        if let Some(mut done) = Self::replay_queue_call::<QueuesConfigureResult>(&bound)? {
            done.replayed = true;
            return Ok(done);
        }
        let (limiter, defaults) = self.queues.configure(p.limiter, p.defaults).await?;
        let result = QueuesConfigureResult {
            replayed: false,
            limiter,
            defaults,
        };
        self.finish_queue_call(&mut bound, &result)?;
        Ok(result)
    }
}
