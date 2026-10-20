//! Kill-switch audit records retained while the audit log cannot be written (04 section 4.6 item 8).
//!
//! The emergency stop only removes authority, so it proceeds when its write-ahead record fails. The record is then
//! kept in `<state>/audit-deferred/` (and in memory when even that write fails) and appended, preceded by an
//! `audit.error` record naming the gap, as soon as the log accepts writes again. Each retained record carries a
//! `deferred_id`; startup replay skips ids already present in the log, so a crash between the append and the file
//! removal cannot duplicate the record.
use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use zeus_types::{Actor, Timestamp};
use zeus_verify::audit::{AuditEntry, AuditLog, AuditOutcome, AuditSink, AuditSubject, actions};

/// One retained record. Data members follow the audit rules: no secrets, untrusted text only as digests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Retained {
    v: u8,
    deferred_id: String,
    failed_at: Timestamp,
    actor: Actor,
    action: String,
    subject: AuditSubject,
    outcome: AuditOutcome,
    data: serde_json::Map<String, serde_json::Value>,
}

/// Records whose write-ahead append failed but whose act proceeded.
pub(crate) struct DeferredAudit {
    dir: PathBuf,
    memory: Mutex<Vec<Retained>>,
    /// Ids appended by this process, so a file that could not be removed is not appended again.
    appended: Mutex<BTreeSet<String>>,
    pending: AtomicBool,
}

impl DeferredAudit {
    /// Open the retention directory. Existing records stay pending until [`Self::replay`] appends them.
    pub(crate) fn open(state: &Path) -> std::io::Result<Self> {
        let dir = state.join("audit-deferred");
        std::fs::create_dir_all(&dir)?;
        zeus_api::transport::secure_private_directory(&dir)?;
        let pending = std::fs::read_dir(&dir)?.next().is_some();
        Ok(Self {
            dir,
            memory: Mutex::new(Vec::new()),
            appended: Mutex::new(BTreeSet::new()),
            pending: AtomicBool::new(pending),
        })
    }

    /// True while a retained record has not reached the log.
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    /// Keep `entry` for a later append. Never fails: when the private file cannot be written either, the record
    /// stays in memory and is still appended once the log recovers in this process.
    pub(crate) fn retain(&self, entry: &AuditEntry) {
        let record = Retained {
            v: 1,
            deferred_id: zeus_types::IdGenerator::new().next_ulid().to_string(),
            failed_at: Timestamp::now(),
            actor: entry.actor.clone(),
            action: entry.action.clone(),
            subject: entry.subject.clone(),
            outcome: entry.outcome,
            data: entry.data.clone(),
        };
        self.pending.store(true, Ordering::Release);
        if let Err(error) = self.persist(&record) {
            tracing::error!(
                kind = ?error.kind(),
                action = %record.action,
                "audit record could not be retained on disk; it is kept in memory until the log recovers"
            );
            if let Ok(mut memory) = self.memory.lock() {
                memory.push(record);
            }
        } else {
            tracing::error!(
                action = %record.action,
                "audit log unavailable; the record is retained and will be appended when the log recovers"
            );
        }
    }

    fn persist(&self, record: &Retained) -> std::io::Result<()> {
        let body = serde_json::to_vec(record).map_err(std::io::Error::other)?;
        let path = self.dir.join(format!("{}.json", record.deferred_id));
        let temp = self.dir.join(format!(".{}.tmp", record.deferred_id));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            zeus_api::transport::secure_private_file(&temp)?;
            file.write_all(&body)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, &path)?;
            zeus_fs::Filesystem::sync_dir(&zeus_fs::RealFs, &self.dir)
                .map_err(std::io::Error::other)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    fn load(&self) -> Vec<(Option<PathBuf>, Retained)> {
        let mut records = Vec::new();
        let mut names = std::fs::read_dir(&self.dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "json"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        // ULID names sort in creation order.
        names.sort();
        for path in names {
            match std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Retained>(&bytes).ok())
            {
                Some(record) if record.v == 1 => records.push((Some(path), record)),
                // A damaged retained record is never deleted silently; it stays for a person to inspect.
                _ => tracing::error!("a retained audit record is unreadable and was left in place"),
            }
        }
        if let Ok(memory) = self.memory.lock() {
            records.extend(memory.iter().cloned().map(|r| (None, r)));
        }
        records
    }

    /// Ids already present in the log, so a startup replay never appends a record twice. Only the replayed record
    /// itself (`deferred: true`) counts: its `audit.error` gap record carries the same id but may have reached the
    /// log without it.
    pub(crate) fn recorded_ids(audit: &AuditLog) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        let _ = audit.visit_records(|record| {
            if record.data.get("deferred").and_then(|v| v.as_bool()) == Some(true)
                && let Some(id) = record.data.get("deferred_id").and_then(|v| v.as_str())
            {
                ids.insert(id.to_owned());
            }
            Ok(())
        });
        ids
    }

    /// Append every retained record, oldest first, each preceded by an `audit.error` naming the gap. Stops at the
    /// first failed append and keeps the rest. Returns how many records reached the log.
    pub(crate) fn replay(&self, audit: &dyn AuditSink, already: &BTreeSet<String>) -> usize {
        if !self.is_pending() {
            return 0;
        }
        let mut appended = 0;
        for (path, record) in self.load() {
            let seen = already.contains(&record.deferred_id)
                || self
                    .appended
                    .lock()
                    .is_ok_and(|ids| ids.contains(&record.deferred_id));
            if !seen {
                let mut gap = AuditEntry::new(
                    Timestamp::now(),
                    Actor::system(),
                    actions::AUDIT_ERROR,
                    AuditOutcome::Error,
                );
                gap.data
                    .insert("reason".into(), serde_json::json!("write_ahead_deferred"));
                gap.data
                    .insert("action".into(), serde_json::json!(record.action));
                gap.data
                    .insert("deferred_id".into(), serde_json::json!(record.deferred_id));
                gap.data
                    .insert("failed_at".into(), serde_json::json!(record.failed_at));
                let mut entry = AuditEntry::new(
                    Timestamp::now(),
                    record.actor.clone(),
                    &record.action,
                    record.outcome,
                );
                entry.subject = record.subject.clone();
                entry.data = record.data.clone();
                entry
                    .data
                    .insert("deferred".into(), serde_json::json!(true));
                entry
                    .data
                    .insert("deferred_id".into(), serde_json::json!(record.deferred_id));
                entry
                    .data
                    .insert("failed_at".into(), serde_json::json!(record.failed_at));
                if audit.append(gap).is_err() || audit.append(entry).is_err() {
                    return appended;
                }
                if let Ok(mut ids) = self.appended.lock() {
                    ids.insert(record.deferred_id.clone());
                }
            }
            match path {
                Some(path) => {
                    if std::fs::remove_file(&path).is_err() {
                        // The startup replay deduplicates by id, so a leftover file cannot duplicate the record.
                        tracing::warn!("an appended retained audit record could not be removed");
                    }
                }
                None => {
                    if let Ok(mut memory) = self.memory.lock() {
                        memory.retain(|r| r.deferred_id != record.deferred_id);
                    }
                }
            }
            appended += 1;
        }
        let appended_ids = self
            .appended
            .lock()
            .map(|ids| ids.clone())
            .unwrap_or_default();
        let empty = self.memory.lock().map(|m| m.is_empty()).unwrap_or(false)
            && std::fs::read_dir(&self.dir)
                .map(|mut d| {
                    d.all(|e| {
                        e.map(|e| {
                            let path = e.path();
                            path.extension().is_none_or(|x| x != "json")
                                || path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .is_some_and(|s| appended_ids.contains(s))
                        })
                        .unwrap_or(true)
                    })
                })
                .unwrap_or(false);
        if empty {
            self.pending.store(false, Ordering::Release);
        }
        appended
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeus_types::Surface;
    use zeus_verify::audit::{AuditRecord, MemoryAuditLog};

    /// A sink whose appends fail while `broken` is set.
    struct Flaky {
        inner: MemoryAuditLog,
        broken: AtomicBool,
    }
    impl AuditSink for Flaky {
        fn append(&self, entry: AuditEntry) -> zeus_verify::VerifyResult<AuditRecord> {
            if self.broken.load(Ordering::Acquire) {
                Err(zeus_verify::VerifyError::AuditUnavailable(
                    "disk full".into(),
                ))
            } else {
                self.inner.append(entry)
            }
        }
        fn head(&self) -> zeus_verify::VerifyResult<Option<zeus_types::AuditHead>> {
            self.inner.head()
        }
    }

    fn entry() -> AuditEntry {
        let mut entry = AuditEntry::new(
            Timestamp::now(),
            Actor::human(Surface::Tui),
            "daemon.kill_switch",
            AuditOutcome::Ok,
        );
        entry
            .data
            .insert("action".into(), serde_json::json!("engage"));
        entry
    }

    fn sink() -> Flaky {
        let ids = zeus_types::IdGenerator::new();
        Flaky {
            inner: MemoryAuditLog::new(
                ids.chain_id(),
                ids.store_id(),
                "test",
                Timestamp::now(),
                None,
            ),
            broken: AtomicBool::new(true),
        }
    }

    #[test]
    fn a_retained_record_survives_reopen_and_is_appended_once_with_a_gap_record() {
        let dir = tempfile::tempdir().unwrap();
        let deferred = DeferredAudit::open(dir.path()).unwrap();
        assert!(!deferred.is_pending());
        deferred.retain(&entry());
        assert!(deferred.is_pending());
        let sink = sink();
        assert_eq!(deferred.replay(&sink, &BTreeSet::new()), 0);
        drop(deferred);

        let reopened = DeferredAudit::open(dir.path()).unwrap();
        assert!(reopened.is_pending());
        sink.broken.store(false, Ordering::Release);
        assert_eq!(reopened.replay(&sink, &BTreeSet::new()), 1);
        assert!(!reopened.is_pending());
        assert_eq!(reopened.replay(&sink, &BTreeSet::new()), 0);
        let values = sink.inner.to_values();
        let actions: Vec<_> = values.iter().filter_map(|v| v["action"].as_str()).collect();
        assert_eq!(
            actions[actions.len() - 2..],
            ["audit.error", "daemon.kill_switch"]
        );
        let last = values.last().unwrap();
        assert_eq!(last["data"]["deferred"], true);
        assert_eq!(last["data"]["action"], "engage");
        assert_eq!(last["actor"]["kind"], "human");
    }

    #[test]
    fn startup_replay_skips_ids_that_already_reached_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let deferred = DeferredAudit::open(dir.path()).unwrap();
        deferred.retain(&entry());
        let id = deferred.load()[0].1.deferred_id.clone();
        let sink = sink();
        sink.broken.store(false, Ordering::Release);
        let before = sink.inner.to_values().len();
        assert_eq!(deferred.replay(&sink, &BTreeSet::from([id])), 1);
        assert_eq!(sink.inner.to_values().len(), before);
        assert!(!deferred.is_pending());
    }

    /// A sink that accepts one append and then fails, as a disk that fills up between two appends.
    struct OneAppend<'a> {
        inner: &'a AuditLog,
        used: AtomicBool,
    }
    impl AuditSink for OneAppend<'_> {
        fn append(&self, entry: AuditEntry) -> zeus_verify::VerifyResult<AuditRecord> {
            if self.used.swap(true, Ordering::AcqRel) {
                Err(zeus_verify::VerifyError::AuditUnavailable(
                    "disk full".into(),
                ))
            } else {
                self.inner.append(entry)
            }
        }
        fn head(&self) -> zeus_verify::VerifyResult<Option<zeus_types::AuditHead>> {
            self.inner.head()
        }
    }

    #[test]
    fn a_gap_record_alone_does_not_count_as_the_record_reaching_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let deferred = DeferredAudit::open(dir.path()).unwrap();
        deferred.retain(&entry());
        let id = deferred.load()[0].1.deferred_id.clone();
        let audit = AuditLog::open(
            &dir.path().join("audit"),
            None,
            zeus_types::IdGenerator::new().store_id(),
            "0.1.0",
            Default::default(),
        )
        .unwrap();
        let filling = OneAppend {
            inner: &audit,
            used: AtomicBool::new(false),
        };
        // The gap record is appended, then the kill-switch record is not.
        assert_eq!(
            deferred.replay(&filling, &DeferredAudit::recorded_ids(&audit)),
            0
        );
        drop(deferred);

        let restarted = DeferredAudit::open(dir.path()).unwrap();
        let recorded = DeferredAudit::recorded_ids(&audit);
        assert!(!recorded.contains(&id));
        assert_eq!(restarted.replay(&audit, &recorded), 1);
        assert!(DeferredAudit::recorded_ids(&audit).contains(&id));
    }

    #[test]
    fn a_record_that_cannot_reach_disk_is_kept_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let deferred = DeferredAudit::open(dir.path()).unwrap();
        std::fs::remove_dir_all(dir.path().join("audit-deferred")).unwrap();
        deferred.retain(&entry());
        assert!(deferred.is_pending());
        let sink = sink();
        sink.broken.store(false, Ordering::Release);
        assert_eq!(deferred.replay(&sink, &BTreeSet::new()), 1);
        assert!(deferred.memory.lock().unwrap().is_empty());
    }
}
