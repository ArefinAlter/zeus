use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use zeus_api::{ApiError, ApiResult};
use zeus_types::JobId;

const ENTRIES: TableDefinition<&str, &[u8]> = TableDefinition::new("tasks_v1");
const KEYS: TableDefinition<&str, &str> = TableDefinition::new("keys_v1");
const MESSAGES: TableDefinition<&str, &str> = TableDefinition::new("messages_v1");

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub task_id: String,
    pub context_id: String,
    pub peer: String,
    pub request_hash: String,
    pub key: String,
    pub message_key: String,
    pub job_id: Option<JobId>,
    pub created_ms: u64,
}
pub(crate) struct Store {
    db: Database,
}
fn storage<E>(_: E) -> ApiError {
    ApiError::internal("A2A durable index is unavailable; no new work was submitted")
}

pub(crate) fn digest(value: &serde_json::Value) -> ApiResult<String> {
    let bytes = zeus_verify::canonical::to_jcs_bytes(value).map_err(storage)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

impl Store {
    pub fn open(path: &std::path::Path) -> ApiResult<Self> {
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(storage(()));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).map_err(storage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if file.metadata().map_err(storage)?.mode() & 0o077 != 0 {
                return Err(storage(()));
            }
        }
        drop(file);
        zeus_api::transport::secure_private_file(path).map_err(storage)?;
        let db = Database::create(path).map_err(storage)?;
        let write = db.begin_write().map_err(storage)?;
        {
            write.open_table(ENTRIES).map_err(storage)?;
            write.open_table(KEYS).map_err(storage)?;
            write.open_table(MESSAGES).map_err(storage)?;
        }
        write.commit().map_err(storage)?;
        Ok(Self { db })
    }

    pub fn reserve(&self, entry: Entry) -> ApiResult<Entry> {
        let write = self.db.begin_write().map_err(storage)?;
        let result;
        {
            let mut tasks = write.open_table(ENTRIES).map_err(storage)?;
            let mut keys = write.open_table(KEYS).map_err(storage)?;
            let mut messages = write.open_table(MESSAGES).map_err(storage)?;
            let existing = keys
                .get(entry.key.as_str())
                .map_err(storage)?
                .map(|v| v.value().to_string());
            if let Some(id) = existing {
                let value = tasks
                    .get(id.as_str())
                    .map_err(storage)?
                    .ok_or_else(|| storage(()))?;
                let previous: Entry = serde_json::from_slice(value.value()).map_err(storage)?;
                if previous.request_hash != entry.request_hash
                    || previous.message_key != entry.message_key
                {
                    return Err(ApiError::new(
                        "api.idempotency_conflict",
                        "The idempotency key is already bound to different work",
                    ));
                }
                result = previous;
            } else {
                if messages
                    .get(entry.message_key.as_str())
                    .map_err(storage)?
                    .is_some()
                {
                    return Err(ApiError::new(
                        "api.idempotency_conflict",
                        "The message id is already bound to another idempotency key",
                    ));
                }
                let bytes = serde_json::to_vec(&entry).map_err(storage)?;
                tasks
                    .insert(entry.task_id.as_str(), bytes.as_slice())
                    .map_err(storage)?;
                keys.insert(entry.key.as_str(), entry.task_id.as_str())
                    .map_err(storage)?;
                messages
                    .insert(entry.message_key.as_str(), entry.task_id.as_str())
                    .map_err(storage)?;
                result = entry;
            }
        }
        write.commit().map_err(storage)?;
        Ok(result)
    }

    pub fn attach(&self, entry: &mut Entry, job: JobId) -> ApiResult<()> {
        if entry.job_id.is_some_and(|old| old != job) {
            return Err(storage(()));
        }
        entry.job_id = Some(job);
        let bytes = serde_json::to_vec(entry).map_err(storage)?;
        let write = self.db.begin_write().map_err(storage)?;
        {
            write
                .open_table(ENTRIES)
                .map_err(storage)?
                .insert(entry.task_id.as_str(), bytes.as_slice())
                .map_err(storage)?;
        }
        write.commit().map_err(storage)
    }

    pub fn get(&self, peer: &str, id: &str) -> ApiResult<Entry> {
        let read = self.db.begin_read().map_err(storage)?;
        let table = read.open_table(ENTRIES).map_err(storage)?;
        let value = table.get(id).map_err(storage)?.ok_or_else(missing)?;
        let entry: Entry = serde_json::from_slice(value.value()).map_err(storage)?;
        if entry.peer != peer {
            return Err(missing());
        }
        Ok(entry)
    }

    pub fn list(&self, peer: &str) -> ApiResult<Vec<Entry>> {
        let read = self.db.begin_read().map_err(storage)?;
        let table = read.open_table(ENTRIES).map_err(storage)?;
        let mut out = Vec::new();
        for row in table.iter().map_err(storage)? {
            let (_, value) = row.map_err(storage)?;
            let entry: Entry = serde_json::from_slice(value.value()).map_err(storage)?;
            if entry.peer == peer {
                out.push(entry);
            }
            if out.len() > 10_000 {
                return Err(ApiError::new(
                    "api.unavailable",
                    "A2A task index needs archival before more tasks can be listed",
                ));
            }
        }
        Ok(out)
    }
}
pub(crate) fn missing() -> ApiError {
    ApiError::new("api.not_found", "Task not found")
}
