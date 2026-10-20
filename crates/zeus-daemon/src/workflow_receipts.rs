//! Workflow retry receipts share the transaction that publishes a draft or control checkpoint.
use super::*;

pub(super) struct Binding {
    pub key: String,
    hash: String,
}

#[derive(Serialize, Deserialize)]
struct Receipt {
    hash: String,
    result: Option<serde_json::Value>,
}

impl Binding {
    pub fn new(
        identity: &ClientIdentity,
        method: &str,
        key: Option<&str>,
        params: &impl Serialize,
    ) -> ApiResult<Option<Self>> {
        let Some(key) = key else { return Ok(None) };
        if key.chars().any(char::is_control) {
            return Err(ApiError::invalid_params("invalid idempotency key"));
        }
        let key =
            crate::mutations::MutationStore::binding(identity.credential_id.as_str(), method, key)?;
        let mut params = serde_json::to_value(params).map_err(|_| fail())?;
        params
            .as_object_mut()
            .ok_or_else(fail)?
            .remove("idempotency_key");
        let bytes = zeus_verify::canonical::to_jcs_bytes(&params).map_err(|_| fail())?;
        Ok(Some(Self {
            key: format!("mutation:{key}"),
            hash: blake3::hash(&bytes).to_hex().to_string(),
        }))
    }

    pub fn bytes(&self, result: &impl Serialize) -> ApiResult<Vec<u8>> {
        serde_json::to_vec(&Receipt {
            hash: self.hash.clone(),
            result: Some(serde_json::to_value(result).map_err(|_| fail())?),
        })
        .map_err(|_| fail())
    }
}

impl WorkflowStore {
    pub(super) fn replay<T: DeserializeOwned>(
        &self,
        binding: Option<&Binding>,
    ) -> ApiResult<Option<T>> {
        let Some(binding) = binding else {
            return Ok(None);
        };
        let Some(receipt) = self.get::<Receipt>(&binding.key)? else {
            return Ok(None);
        };
        if receipt.hash != binding.hash {
            return Err(ApiError::new(
                "api.idempotency_conflict",
                "idempotency key was already bound to different parameters",
            ));
        }
        let result = receipt.result.ok_or_else(|| {
            ApiError::new(
                "api.conflict",
                "export completion is uncertain; inspect the run's exports before a new request",
            )
            .with_detail("reason", "export_receipt_incomplete")
        })?;
        serde_json::from_value(result).map(Some).map_err(|_| fail())
    }

    pub(super) fn put_with_receipt(
        &self,
        key: &str,
        row: &impl Serialize,
        binding: Option<&Binding>,
        result: &impl Serialize,
    ) -> ApiResult<()> {
        let data = serde_json::to_vec(row).map_err(|_| fail())?;
        let mut tx = self.db.begin_write().map_err(|_| fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| fail())?;
        {
            let mut table = tx.open_table(RECORDS).map_err(|_| fail())?;
            table.insert(key, data.as_slice()).map_err(|_| fail())?;
            if let Some(binding) = binding {
                let bytes = binding.bytes(result)?;
                table
                    .insert(binding.key.as_str(), bytes.as_slice())
                    .map_err(|_| fail())?;
            }
        }
        tx.commit().map_err(|_| fail())
    }

    /// Reserve before filesystem effects. A crash cannot silently export again under the same key.
    pub(super) fn reserve_export(&self, binding: &Binding) -> ApiResult<()> {
        self.put(
            &binding.key,
            &Receipt {
                hash: binding.hash.clone(),
                result: None,
            },
        )
    }

    pub(super) fn finish_export(
        &self,
        binding: &Binding,
        result: &WorkflowsManifestResult,
    ) -> ApiResult<()> {
        self.put(
            &binding.key,
            &Receipt {
                hash: binding.hash.clone(),
                result: Some(serde_json::to_value(result).map_err(|_| fail())?),
            },
        )
    }
}

/// Reuse the runtime's transition rules while committing the response with the checkpoint.
pub(super) struct ControlPort<'a> {
    pub service: &'a WorkflowService,
    pub binding: Option<&'a Binding>,
}

#[async_trait]
impl RuntimePort for ControlPort<'_> {
    async fn authorize(&self, run: RunId, hash: &str) -> RuntimeResult<()> {
        self.service.authorize(run, hash).await
    }
    async fn fetch(&self, request: FetchRequest) -> RuntimeResult<Document> {
        self.service.fetch(request).await
    }
    async fn job_totals(&self, run: RunId) -> RuntimeResult<JobTotals> {
        self.service.job_totals(run).await
    }
    async fn commit(
        &self,
        expected: u64,
        next: &RunSnapshot,
        jobs: &[JobIntent],
    ) -> RuntimeResult<()> {
        if matches!(
            next.report.status,
            RunStatus::Running | RunStatus::AwaitingApproval
        ) {
            self.authorize(next.report.run_id, &next.report.plan_hash)
                .await?;
        }
        let projected = if self.binding.is_some() {
            // Finish all journal reads before acquiring the workflow database write lock.
            // Its checkpoint CAS fences a concurrent control/discovery update.
            let mut row = self
                .service
                .store
                .run(next.report.run_id)
                .map_err(|_| runtime_fail())?
                .ok_or_else(runtime_fail)?;
            row.snapshot = next.clone();
            Some(
                self.service
                    .public_report(row)
                    .map_err(|_| runtime_fail())?,
            )
        } else {
            None
        };
        self.service.store.commit_with_receipt(
            expected,
            next,
            jobs,
            self.binding,
            projected.as_ref(),
        )
    }
    async fn control_jobs(&self, run: RunId, action: RunAction) -> RuntimeResult<()> {
        self.service.control_jobs(run, action).await
    }
    async fn finish(&self, plan: &Plan, snapshot: &RunSnapshot) -> RuntimeResult<()> {
        self.service.finish(plan, snapshot).await
    }
    async fn validate_environment(&self, plan: &Plan) -> RuntimeResult<()> {
        self.service.validate_environment(plan).await
    }
}
