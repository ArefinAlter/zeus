//! Durable write-ahead receipts for native rename/move and guarded undo. No automatic filesystem
//! mutation runs on restart; incomplete receipts remain visible for a person's reconciliation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeus_app::failure::{Failure, Result};
use zeus_fs::{Filesystem, RealFs};

use crate::{
    facts::{FileFacts, quarantined_under},
    ops,
    paths::{RootCaps, cap_failure, resolve_new_inside, shown},
};

const MAX_RECORDS: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    file_id: Option<zeus_types::FileId>,
    bytes: u64,
    mtime_ms: i64,
    directory: bool,
}
fn identity(path: &Path) -> Result<Identity> {
    let meta = RealFs
        .metadata(path)
        .map_err(|_| Failure::new("app.io", "The file identity could not be read."))?
        .ok_or_else(|| Failure::new("app.not_found", "That item no longer exists."))?;
    if meta.is_symlink || (!meta.is_regular_file && !meta.is_dir) {
        return Err(Failure::invalid(
            "Choose a file or folder itself, rather than a link or special file.",
        ));
    }
    Ok(identity_from_meta(meta))
}
fn identity_from_meta(meta: zeus_fs::FileMeta) -> Identity {
    Identity {
        file_id: meta.file_id,
        bytes: meta.len,
        mtime_ms: meta.mtime_ms,
        directory: meta.is_dir,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    id: String,
    from: String,
    path: String,
    identity: Identity,
    state: String,
    created_ms: u64,
}

/// Bounded durable history; one lock serializes decisions and their filesystem operation.
pub struct Journal {
    path: PathBuf,
    records: Mutex<Vec<Record>>,
    load_failed: bool,
}
impl Journal {
    /// Load receipts without executing or guessing the result of any interrupted operation.
    pub fn new(path: PathBuf) -> Self {
        let read = match std::fs::metadata(&path) {
            Ok(m) if m.len() > 16 * 1024 * 1024 => {
                Err(std::io::Error::other("history exceeds its bound"))
            }
            _ => std::fs::read(&path),
        };
        let (records, load_failed) = match read {
            Ok(bytes) => match serde_json::from_slice::<Vec<Record>>(&bytes) {
                Ok(rows) if rows.len() <= MAX_RECORDS => (rows, false),
                _ => (Vec::new(), true),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), false),
            Err(_) => (Vec::new(), true),
        };
        Self {
            path,
            records: Mutex::new(records),
            load_failed,
        }
    }
    fn save(&self, records: &[Record]) -> Result<()> {
        if self.load_failed {
            return Err(Failure::new(
                "app.history_corrupt",
                "File-operation history could not be read. Preserve its file and repair it before making another tracked move.",
            ));
        }
        zeus_app::engine_config::write_atomic(&self.path, &serde_json::to_vec(records)?)?;
        Ok(())
    }
    /// Current receipts; pending and undoing states require review and never trigger replay.
    pub fn list(&self) -> Value {
        match self.records.lock() {
            Ok(records) => {
                json!({"records":records.iter().rev().map(|r| json!({"id":r.id,"from":r.from,"path":r.path,"state":r.state,"created_ms":r.created_ms,"can_undo":r.state=="complete","directory":r.identity.directory})).collect::<Vec<_>>(),"load_failed":self.load_failed})
            }
            Err(_) => json!({"records":[],"load_failed":true}),
        }
    }
    /// Persist an intent, move atomically without replacement, then persist its result.
    pub fn relocate(
        &self,
        from: &str,
        to: &str,
        roots: &[String],
        facts: &HashMap<String, FileFacts>,
    ) -> Result<Value> {
        self.relocate_pinned(from, to, &RootCaps::capture(roots)?, facts)
    }
    /// Move using roots pinned against the desktop service's enablement identities.
    pub fn relocate_pinned(
        &self,
        from: &str,
        to: &str,
        roots: &RootCaps,
        facts: &HashMap<String, FileFacts>,
    ) -> Result<Value> {
        let from = roots.path(from)?;
        let to = roots.path(to)?;
        if from == to || to.starts_with(&from) {
            return Err(Failure::invalid(
                "Choose a different path outside the source.",
            ));
        }
        if roots
            .paths()
            .iter()
            .any(|r| crate::config::norm(r) == crate::config::norm(&shown(&from)))
        {
            return Err(Failure::invalid(
                "An enabled root itself cannot be moved or renamed here.",
            ));
        }
        if quarantined_under(facts, &shown(&from)) || quarantined_under(facts, &shown(&to)) {
            return Err(ops::quarantine_refusal("This item"));
        }
        let (source_parent, source_name) = roots.parent(&from)?;
        let (target_parent, target_name) = roots.parent(&to)?;
        let source = source_parent
            .prepare_rename(&source_name)
            .map_err(cap_failure)?;
        let source_identity = identity_from_meta(source.metadata().map_err(cap_failure)?);
        let mut records = self
            .records
            .lock()
            .map_err(|_| Failure::new("app.io", "File-operation history is busy."))?;
        if records.len() >= MAX_RECORDS {
            if let Some(at) = records
                .iter()
                .position(|r| matches!(r.state.as_str(), "undone" | "failed"))
            {
                records.remove(at);
            } else {
                return Err(Failure::new(
                    "app.history_full",
                    "The 1,000 retained operation receipts are full. Keep them for recovery; no history was discarded.",
                ));
            }
        }
        let id = format!("fop_{}", zeus_app::review::random_id());
        records.push(Record {
            id: id.clone(),
            from: shown(&from),
            path: shown(&to),
            identity: source_identity,
            state: "pending".into(),
            created_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |x| x.as_millis() as u64),
        });
        self.save(&records)?;
        // The source and both parents remain open across durable intent and publication.
        if identity_from_meta(source.metadata().map_err(cap_failure)?)
            != records.last().expect("receipt exists").identity
        {
            return Err(Failure::invalid(
                "The source changed before the move; review its receipt.",
            ));
        }
        let result = source.rename_noreplace(&target_parent, &target_name);
        let row = records.last_mut().expect("receipt was just appended");
        row.state = if result.is_ok() { "complete" } else { "failed" }.into();
        // A save failure leaves the durable pending intent; never pretend the move did not happen.
        if self.save(&records).is_err() {
            return Err(Failure::new(
                "app.history_uncertain",
                "The operation result could not be saved. Check both paths and the pending receipt before retrying.",
            ));
        }
        result.map_err(|e| if matches!(e, zeus_fs::FsError::AlreadyExists) { Failure::new("app.exists", "That destination already exists; nothing was replaced.") } else { Failure::new("app.move_failed", "The move was refused. Moves across drives require Copy, then a separate deletion after verification.") })?;
        Ok(json!({"ok":true,"id":id,"from":shown(&from),"path":shown(&to),"can_undo":true}))
    }
    /// Undo a completed move only when its identity still matches and the old path is unoccupied.
    pub fn undo(
        &self,
        id: &str,
        roots: &[String],
        facts: &HashMap<String, FileFacts>,
    ) -> Result<Value> {
        self.undo_pinned(id, &RootCaps::capture(roots)?, facts)
    }
    /// Undo using retained root authority and an opened source identity.
    pub fn undo_pinned(
        &self,
        id: &str,
        roots: &RootCaps,
        facts: &HashMap<String, FileFacts>,
    ) -> Result<Value> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| Failure::new("app.io", "File-operation history is busy."))?;
        let at = records
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| Failure::invalid("Choose an operation from this device's history."))?;
        let r = records[at].clone();
        if r.state != "complete" {
            return Err(Failure::new(
                "app.undo_unavailable",
                "Only a completed, not-yet-undone move can be undone.",
            ));
        }
        let current = roots.path(&r.path)?;
        let original = roots.path(&r.from)?;
        if quarantined_under(facts, &r.path) || quarantined_under(facts, &r.from) {
            return Err(ops::quarantine_refusal("This item"));
        }
        let (current_parent, current_name) = roots.parent(&current)?;
        let (original_parent, original_name) = roots.parent(&original)?;
        let source = current_parent
            .prepare_rename(&current_name)
            .map_err(cap_failure)?;
        if identity_from_meta(source.metadata().map_err(cap_failure)?) != r.identity {
            return Err(Failure::new(
                "app.file_changed",
                "This item changed or was replaced since the move; undo was refused.",
            ));
        }
        records[at].state = "undoing".into();
        self.save(&records)?;
        if identity_from_meta(source.metadata().map_err(cap_failure)?) != r.identity {
            return Err(Failure::new(
                "app.path_changed",
                "The source changed before undo; review its receipt.",
            ));
        }
        let result = source.rename_noreplace(&original_parent, &original_name);
        records[at].state = if result.is_ok() { "undone" } else { "complete" }.into();
        if self.save(&records).is_err() {
            return Err(Failure::new(
                "app.history_uncertain",
                "Undo's result could not be saved. Review both paths before another action.",
            ));
        }
        result.map_err(|error| {
            if matches!(error, zeus_fs::FsError::AlreadyExists) {
                return cap_failure(error);
            }
            Failure::new(
                "app.undo_failed",
                "Undo could not move the item. No existing destination was replaced.",
            )
        })?;
        Ok(json!({"ok":true,"from":r.path,"path":r.from,"id":r.id}))
    }
    /// Reconcile an interrupted intent after a person requests it; inspect identities without replay.
    pub fn reconcile(&self, id: &str, roots: &[String]) -> Result<Value> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| Failure::new("app.io", "File-operation history is busy."))?;
        let row = records
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| Failure::invalid("Unknown operation."))?;
        if !matches!(row.state.as_str(), "pending" | "undoing") {
            return Err(Failure::invalid(
                "This receipt does not need reconciliation.",
            ));
        }
        let from = resolve_new_inside(&row.from, roots)?;
        let to = resolve_new_inside(&row.path, roots)?;
        let original_matches = identity(&from).ok().as_ref() == Some(&row.identity);
        let target_matches = identity(&to).ok().as_ref() == Some(&row.identity);
        if original_matches
            && std::fs::symlink_metadata(&to)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            row.state = if row.state == "undoing" {
                "undone"
            } else {
                "failed"
            }
            .into();
        } else if target_matches
            && std::fs::symlink_metadata(&from)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            row.state = "complete".into();
        } else {
            return Err(Failure::new(
                "app.history_uncertain",
                "Both paths need manual review; their identities do not establish a single completed operation.",
            ));
        }
        self.save(&records)?;
        Ok(json!({"ok":true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Vec<String>, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let from = root.join("before.txt");
        let to = root.join("after.txt");
        std::fs::write(&from, b"original").unwrap();
        let roots = vec![shown(&root)];
        (dir, roots, from, to)
    }
    #[test]
    fn restart_retains_undo_and_refuses_changed_identity_occupied_path_and_revocation() {
        let (dir, roots, from, to) = setup();
        let db = dir.path().join("ops.json");
        let history = Journal::new(db.clone());
        let moved = history
            .relocate(&shown(&from), &shown(&to), &roots, &HashMap::new())
            .unwrap();
        let id = moved["id"].as_str().unwrap();
        drop(history);
        let history = Journal::new(db);
        std::fs::write(&from, b"occupied").unwrap();
        assert_eq!(
            history.undo(id, &roots, &HashMap::new()).unwrap_err().code,
            "app.exists"
        );
        std::fs::remove_file(&from).unwrap();
        assert!(history.undo(id, &[], &HashMap::new()).is_err());
        history.undo(id, &roots, &HashMap::new()).unwrap();
        assert_eq!(std::fs::read(&from).unwrap(), b"original");
        assert!(history.undo(id, &roots, &HashMap::new()).is_err());
    }
    #[test]
    fn changed_file_and_corrupt_receipts_are_fail_closed() {
        let (dir, roots, from, to) = setup();
        let db = dir.path().join("ops.json");
        let history = Journal::new(db.clone());
        let moved = history
            .relocate(&shown(&from), &shown(&to), &roots, &HashMap::new())
            .unwrap();
        std::fs::write(&to, b"edited content").unwrap();
        assert_eq!(
            history
                .undo(moved["id"].as_str().unwrap(), &roots, &HashMap::new())
                .unwrap_err()
                .code,
            "app.file_changed"
        );
        std::fs::write(&db, b"broken").unwrap();
        let broken = Journal::new(db);
        assert!(
            broken
                .relocate(&shown(&to), &shown(&from), &roots, &HashMap::new())
                .is_err()
        );
        assert!(to.exists());
    }
    #[test]
    fn directories_and_interrupted_receipts_reconcile_without_replaying() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let from = root.join("before");
        let to = root.join("after");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("keep.txt"), b"keep").unwrap();
        let roots = vec![shown(&root)];
        let db = dir.path().join("ops.json");
        let history = Journal::new(db.clone());
        let moved = history
            .relocate(&shown(&from), &shown(&to), &roots, &HashMap::new())
            .unwrap();
        let id = moved["id"].as_str().unwrap();
        {
            let mut records = history.records.lock().unwrap();
            records[0].state = "pending".into();
            history.save(&records).unwrap();
        }
        drop(history);
        let history = Journal::new(db);
        assert!(!from.exists());
        history.reconcile(id, &roots).unwrap();
        assert!(!from.exists());
        history.undo(id, &roots, &HashMap::new()).unwrap();
        assert_eq!(std::fs::read(from.join("keep.txt")).unwrap(), b"keep");
        assert!(!to.exists());
    }
}
