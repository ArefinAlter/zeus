//! Durable request binding and per-item receipts for batch enqueue retries.
use redb::{Database, ReadableDatabase, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use zeus_api::{ApiError, ApiResult};
const REQUESTS: TableDefinition<&str, &[u8]> = TableDefinition::new("api_mutations_v1");

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub hash: String,
    pub entries: Vec<Value>,
    pub result: Option<Value>,
}
pub(crate) struct MutationStore(Database);
fn failure() -> ApiError {
    ApiError::internal("durable API receipt unavailable")
}
impl MutationStore {
    pub fn open(path: &Path) -> ApiResult<Self> {
        if path.is_symlink() {
            return Err(failure());
        }
        let db = Database::create(path).map_err(|_| failure())?;
        let tx = db.begin_write().map_err(|_| failure())?;
        tx.open_table(REQUESTS).map_err(|_| failure())?;
        tx.commit().map_err(|_| failure())?;
        Ok(Self(db))
    }
    pub fn binding(client: &str, method: &str, key: &str) -> ApiResult<String> {
        if key.is_empty() || key.len() > 128 {
            return Err(ApiError::invalid_params(
                "idempotency key must contain 1 to 128 bytes",
            ));
        }
        let tuple = serde_json::to_vec(&(client, method, key)).map_err(|_| failure())?;
        Ok(blake3::hash(&tuple).to_hex().to_string())
    }
    pub fn begin(&self, key: &str, mut params: Value) -> ApiResult<Receipt> {
        if let Some(object) = params.as_object_mut() {
            object.remove("idempotency_key");
        }
        let bytes = zeus_verify::canonical::to_jcs_bytes(&params).map_err(|_| failure())?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let tx = self.0.begin_read().map_err(|_| failure())?;
        let table = tx.open_table(REQUESTS).map_err(|_| failure())?;
        if let Some(value) = table.get(key).map_err(|_| failure())? {
            let receipt: Receipt = serde_json::from_slice(value.value()).map_err(|_| failure())?;
            if receipt.hash != hash {
                return Err(ApiError::new(
                    "api.idempotency_conflict",
                    "idempotency key was already bound to different parameters",
                ));
            }
            return Ok(receipt);
        }
        drop(table);
        drop(tx);
        let receipt = Receipt {
            hash,
            entries: Vec::new(),
            result: None,
        };
        self.save(key, &receipt)?;
        Ok(receipt)
    }
    /// Inspect an existing exact request without reserving a retry key or writing state.
    pub fn lookup(&self, key: &str, mut params: Value) -> ApiResult<Option<Receipt>> {
        if let Some(object) = params.as_object_mut() {
            object.remove("idempotency_key");
        }
        let bytes = zeus_verify::canonical::to_jcs_bytes(&params).map_err(|_| failure())?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let tx = self.0.begin_read().map_err(|_| failure())?;
        let table = tx.open_table(REQUESTS).map_err(|_| failure())?;
        let Some(value) = table.get(key).map_err(|_| failure())? else {
            return Ok(None);
        };
        let receipt: Receipt = serde_json::from_slice(value.value()).map_err(|_| failure())?;
        if receipt.hash != hash {
            return Err(ApiError::new(
                "api.idempotency_conflict",
                "idempotency key was already bound to different parameters",
            ));
        }
        Ok(Some(receipt))
    }
    pub fn save(&self, key: &str, receipt: &Receipt) -> ApiResult<()> {
        let bytes = serde_json::to_vec(receipt).map_err(|_| failure())?;
        let mut tx = self.0.begin_write().map_err(|_| failure())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| failure())?;
        tx.open_table(REQUESTS)
            .map_err(|_| failure())?
            .insert(key, bytes.as_slice())
            .map_err(|_| failure())?;
        tx.commit().map_err(|_| failure())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maintenance_status_lookup_never_reserves_missing_keys() {
        let dir = tempfile::tempdir().unwrap();
        let store = MutationStore::open(&dir.path().join("api.redb")).unwrap();
        assert!(
            store
                .lookup("key", serde_json::json!({"plan":"old"}))
                .unwrap()
                .is_none()
        );
        store
            .begin("key", serde_json::json!({"plan":"new"}))
            .unwrap();
        assert!(
            store
                .lookup("key", serde_json::json!({"plan":"new"}))
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .lookup("key", serde_json::json!({"plan":"old"}))
                .is_err()
        );
    }
    #[test]
    fn retries_survive_restart_and_bind_the_entire_batch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.redb");
        let key = MutationStore::binding("agent-a", "jobs.enqueue", "retry").unwrap();
        {
            let store = MutationStore::open(&path).unwrap();
            let mut receipt = store.begin(&key, serde_json::json!({"jobs":[{"url":"https://example.org/a"}],"idempotency_key":"retry"})).unwrap();
            receipt.entries.push(serde_json::json!({"job_id":"saved"}));
            store.save(&key, &receipt).unwrap();
        }
        let store = MutationStore::open(&path).unwrap();
        let receipt = store
            .begin(
                &key,
                serde_json::json!({"jobs":[{"url":"https://example.org/a"}]}),
            )
            .unwrap();
        assert_eq!(receipt.entries.len(), 1);
        assert!(
            store
                .begin(
                    &key,
                    serde_json::json!({"jobs":[{"url":"https://example.org/b"}]})
                )
                .is_err()
        );
        assert_ne!(
            key,
            MutationStore::binding("agent-b", "jobs.enqueue", "retry").unwrap()
        );
    }
}
