//! Private durable cloud records; bearer checkpoints never have a public projection.
use super::*;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
const LIMITS: TableDefinition<&str, u64> = TableDefinition::new("cloud_limits_v1");
const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ROWS: u64 = 4096;
pub(super) fn capacity() -> ApiError {
    ApiError::new(
        "cloud.capacity_exceeded",
        "bounded cloud state is full; existing receipts and recovery records are retained",
    )
}
fn insert_record(
    table: &mut redb::Table<'_, &str, &[u8]>,
    limits: &mut redb::Table<'_, &str, u64>,
    key: &str,
    value: &[u8],
) -> ApiResult<()> {
    if value.len() > MAX_RECORD_BYTES || key.len() > 512 {
        return Err(capacity());
    }
    let previous = table
        .get(key)
        .map_err(|_| fail())?
        .map(|v| v.value().len() as u64);
    if previous.is_none() && table.len().map_err(|_| fail())? >= MAX_ROWS {
        return Err(capacity());
    }
    let used = limits
        .get("bytes")
        .map_err(|_| fail())?
        .map_or(0, |v| v.value());
    let next = used
        .saturating_sub(previous.unwrap_or(0))
        .saturating_add(value.len() as u64);
    // Reserve 64 MiB for checkpoint growth and stopping existing operations.
    if next > MAX_TOTAL_BYTES || previous.is_none() && next > MAX_TOTAL_BYTES - 64 * 1024 * 1024 {
        return Err(capacity());
    }
    table.insert(key, value).map_err(|_| fail())?;
    limits.insert("bytes", next).map_err(|_| fail())?;
    Ok(())
}
const RECORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("cloud_records_v1");
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Account {
    pub view: CloudAccountView,
    pub config: ProviderConfig,
    pub credential_key: Option<String>,
    #[serde(default)]
    pub oauth: Option<super::oauth::OAuthAccount>,
    #[serde(default)]
    pub pending_credentials: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Plan {
    pub owner: String,
    pub view: CloudPlanView,
    pub source: CloudSource,
    pub upload: zeus_cloud::UploadRequest,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Delivery {
    pub owner: String,
    pub view: CloudDeliveryView,
    pub source: CloudSource,
    pub upload: zeus_cloud::UploadRequest,
    pub checkpoint: Option<zeus_cloud::PrivateCheckpoint>,
    pub started_at: Option<Timestamp>,
    pub stop: Option<CloudControlAction>,
    #[serde(default)]
    pub staged_sha256: Option<Hex256>,
    #[serde(default)]
    pub source_checkpoint: Option<super::staging::SourceCheckpoint>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Receipt {
    pub hash: String,
    pub result: serde_json::Value,
    /// When the receipt was written; older stores lack it until the first retention pass stamps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Timestamp>,
}
pub(super) struct Store(Database);
impl Store {
    pub fn open(path: &Path) -> ApiResult<Self> {
        if path.is_symlink() {
            return Err(fail());
        }
        let db = Database::create(path).map_err(|_| fail())?;
        let tx = db.begin_write().map_err(|_| fail())?;
        {
            let table = tx.open_table(RECORDS).map_err(|_| fail())?;
            let mut limits = tx.open_table(LIMITS).map_err(|_| fail())?;
            if limits.get("bytes").map_err(|_| fail())?.is_none() {
                let mut bytes = 0u64;
                for entry in table.iter().map_err(|_| fail())? {
                    let (_, value) = entry.map_err(|_| fail())?;
                    bytes = bytes.saturating_add(value.value().len() as u64);
                }
                limits.insert("bytes", bytes).map_err(|_| fail())?;
            }
        }
        tx.commit().map_err(|_| fail())?;
        Ok(Self(db))
    }
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> ApiResult<Option<T>> {
        let tx = self.0.begin_read().map_err(|_| fail())?;
        let table = tx.open_table(RECORDS).map_err(|_| fail())?;
        table
            .get(key)
            .map_err(|_| fail())?
            .map(|v| serde_json::from_slice(v.value()).map_err(|_| fail()))
            .transpose()
    }
    pub fn keys(&self, prefix: &str) -> ApiResult<Vec<String>> {
        let tx = self.0.begin_read().map_err(|_| fail())?;
        let table = tx.open_table(RECORDS).map_err(|_| fail())?;
        let mut keys = Vec::new();
        for entry in table.iter().map_err(|_| fail())? {
            let (k, _) = entry.map_err(|_| fail())?;
            if k.value().starts_with(prefix) {
                keys.push(k.value().to_owned());
            }
        }
        Ok(keys)
    }
    pub fn map<T: DeserializeOwned, R>(
        &self,
        prefix: &str,
        mut project: impl FnMut(T) -> Option<R>,
    ) -> ApiResult<Vec<R>> {
        let tx = self.0.begin_read().map_err(|_| fail())?;
        let table = tx.open_table(RECORDS).map_err(|_| fail())?;
        let mut rows = Vec::new();
        for entry in table.iter().map_err(|_| fail())? {
            let (k, v) = entry.map_err(|_| fail())?;
            if k.value().starts_with(prefix) {
                let value = serde_json::from_slice(v.value()).map_err(|_| fail())?;
                if let Some(row) = project(value) {
                    rows.push(row);
                }
            }
        }
        Ok(rows)
    }
    pub fn list<T: DeserializeOwned>(&self, prefix: &str) -> ApiResult<Vec<T>> {
        self.map(prefix, Some)
    }
    /// Remove a private cleanup marker while maintaining the bounded byte ledger.
    pub fn remove(&self, key: &str) -> ApiResult<()> {
        let mut tx = self.0.begin_write().map_err(|_| fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| fail())?;
        {
            let mut table = tx.open_table(RECORDS).map_err(|_| fail())?;
            let mut limits = tx.open_table(LIMITS).map_err(|_| fail())?;
            let freed = table
                .remove(key)
                .map_err(|_| fail())?
                .map_or(0, |v| v.value().len() as u64);
            let used = limits
                .get("bytes")
                .map_err(|_| fail())?
                .map_or(0, |v| v.value());
            limits
                .insert("bytes", used.saturating_sub(freed))
                .map_err(|_| fail())?;
        }
        tx.commit().map_err(|_| fail())
    }
    pub fn save<T: Serialize>(&self, key: &str, value: &T) -> ApiResult<()> {
        self.save_many(&[(
            key.to_owned(),
            serde_json::to_vec(value).map_err(|_| fail())?,
        )])
    }
    pub fn save_many(&self, rows: &[(String, Vec<u8>)]) -> ApiResult<()> {
        let mut tx = self.0.begin_write().map_err(|_| fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| fail())?;
        {
            let mut table = tx.open_table(RECORDS).map_err(|_| fail())?;
            let mut limits = tx.open_table(LIMITS).map_err(|_| fail())?;
            for (k, v) in rows {
                insert_record(&mut table, &mut limits, k.as_str(), v.as_slice())?;
            }
        }
        tx.commit().map_err(|_| fail())
    }
    pub fn control(
        &self,
        id: &str,
        key: &str,
        hash: String,
        f: impl FnOnce(&mut Delivery) -> ApiResult<()>,
    ) -> ApiResult<CloudMutationResult> {
        let row_key = format!("delivery:{id}");
        let mut tx = self.0.begin_write().map_err(|_| fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| fail())?;
        let result = {
            let mut table = tx.open_table(RECORDS).map_err(|_| fail())?;
            let mut limits = tx.open_table(LIMITS).map_err(|_| fail())?;
            let mut row: Delivery = {
                let bytes = table
                    .get(row_key.as_str())
                    .map_err(|_| fail())?
                    .ok_or_else(missing)?;
                serde_json::from_slice(bytes.value()).map_err(|_| fail())?
            };
            f(&mut row)?;
            row.view.updated_at = Timestamp::now();
            let result = CloudMutationResult {
                delivery: row.view.clone(),
                replayed: false,
            };
            let bytes = serde_json::to_vec(&row).map_err(|_| fail())?;
            insert_record(&mut table, &mut limits, row_key.as_str(), bytes.as_slice())?;
            let bytes = serde_json::to_vec(&Receipt {
                created_at: Some(Timestamp::now()),
                hash,
                result: serde_json::to_value(&result).map_err(|_| fail())?,
            })
            .map_err(|_| fail())?;
            insert_record(&mut table, &mut limits, key, bytes.as_slice())?;
            result
        };
        tx.commit().map_err(|_| fail())?;
        Ok(result)
    }
    pub fn mutate_delivery<T>(
        &self,
        id: &str,
        f: impl FnOnce(&mut Delivery) -> ApiResult<T>,
    ) -> ApiResult<T> {
        let key = format!("delivery:{id}");
        let mut tx = self.0.begin_write().map_err(|_| fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| fail())?;
        let result = {
            let mut table = tx.open_table(RECORDS).map_err(|_| fail())?;
            let mut limits = tx.open_table(LIMITS).map_err(|_| fail())?;
            let mut value: Delivery = {
                let bytes = table
                    .get(key.as_str())
                    .map_err(|_| fail())?
                    .ok_or_else(missing)?;
                serde_json::from_slice(bytes.value()).map_err(|_| fail())?
            };
            let result = f(&mut value)?;
            value.view.updated_at = Timestamp::now();
            let bytes = serde_json::to_vec(&value).map_err(|_| fail())?;
            insert_record(&mut table, &mut limits, key.as_str(), bytes.as_slice())?;
            result
        };
        tx.commit().map_err(|_| fail())?;
        Ok(result)
    }
}
/// What one retention pass removed (and how many old receipts it first stamped with a time).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Pruned {
    pub deliveries: u64,
    pub plans: u64,
    pub receipts: u64,
    pub stamped: u64,
}

impl Store {
    /// Retention (roadmap D05): finished history ages out, live evidence never does. A delivery goes only when it
    /// is completed, failed or canceled, its staging file is gone, no cleanup is pending and it has not changed
    /// for `terminal_ms`; `remote_pending`, `reconciling` and `remote_cancellation_unavailable` are never
    /// removed. A plan goes once it expired without a live delivery. An idempotency receipt goes `receipt_ms`
    /// after it was written, once the delivery it names is gone or finished. One write transaction.
    pub fn prune(
        &self,
        now: Timestamp,
        terminal_ms: u64,
        receipt_ms: u64,
        staging: &Path,
    ) -> ApiResult<Pruned> {
        let older = |at: Timestamp, ms: u64| at.saturating_add_ms(ms) <= now;
        let mut tx = self.0.begin_write().map_err(|_| fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| fail())?;
        let mut pruned = Pruned::default();
        {
            let mut table = tx.open_table(RECORDS).map_err(|_| fail())?;
            let mut limits = tx.open_table(LIMITS).map_err(|_| fail())?;
            let mut rows = Vec::new();
            for entry in table.iter().map_err(|_| fail())? {
                let (k, v) = entry.map_err(|_| fail())?;
                rows.push((k.value().to_owned(), v.value().to_vec()));
            }
            // delivery id -> (finished, removable now)
            let mut deliveries = BTreeMap::new();
            for (key, bytes) in &rows {
                if let Some(id) = key.strip_prefix("delivery:") {
                    let row: Delivery = serde_json::from_slice(bytes).map_err(|_| fail())?;
                    let finished = terminal(row.view.state);
                    let removable = finished
                        && !row.view.cleanup_pending
                        && older(row.view.updated_at, terminal_ms)
                        && !staging.join(format!("{id}.part")).exists();
                    deliveries.insert(id.to_owned(), (finished, removable));
                }
            }
            let live = |id: &str| deliveries.get(id).is_some_and(|(_, removable)| !removable);
            let mut remove = Vec::new();
            let mut stamp = Vec::new();
            for (key, bytes) in &rows {
                if let Some(id) = key.strip_prefix("delivery:") {
                    if deliveries.get(id).is_some_and(|(_, removable)| *removable) {
                        remove.push((key.clone(), bytes.len()));
                        pruned.deliveries += 1;
                    }
                } else if key.starts_with("plan:") {
                    let plan: Plan = serde_json::from_slice(bytes).map_err(|_| fail())?;
                    if plan.view.expires_at <= now && !live(&plan.view.delivery_id) {
                        remove.push((key.clone(), bytes.len()));
                        pruned.plans += 1;
                    }
                } else if key.starts_with("receipt:") {
                    let mut receipt: Receipt = serde_json::from_slice(bytes).map_err(|_| fail())?;
                    let Some(created) = receipt.created_at else {
                        receipt.created_at = Some(now);
                        stamp.push((
                            key.clone(),
                            serde_json::to_vec(&receipt).map_err(|_| fail())?,
                        ));
                        continue;
                    };
                    let names = receipt.result["delivery"]["delivery_id"]
                        .as_str()
                        .or_else(|| receipt.result["plan"]["delivery_id"].as_str());
                    let settled = names
                        .is_none_or(|id| deliveries.get(id).is_none_or(|(finished, _)| *finished));
                    if older(created, receipt_ms) && settled {
                        remove.push((key.clone(), bytes.len()));
                        pruned.receipts += 1;
                    }
                }
            }
            let mut freed = 0u64;
            for (key, size) in &remove {
                table.remove(key.as_str()).map_err(|_| fail())?;
                freed = freed.saturating_add(*size as u64);
            }
            let used = limits
                .get("bytes")
                .map_err(|_| fail())?
                .map_or(0, |v| v.value());
            limits
                .insert("bytes", used.saturating_sub(freed))
                .map_err(|_| fail())?;
            for (key, value) in &stamp {
                insert_record(&mut table, &mut limits, key, value)?;
                pruned.stamped += 1;
            }
        }
        tx.commit().map_err(|_| fail())?;
        Ok(pruned)
    }
}

pub(super) fn encoded<T: Serialize>(key: String, value: &T) -> ApiResult<(String, Vec<u8>)> {
    Ok((key, serde_json::to_vec(value).map_err(|_| fail())?))
}
