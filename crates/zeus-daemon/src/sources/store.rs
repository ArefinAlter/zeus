//! Private, bounded, immediately durable source records. No credential values.
use super::*;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Serialize, de::DeserializeOwned};
const ROWS: TableDefinition<&str, &[u8]> = TableDefinition::new("source_records_v1");
const MAX_ROWS: u64 = 4096;
const LIMITS: TableDefinition<&str, u64> = TableDefinition::new("source_limits_v1");
const MAX_TOTAL: u64 = 256 * 1024 * 1024;
const MAX_RECORD: usize = 2 * 1024 * 1024;
pub(super) struct Store(Database);
impl Store {
    pub fn open(path: &Path) -> ApiResult<Self> {
        if path.is_symlink() {
            return Err(storage());
        }
        let db = Database::create(path).map_err(|_| storage())?;
        let tx = db.begin_write().map_err(|_| storage())?;
        tx.open_table(ROWS).map_err(|_| storage())?;
        tx.open_table(LIMITS).map_err(|_| storage())?;
        tx.commit().map_err(|_| storage())?;
        Ok(Self(db))
    }
    pub fn remove(&self, key: &str) -> ApiResult<()> {
        let tx = self.0.begin_write().map_err(|_| storage())?;
        {
            let mut table = tx.open_table(ROWS).map_err(|_| storage())?;
            let freed = table
                .remove(key)
                .map_err(|_| storage())?
                .map_or(0, |v| v.value().len() as u64);
            let mut limits = tx.open_table(LIMITS).map_err(|_| storage())?;
            let used = limits
                .get("bytes")
                .map_err(|_| storage())?
                .map_or(0, |v| v.value());
            limits
                .insert("bytes", used.saturating_sub(freed))
                .map_err(|_| storage())?;
        }
        tx.commit().map_err(|_| storage())
    }
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> ApiResult<Option<T>> {
        let tx = self.0.begin_read().map_err(|_| storage())?;
        let table = tx.open_table(ROWS).map_err(|_| storage())?;
        table
            .get(key)
            .map_err(|_| storage())?
            .map(|v| serde_json::from_slice(v.value()).map_err(|_| storage()))
            .transpose()
    }
    pub fn list<T: DeserializeOwned>(&self, prefix: &str) -> ApiResult<Vec<T>> {
        let tx = self.0.begin_read().map_err(|_| storage())?;
        let table = tx.open_table(ROWS).map_err(|_| storage())?;
        let mut rows = Vec::new();
        for row in table.iter().map_err(|_| storage())? {
            let (key, value) = row.map_err(|_| storage())?;
            if key.value().starts_with(prefix) {
                rows.push(serde_json::from_slice(value.value()).map_err(|_| storage())?);
            }
        }
        Ok(rows)
    }
    pub fn save<T: Serialize>(&self, key: &str, value: &T) -> ApiResult<()> {
        self.save_many(&[(
            key.to_owned(),
            serde_json::to_vec(value).map_err(|_| storage())?,
        )])
    }
    pub fn save_many(&self, rows: &[(String, Vec<u8>)]) -> ApiResult<()> {
        let mut tx = self.0.begin_write().map_err(|_| storage())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| storage())?;
        {
            let mut table = tx.open_table(ROWS).map_err(|_| storage())?;
            let mut limits = tx.open_table(LIMITS).map_err(|_| storage())?;
            for (key, value) in rows {
                let old = table
                    .get(key.as_str())
                    .map_err(|_| storage())?
                    .map_or(0, |v| v.value().len() as u64);
                let total = limits
                    .get("bytes")
                    .map_err(|_| storage())?
                    .map_or(0, |v| v.value())
                    .saturating_sub(old)
                    .saturating_add(value.len() as u64);
                if total > MAX_TOTAL || (old == 0 && total > MAX_TOTAL - 32 * 1024 * 1024) {
                    return Err(storage());
                }
                if key.len() > 512
                    || value.len() > MAX_RECORD
                    || (table.get(key.as_str()).map_err(|_| storage())?.is_none()
                        && table.len().map_err(|_| storage())? >= MAX_ROWS)
                {
                    return Err(ApiError::new(
                        "sources.capacity",
                        "source history is full; existing authority is retained",
                    ));
                }
                table
                    .insert(key.as_str(), value.as_slice())
                    .map_err(|_| storage())?;
                limits.insert("bytes", total).map_err(|_| storage())?;
            }
        }
        tx.commit().map_err(|_| storage())
    }
    pub fn mutate<T: Serialize + DeserializeOwned, R>(
        &self,
        key: &str,
        change: impl FnOnce(&mut T) -> ApiResult<R>,
    ) -> ApiResult<R> {
        let mut tx = self.0.begin_write().map_err(|_| storage())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| storage())?;
        let result = {
            let mut table = tx.open_table(ROWS).map_err(|_| storage())?;
            let mut value: T = {
                let raw = table.get(key).map_err(|_| storage())?.ok_or_else(missing)?;
                serde_json::from_slice(raw.value()).map_err(|_| storage())?
            };
            let result = change(&mut value)?;
            let bytes = serde_json::to_vec(&value).map_err(|_| storage())?;
            if bytes.len() > MAX_RECORD {
                return Err(storage());
            }
            let previous = table
                .get(key)
                .map_err(|_| storage())?
                .map_or(0, |v| v.value().len() as u64);
            let mut limits = tx.open_table(LIMITS).map_err(|_| storage())?;
            let total = limits
                .get("bytes")
                .map_err(|_| storage())?
                .map_or(0, |v| v.value())
                .saturating_sub(previous)
                .saturating_add(bytes.len() as u64);
            if total > MAX_TOTAL {
                return Err(storage());
            }
            table.insert(key, bytes.as_slice()).map_err(|_| storage())?;
            limits.insert("bytes", total).map_err(|_| storage())?;
            result
        };
        tx.commit().map_err(|_| storage())?;
        Ok(result)
    }
}
