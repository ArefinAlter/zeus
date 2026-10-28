//! Bounded desktop history. Only typed event names, job ids, times and states are cached.
//! Cursor and projected records are replaced together; failed reads/writes never advance the cursor.

use crate::{
    daemon::Daemon,
    failure::{Failure, Result},
    home::AppHome,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tokio::sync::Mutex;
use zeus_api::methods::events::{EventCursor, EventsReadResult};
use zeus_types::{Event, EventKind, EventPayload, JobId, JobState, Timestamp};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 2000;
const PER_JOB: usize = 50;

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    seq: u64,
    ts: Timestamp,
    job_id: JobId,
    kind: String,
    state: Option<JobState>,
}

impl Entry {
    fn from_event(event: &Event) -> Option<Self> {
        let kind = EventKind::from_name(event.payload.type_name())?;
        if !kind.requires_job_id() || !kind.is_durable() {
            return None;
        }
        Some(Self {
            seq: event.seq?.get(),
            ts: event.ts,
            job_id: event.job_id?,
            kind: kind.name().into(),
            state: match &event.payload {
                EventPayload::JobStateChanged(s) => Some(s.to),
                _ => None,
            },
        })
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Cache {
    cursor: Option<EventCursor>,
    entries: Vec<Entry>,
    history_lost: bool,
    limited: bool,
    replaying: bool,
}

impl Cache {
    fn valid(&self) -> bool {
        self.entries.len() <= MAX_ENTRIES
            && self
                .cursor
                .as_ref()
                .is_none_or(|c| c.instance_id.len() <= 64)
            && self.entries.iter().all(|e| {
                e.seq > 0
                    && self.cursor.as_ref().is_some_and(|c| e.seq <= c.seq)
                    && EventKind::from_name(&e.kind)
                        .is_some_and(|k| k.requires_job_id() && k.is_durable())
                    && ((e.kind == "job.state_changed") == e.state.is_some())
            })
            && self.entries.windows(2).all(|e| e[0].seq < e[1].seq)
    }

    fn apply(&mut self, page: EventsReadResult) -> Result<()> {
        let previous = self.cursor.as_ref();
        let reset = page.reset.is_some();
        if !reset
            && previous.is_some_and(|c| {
                c.store_id != page.cursor.store_id
                    || c.instance_id != page.cursor.instance_id
                    || c.seq > page.cursor.seq
            })
        {
            return Err(Failure::new(
                "app.history_protocol",
                "The engine returned an invalid history checkpoint.",
            ));
        }
        let start = if reset {
            0
        } else {
            previous.map_or(0, |c| c.seq)
        };
        let mut last = start;
        for event in &page.events {
            let seq = event.seq.map_or(0, |s| s.get());
            if seq <= last || seq > page.cursor.seq {
                return Err(Failure::new(
                    "app.history_protocol",
                    "The engine returned an invalid history order.",
                ));
            }
            last = seq;
        }
        if page.cursor.seq > page.head_seq
            || page.has_more != (page.cursor.seq < page.head_seq)
            || (page.has_more && page.cursor.seq == start)
        {
            return Err(Failure::new(
                "app.history_protocol",
                "The engine did not advance history replay.",
            ));
        }
        if reset {
            *self = Self::default();
        }
        self.history_lost |= page.history_lost;
        self.entries
            .extend(page.events.iter().filter_map(Entry::from_event));
        let mut counts = HashMap::<JobId, usize>::new();
        let before = self.entries.len();
        self.entries.reverse();
        self.entries.retain(|e| {
            let n = counts.entry(e.job_id).or_default();
            *n += 1;
            *n <= PER_JOB
        });
        self.entries.truncate(MAX_ENTRIES);
        self.entries.reverse();
        self.limited |= self.entries.len() < before;
        self.cursor = Some(page.cursor);
        self.replaying = page.has_more;
        Ok(())
    }
}

/// The native app's private display cache; it grants no authority to act on a job.
pub struct History {
    path: PathBuf,
    cache: Mutex<Option<Cache>>,
}

impl History {
    /// Open lazily under the app's own profile.
    pub fn new(home: &AppHome) -> Self {
        Self {
            path: home.app_dir().join("history/events.json"),
            cache: Mutex::new(None),
        }
    }

    /// Replay at most four pages per call. Concurrent windows share one cursor and atomic cache.
    pub async fn refresh(&self, daemon: &Daemon) -> Result<serde_json::Value> {
        let mut guard = self.cache.lock().await;
        let old = guard.get_or_insert_with(|| load(&self.path));
        let mut next = old.clone();
        for _ in 0..4 {
            let reply = daemon
                .raw(
                    "events.read",
                    serde_json::json!({"cursor":next.cursor,"limit":128}),
                )
                .await?;
            let page: EventsReadResult = serde_json::from_value(reply)?;
            next.apply(page)?;
            if !next.replaying {
                break;
            }
        }
        let value = serde_json::to_value(&next)?;
        if serde_json::to_value(&*old)? != value {
            save(&self.path, &next)?;
            *old = next;
        }
        Ok(value)
    }

    /// Clear local display history; the journal remains the source of truth on the next refresh.
    pub async fn clear(&self) -> Result<()> {
        let mut guard = self.cache.lock().await;
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        *guard = Some(Cache::default());
        Ok(())
    }
}

fn load(path: &Path) -> Cache {
    let read = || -> Option<Cache> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_BYTES {
            return None;
        }
        let cache: Cache = serde_json::from_slice(&bytes).ok()?;
        cache.valid().then_some(cache)
    };
    read().unwrap_or_default()
}

fn save(path: &Path, cache: &Cache) -> Result<()> {
    let bytes = serde_json::to_vec(cache)?;
    if bytes.len() > MAX_BYTES {
        return Err(Failure::new(
            "app.history_limit",
            "The history cache exceeded its size limit.",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| Failure::invalid("Invalid history path."))?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeus_api::methods::events::EventReset;

    fn page(first: u64, count: u64, job: JobId) -> EventsReadResult {
        let events = (first..first+count).map(|seq| serde_json::from_value(serde_json::json!({
            "seq":seq,"ts":"2026-10-28T00:00:00.000Z","job_id":job,"type":"job.completed",
            "data":{"path":"ZEUS-CANARY-PRIVATE-PATH","size_bytes":3,"sha256":"00".repeat(32),"blake3":"11".repeat(32),"quarantined":false,"duration_ms":1}
        })).unwrap()).collect();
        EventsReadResult {
            events,
            cursor: EventCursor {
                store_id: "01K8M4VZ390000000000000001".parse().unwrap(),
                instance_id: "instance".into(),
                seq: first + count - 1,
            },
            head_seq: first + count - 1,
            has_more: false,
            reset: None,
            history_lost: false,
        }
    }

    #[test]
    fn cache_and_cursor_round_trip_together_without_raw_event_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.json");
        let mut cache = Cache::default();
        cache
            .apply(page(1, 4, zeus_types::IdGenerator::new().job_id()))
            .unwrap();
        save(&path, &cache).unwrap();
        let restored = load(&path);
        assert_eq!(restored.cursor, cache.cursor);
        assert_eq!(restored.entries.len(), 4);
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("ZEUS-CANARY")
        );
        // Failure before replacement leaves the previous cursor and records readable together.
        std::fs::create_dir(path.with_extension("tmp")).unwrap();
        cache
            .apply(page(5, 2, zeus_types::IdGenerator::new().job_id()))
            .unwrap();
        assert!(save(&path, &cache).is_err());
        assert_eq!(load(&path).cursor, restored.cursor);
        std::fs::write(&path, b"{partial").unwrap();
        assert!(load(&path).cursor.is_none());
    }

    #[test]
    fn replay_is_ordered_bounded_and_reset_drops_the_previous_store() {
        let mut cache = Cache::default();
        let job = zeus_types::IdGenerator::new().job_id();
        cache.apply(page(1, 60, job)).unwrap();
        assert_eq!(cache.entries.len(), PER_JOB);
        assert!(cache.limited && cache.valid());
        assert!(cache.apply(page(59, 2, job)).is_err());
        let mut replacement = page(1, 2, zeus_types::IdGenerator::new().job_id());
        replacement.reset = Some(EventReset::StoreChanged);
        replacement.cursor.store_id = zeus_types::IdGenerator::new().store_id();
        replacement.history_lost = true;
        cache.apply(replacement).unwrap();
        assert_eq!(cache.entries.len(), 2);
        assert!(!cache.limited);
        assert!(cache.history_lost);
        assert!(cache.entries.iter().all(|e| e.job_id != job));
    }

    #[test]
    fn empty_filtered_pages_advance_and_unknown_events_are_not_displayed() {
        let mut cache = Cache::default();
        let mut p = page(1, 1, zeus_types::IdGenerator::new().job_id());
        p.events.clear();
        p.cursor.seq = 8;
        p.head_seq = 10;
        p.has_more = true;
        cache.apply(p.clone()).unwrap();
        assert!(cache.replaying && cache.entries.is_empty() && !cache.history_lost);
        assert!(cache.apply(p).is_err()); // no progress must not loop forever
        let mut p = page(9, 2, zeus_types::IdGenerator::new().job_id());
        p.events[0].payload = EventPayload::Unknown {
            kind: "ZEUS-CANARY-UNKNOWN".into(),
            data: Default::default(),
        };
        cache.apply(p).unwrap();
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.cursor.unwrap().seq, 10);
    }

    #[test]
    fn global_cache_size_is_bounded_and_corrupt_metadata_restarts_replay() {
        let mut cache = Cache::default();
        for n in 0..45 {
            cache
                .apply(page(
                    n * 50 + 1,
                    50,
                    zeus_types::IdGenerator::new().job_id(),
                ))
                .unwrap();
        }
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(cache.valid() && cache.limited);
        assert!(serde_json::to_vec(&cache).unwrap().len() < MAX_BYTES);
        cache.entries[0].kind = "ZEUS-CANARY-CORRUPT".into();
        assert!(!cache.valid());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.json");
        save(&path, &cache).unwrap();
        assert!(load(&path).cursor.is_none());
    }
}
