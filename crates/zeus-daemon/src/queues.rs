//! Download queues: the persistent store and the scheduler (05 section 9.14, ADR 0012).
//!
//! A queue holds downloads until it runs. The store keeps the queues' settings, which downloads belong to which queue
//! and in what order, which of them the daemon is holding back, the speed limiter's settings and the scheduler
//! defaults. It is one small file, `queues.json` in the state directory, rewritten atomically after every change, so
//! queues, membership and held state survive a restart. The engine's journal is never touched.
//!
//! A download joins a queue when it is enqueued with the label `zeus.queue=<id>` or through `queues.control`. The
//! scheduler ticks once a second and after every change: it starts and stops queues on their schedule, holds the
//! members of a queue that is not running by pausing them (they are paused in the engine and reported as held), releases
//! held members in order up to the queue's `max_concurrent`, retries failures that are worth retrying, and notices when
//! a queue has run out of work. It never resumes anything while the kill switch is engaged, and a schedule never
//! releases the kill switch.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use zeus_api::methods::queues::{
    LimiterDef, LimiterView, MAIN_QUEUE, MAX_LIMIT_KIB_S, MAX_QUEUED_JOBS, MAX_QUEUES,
    MissedWindow, NetworkCost, NetworkMode, NetworkView, OnFinish, QUEUE_LABEL, QueueAction,
    QueueDef, QueueDefaults, QueueKind, QueueView, StopBehaviour, SyncFileView, SyncStatus,
};
use zeus_api::{ApiError, ApiResult};
use zeus_types::{JobId, JobSpec, JobState};

#[path = "queue_network.rs"]
mod network;

use crate::limiter::{Limits, fingerprint};
use crate::queue_schedule::{Clock, parse_hm, parse_ymd, valid_days};

/// Which job ids a caller may see and name. Services see only their own jobs; every other caller sees all.
pub type Visible<'a> = &'a (dyn Fn(JobId) -> bool + Sync);

/// How often the scheduler persists its own clock when nothing else changed.
const CLOCK_SAVE_MS: i64 = 30_000;

/// What the scheduler needs to know about a job, and the three things it may do to one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// The job's state.
    pub state: JobState,
    /// The job failed with an error worth retrying.
    pub retryable: bool,
}

/// The scheduler's view of the engine. The daemon implements it over the engine and the journal; tests implement it
/// with a script.
#[async_trait]
pub trait JobPort: Send + Sync {
    /// The job, or `None` when it no longer exists.
    fn observe(&self, id: JobId) -> Option<Observed>;
    /// The fingerprint of the job's request (see [`crate::limiter::fingerprint`]).
    fn fingerprint(&self, id: JobId) -> Option<String>;
    /// Pause the job. True when the engine accepted it.
    async fn pause(&self, id: JobId) -> bool;
    /// Resume a paused job. True when the engine accepted it.
    async fn resume(&self, id: JobId) -> bool;
    /// Retry a failed job. True when the engine accepted it.
    async fn retry(&self, id: JobId) -> bool;
    /// The kill switch is engaged.
    fn kill_switch(&self) -> bool;
    /// Read the operating system network-cost hint.
    async fn network_cost(&self) -> NetworkCost {
        NetworkCost::Unknown
    }
    /// Incomplete jobs, including downloads outside named queues.
    fn incomplete(&self) -> Vec<(JobId, JobState)> {
        Vec::new()
    }
    /// Check whether a completed file has a new strongly identified representation.
    async fn sync_probe(&self, _id: JobId) -> SyncCheck {
        SyncCheck::status(SyncStatus::Unverifiable)
    }
    /// Enqueue a rename-only replacement under the original caller and destination.
    async fn sync_enqueue(
        &self,
        _id: JobId,
        _queue: &str,
        _version: &str,
    ) -> Option<(JobId, JobState)> {
        None
    }
    /// Unfinished labelled jobs recovered after a crash.
    fn labelled(&self) -> Vec<(JobId, JobState, JobSpec)> {
        Vec::new()
    }
}

/// Bounded synchronization observation; the version key contains only a digest.
pub struct SyncCheck {
    /// Outcome of the validator comparison.
    pub status: SyncStatus,
    /// Digest identifying a changed representation.
    pub version: Option<String>,
}
impl SyncCheck {
    fn status(status: SyncStatus) -> Self {
        Self {
            status,
            version: None,
        }
    }
}

/// Current requester permission and destination-root check for a new synchronized version.
pub type SyncAdmission = dyn Fn(&JobSpec, &Path) -> bool + Send + Sync;

/// [`JobPort`] over the real engine and journal.
pub struct EnginePort {
    /// The engine.
    pub engine: zeus_engine::Engine,
    /// Its journal.
    pub journal: zeus_journal::Journal,
    /// Optional signed team restrictions also apply to scheduled effects.
    pub team: crate::team::policy::Guard,
    /// Check that the original requester remains authorized for this destination.
    pub authorize_sync: Arc<SyncAdmission>,
}

#[async_trait]
impl JobPort for EnginePort {
    fn observe(&self, id: JobId) -> Option<Observed> {
        let record = self.journal.job(id).ok().flatten()?;
        Some(Observed {
            state: record.job.state,
            retryable: record.job.error.as_ref().is_some_and(|e| e.retryable),
        })
    }

    fn fingerprint(&self, id: JobId) -> Option<String> {
        let record = self.journal.job(id).ok().flatten()?;
        Some(fingerprint(&record.job.spec))
    }

    async fn pause(&self, id: JobId) -> bool {
        self.engine
            .control(id, zeus_engine::api::ControlAction::Pause)
            .await
            .is_ok()
    }

    async fn resume(&self, id: JobId) -> bool {
        if self.team.admit("downloads").is_err() {
            return false;
        }
        self.engine
            .control(id, zeus_engine::api::ControlAction::Resume)
            .await
            .is_ok()
    }

    async fn retry(&self, id: JobId) -> bool {
        if self.team.admit("downloads").is_err() {
            return false;
        }
        self.engine
            .control(id, zeus_engine::api::ControlAction::Retry)
            .await
            .is_ok()
    }

    fn kill_switch(&self) -> bool {
        self.journal
            .meta()
            .map(|m| m.kill_switch_engaged())
            .unwrap_or(true)
    }

    async fn network_cost(&self) -> NetworkCost {
        network::detect().await
    }

    fn incomplete(&self) -> Vec<(JobId, JobState)> {
        self.journal
            .list_incomplete()
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.job.id, r.job.state))
            .collect()
    }

    async fn sync_probe(&self, id: JobId) -> SyncCheck {
        if self.team.admit("downloads").is_err() {
            return SyncCheck::status(SyncStatus::PolicyBlocked);
        }
        let Some(job) = self.journal.job(id).ok().flatten().map(|r| r.job) else {
            return SyncCheck::status(SyncStatus::Failed);
        };
        if !job
            .result
            .as_ref()
            .is_some_and(|result| (self.authorize_sync)(&job.spec, Path::new(&result.path)))
        {
            return SyncCheck::status(SyncStatus::PolicyBlocked);
        }
        if job.state != JobState::Completed
            || job.quarantined
            || job.spec.expected_sha256.is_some()
            || job.spec.expected_size.is_some()
            || !job.spec.extra_headers.is_empty()
        {
            return SyncCheck::status(SyncStatus::Unverifiable);
        }
        let Some(old) = job.probe.as_ref().and_then(representation_key) else {
            return SyncCheck::status(SyncStatus::Unverifiable);
        };
        let probe = self
            .engine
            .probe(zeus_engine::api::ProbeUrlRequest {
                url: job.spec.url.clone(),
                session_origin: job.spec.session_origin.clone(),
                caller: sync_caller(&job.spec),
            })
            .await;
        let Ok(probe) = probe else {
            return SyncCheck::status(SyncStatus::Failed);
        };
        if probe.requires_session {
            return SyncCheck::status(SyncStatus::NeedsSession);
        }
        if !(200..300).contains(&probe.status) || probe.interstitial_suspected {
            return SyncCheck::status(SyncStatus::Failed);
        }
        let Some(new) = representation_key(&probe) else {
            return SyncCheck::status(SyncStatus::Unverifiable);
        };
        if new == old {
            SyncCheck::status(SyncStatus::Unchanged)
        } else {
            SyncCheck {
                status: SyncStatus::Downloading,
                version: Some(new),
            }
        }
    }

    async fn sync_enqueue(
        &self,
        id: JobId,
        queue: &str,
        version: &str,
    ) -> Option<(JobId, JobState)> {
        if self.team.admit("downloads").is_err() {
            return None;
        }
        let job = self.journal.job(id).ok().flatten()?.job;
        if !job
            .result
            .as_ref()
            .is_some_and(|result| (self.authorize_sync)(&job.spec, Path::new(&result.path)))
        {
            return None;
        }
        if job.state != JobState::Completed
            || job.quarantined
            || job.spec.expected_sha256.is_some()
            || job.spec.expected_size.is_some()
            || !job.spec.extra_headers.is_empty()
        {
            return None;
        }
        let path = Path::new(&job.result.as_ref()?.path);
        let mut spec = job.spec.clone();
        spec.dest_dir = Some(path.parent()?.to_str()?.to_owned());
        spec.filename = Some(path.file_name()?.to_str()?.to_owned());
        spec.hint_url = None;
        spec.labels.insert(QUEUE_LABEL.into(), queue.to_owned());
        spec.overwrite = Some(zeus_types::Overwrite::Rename);
        spec.idempotency_key = Some(format!("zeus-sync:{id}:{version}"));
        let caller = sync_caller(&spec);
        let outcome = self
            .engine
            .enqueue(zeus_engine::api::EnqueueRequest { spec, caller })
            .await
            .ok()?;
        Some((outcome.job.id, outcome.job.state))
    }

    fn labelled(&self) -> Vec<(JobId, JobState, JobSpec)> {
        self.journal
            .list_incomplete()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.job.spec.labels.contains_key(QUEUE_LABEL))
            .map(|r| (r.job.id, r.job.state, r.job.spec))
            .collect()
    }
}

fn sync_caller(spec: &JobSpec) -> zeus_engine::api::Caller {
    zeus_engine::api::Caller {
        client_id: spec
            .source
            .client_id
            .clone()
            .unwrap_or_else(|| "zeus-queue-sync".into()),
        kind: zeus_types::ActorKind::System,
    }
}

fn representation_key(probe: &zeus_types::ProbeResult) -> Option<String> {
    let validator = if probe.validators.has_strong_etag() {
        format!("etag:{}", probe.validators.etag.as_deref()?)
    } else if probe.validators.last_modified_strong == Some(true) && probe.total_bytes.is_some() {
        format!("modified:{}", probe.validators.last_modified.as_deref()?)
    } else {
        return None;
    };
    Some(
        blake3::hash(format!("{validator}:{:?}", probe.total_bytes).as_bytes())
            .to_hex()
            .to_string(),
    )
}

fn internal() -> ApiError {
    ApiError::internal("queue store unavailable")
}

fn invalid(message: &str) -> ApiError {
    ApiError::invalid_params(message.to_owned())
}

fn not_found(what: &str) -> ApiError {
    ApiError::new("api.not_found", format!("{what} not found"))
}

fn kill_switch_error() -> ApiError {
    ApiError::new(
        "policy.kill_switch_engaged",
        "kill switch engaged; a human must release it",
    )
}

/// States that use one of a queue's slots.
fn uses_slot(state: JobState) -> bool {
    matches!(
        state,
        JobState::Queued
            | JobState::Probing
            | JobState::Downloading
            | JobState::Verifying
            | JobState::Finalizing
            | JobState::RetryScheduled
    )
}

/// States the daemon may pause to hold a job back (verifying and finalizing cannot be interrupted).
fn holdable(state: JobState) -> bool {
    matches!(
        state,
        JobState::Queued | JobState::Probing | JobState::Downloading | JobState::RetryScheduled
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredQueue {
    #[serde(flatten)]
    def: QueueDef,
    #[serde(default)]
    running: bool,
    /// A scheduled stop with `finish_current`: running downloads finish, none is released.
    #[serde(default)]
    draining: bool,
    #[serde(default)]
    last_started_ms: Option<u64>,
    #[serde(default)]
    finished_at_ms: Option<u64>,
    #[serde(default)]
    finish_action_pending: bool,
    /// Something was released, enqueued or finished during this run.
    #[serde(default)]
    had_work: bool,
}

impl StoredQueue {
    fn new(def: QueueDef) -> Self {
        Self {
            def,
            running: false,
            draining: false,
            last_started_ms: None,
            finished_at_ms: None,
            finish_action_pending: false,
            had_work: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Member {
    job_id: JobId,
    queue: String,
    fingerprint: String,
    /// Paused by the daemon until the queue releases it.
    #[serde(default)]
    held: bool,
    #[serde(default)]
    retries_used: u32,
    #[serde(default)]
    retry_at_ms: Option<u64>,
    /// The state seen at the last tick.
    #[serde(default)]
    state: Option<JobState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Watch {
    queue: String,
    #[serde(flatten)]
    view: SyncFileView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    version: u32,
    queues: Vec<StoredQueue>,
    limiter: LimiterDef,
    defaults: QueueDefaults,
    members: Vec<Member>,
    #[serde(default)]
    watches: Vec<Watch>,
    #[serde(default)]
    metered_held: Vec<JobId>,
    #[serde(default)]
    manual_paused: Vec<JobId>,
    #[serde(default)]
    last_tick_ms: i64,
}

fn main_queue() -> QueueDef {
    QueueDef {
        id: MAIN_QUEUE.into(),
        name: "Main queue".into(),
        kind: Default::default(),
        sync_every_hours: 24,
        max_concurrent: 3,
        retries: 0,
        retry_delay_s: 30,
        schedule: Default::default(),
        on_finish: OnFinish::Nothing,
        limit_kib_s: None,
    }
}

impl Stored {
    fn fresh() -> Self {
        Self {
            version: 1,
            queues: vec![StoredQueue::new(main_queue())],
            limiter: LimiterDef::default(),
            defaults: QueueDefaults::default(),
            members: Vec::new(),
            watches: Vec::new(),
            metered_held: Vec::new(),
            manual_paused: Vec::new(),
            last_tick_ms: 0,
        }
    }

    fn queue(&self, id: &str) -> Option<&StoredQueue> {
        self.queues.iter().find(|q| q.def.id == id)
    }

    fn queue_mut(&mut self, id: &str) -> Option<&mut StoredQueue> {
        self.queues.iter_mut().find(|q| q.def.id == id)
    }
}

struct Inner {
    stored: Stored,
    /// Jobs left without membership by a crash have been looked for.
    adopted: bool,
    /// The previous tick's instant: a start due since then fires now.
    prev_tick_ms: i64,
    last_saved_ms: i64,
    dirty: bool,
    network_cost: NetworkCost,
    network_checked_ms: Option<i64>,
}

/// Whether a queue definition is acceptable.
pub fn validate_def(def: &QueueDef) -> ApiResult<()> {
    if !(1..=720).contains(&def.sync_every_hours) {
        return Err(invalid("sync_every_hours must be between 1 and 720"));
    }
    let id_ok = !def.id.is_empty()
        && def.id.len() <= 32
        && def
            .id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if !id_ok {
        return Err(invalid(
            "queue ids use 1 to 32 characters from a-z, 0-9, _ and -",
        ));
    }
    let name = def.name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(invalid("a queue name has 1 to 80 characters"));
    }
    if !(1..=16).contains(&def.max_concurrent) {
        return Err(invalid("max_concurrent is 1 to 16"));
    }
    if def.retries > 50 {
        return Err(invalid("retries is 0 to 50"));
    }
    if !(1..=3600).contains(&def.retry_delay_s) {
        return Err(invalid("retry_delay_s is 1 to 3600"));
    }
    if def
        .limit_kib_s
        .is_some_and(|l| !(1..=MAX_LIMIT_KIB_S).contains(&l))
    {
        return Err(invalid("limit_kib_s is 1 to 10000000, or null"));
    }
    let s = &def.schedule;
    if parse_hm(&s.start_time).is_none() || parse_hm(&s.stop_time).is_none() {
        return Err(invalid("schedule times are HH:MM"));
    }
    if !valid_days(&s.days) {
        return Err(invalid("schedule days are the weekdays 0 to 6, each once"));
    }
    if s.start_enabled
        && s.repeat == zeus_api::methods::queues::Repeat::Once
        && parse_ymd(&s.start_date).is_none()
    {
        return Err(invalid("a one-time start needs start_date as YYYY-MM-DD"));
    }
    Ok(())
}

/// Whether a limiter definition is acceptable.
pub fn validate_limiter(l: &LimiterDef) -> ApiResult<()> {
    if !(1..=MAX_LIMIT_KIB_S).contains(&l.limit_kib_s) {
        return Err(invalid("limit_kib_s is 1 to 10000000"));
    }
    if parse_hm(&l.from).is_none() || parse_hm(&l.to).is_none() {
        return Err(invalid("limiter window times are HH:MM"));
    }
    if !valid_days(&l.days) {
        return Err(invalid("limiter days are the weekdays 0 to 6, each once"));
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis().min(i64::MAX as u128) as i64)
}

/// What `queues.list` reports.
pub struct Listing {
    /// Every queue, the built-in one first.
    pub queues: Vec<QueueView>,
    /// The limiter.
    pub limiter: LimiterView,
    /// The defaults.
    pub defaults: QueueDefaults,
    /// The daemon's clock.
    pub now_ms: i64,
    /// Offset of the schedule clock from UTC in minutes.
    pub utc_offset_min: i32,
    /// Actual network-cost policy observation.
    pub network: NetworkView,
}

/// The queues' store and scheduler.
pub struct QueueService {
    path: PathBuf,
    inner: Mutex<Inner>,
    limits: Arc<Limits>,
    port: OnceLock<Arc<dyn JobPort>>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    tick_gate: Mutex<()>,
}

impl QueueService {
    /// Open (or create) the store in `state_dir` and apply its limits.
    pub fn open(state_dir: &Path, limits: Arc<Limits>) -> ApiResult<Arc<Self>> {
        Self::open_with(state_dir, limits, Arc::new(system_now_ms))
    }

    /// [`Self::open`] with an injected clock (milliseconds since the Unix epoch).
    pub fn open_with(
        state_dir: &Path,
        limits: Arc<Limits>,
        now: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> ApiResult<Arc<Self>> {
        let path = state_dir.join("queues.json");
        let stored = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Stored>(&bytes) {
                Ok(s) if s.version == 1 && s.queue(MAIN_QUEUE).is_some() => s,
                _ => {
                    // A damaged or unknown store must not keep the daemon from starting: keep it aside.
                    tracing::warn!("queues.json is unreadable; starting with empty queues");
                    let _ = std::fs::rename(&path, path.with_extension("json.damaged"));
                    Stored::fresh()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stored::fresh(),
            Err(_) => return Err(internal()),
        };
        let started = now();
        let prev =
            if stored.defaults.missed_window == MissedWindow::RunOnce && stored.last_tick_ms > 0 {
                stored.last_tick_ms
            } else {
                started
            };
        let service = Arc::new(Self {
            path,
            inner: Mutex::new(Inner {
                stored,
                adopted: false,
                prev_tick_ms: prev,
                last_saved_ms: started,
                dirty: false,
                network_cost: NetworkCost::Unknown,
                network_checked_ms: None,
            }),
            limits,
            port: OnceLock::new(),
            now,
            tick_gate: Mutex::new(()),
        });
        // Apply the stored limits and membership (the mutex is free: nothing else holds a reference yet).
        if let Ok(inner) = service.inner.try_lock() {
            service.apply_all(&inner.stored, started);
        }
        Ok(service)
    }

    /// Connect the scheduler to the engine. Until then ticks only keep the limiter current.
    pub fn attach(&self, port: Arc<dyn JobPort>) {
        let _ = self.port.set(port);
    }

    /// Whether a queue with this id exists.
    pub async fn knows(&self, id: &str) -> bool {
        self.inner.lock().await.stored.queue(id).is_some()
    }

    fn clock(stored: &Stored) -> Clock {
        Clock::new(stored.defaults.timezone)
    }

    fn apply_all(&self, stored: &Stored, now: i64) {
        for q in &stored.queues {
            self.limits.set_queue(&q.def.id, q.def.limit_kib_s);
        }
        for m in &stored.members {
            self.limits.set_member(&m.fingerprint, Some(&m.queue));
        }
        self.apply_limiter(stored, now);
    }

    fn apply_limiter(&self, stored: &Stored, now: i64) {
        let active = Self::clock(stored).limiter_active(&stored.limiter, now);
        self.limits
            .set_global(active.then_some(stored.limiter.limit_kib_s));
    }

    fn save(&self, inner: &mut Inner, now: i64) -> ApiResult<()> {
        inner.stored.last_tick_ms = now;
        let bytes = serde_json::to_vec_pretty(&inner.stored).map_err(|_| internal())?;
        match write_atomic(&self.path, &bytes) {
            Ok(()) => {
                inner.dirty = false;
                inner.last_saved_ms = now;
                Ok(())
            }
            Err(_) => {
                inner.dirty = true;
                Err(internal())
            }
        }
    }

    // ---------------------------------------------------------------- views

    fn view(stored: &Stored, q: &StoredQueue, now: i64, visible: Visible<'_>) -> QueueView {
        let clock = Self::clock(stored);
        let members: Vec<&Member> = stored
            .members
            .iter()
            .filter(|m| m.queue == q.def.id && visible(m.job_id))
            .collect();
        let active = members
            .iter()
            .filter(|m| !m.held && m.state.is_some_and(uses_slot))
            .count();
        let held: Vec<JobId> = members
            .iter()
            .filter(|m| m.held)
            .map(|m| m.job_id)
            .collect();
        let started = q.last_started_ms.map(|t| t as i64);
        QueueView {
            def: q.def.clone(),
            builtin: q.def.id == MAIN_QUEUE,
            running: q.running,
            last_started_ms: q.last_started_ms,
            next_start_ms: clock
                .next_start(&q.def.schedule, now, false)
                .map(|t| t.max(0) as u64),
            next_stop_ms: if q.running {
                started
                    .and_then(|s| clock.stop_after(&q.def.schedule, s))
                    .map(|t| t.max(0) as u64)
            } else {
                None
            },
            waiting: held.len() as u32,
            active: active as u32,
            order: members
                .iter()
                .map(|m| m.job_id)
                .chain(
                    stored
                        .watches
                        .iter()
                        .filter(|w| w.queue == q.def.id && visible(w.view.job_id))
                        .map(|w| w.view.job_id),
                )
                .collect(),
            held_job_ids: held,
            finished_at_ms: q.finished_at_ms,
            finish_action_pending: q.finish_action_pending,
            sync_files: stored
                .watches
                .iter()
                .filter(|w| w.queue == q.def.id && visible(w.view.job_id))
                .map(|w| w.view.clone())
                .collect(),
        }
    }

    fn limiter_view(stored: &Stored, now: i64) -> LimiterView {
        LimiterView {
            def: stored.limiter.clone(),
            active_now: Self::clock(stored).limiter_active(&stored.limiter, now),
        }
    }

    /// Every queue with its live state, the limiter and the defaults. `visible` filters the job ids a caller may
    /// see.
    pub async fn list(&self, visible: Visible<'_>) -> Listing {
        let inner = self.inner.lock().await;
        let now = (self.now)();
        let s = &inner.stored;
        Listing {
            queues: s
                .queues
                .iter()
                .map(|q| Self::view(s, q, now, visible))
                .collect(),
            limiter: Self::limiter_view(s, now),
            defaults: s.defaults.clone(),
            now_ms: now,
            utc_offset_min: Self::clock(s).offset_minutes(now),
            network: Self::network_view(&inner),
        }
    }

    /// One queue's view.
    pub async fn queue_view(&self, id: &str, visible: Visible<'_>) -> ApiResult<QueueView> {
        let inner = self.inner.lock().await;
        let now = (self.now)();
        let q = inner.stored.queue(id).ok_or_else(|| not_found("queue"))?;
        Ok(Self::view(&inner.stored, q, now, visible))
    }

    // ---------------------------------------------------------------- changes

    /// Create a queue or replace its settings.
    pub async fn put(&self, def: QueueDef) -> ApiResult<()> {
        validate_def(&def)?;
        let mut inner = self.inner.lock().await;
        let now = (self.now)();
        match inner.stored.queue_mut(&def.id) {
            Some(q) => q.def = def.clone(),
            None => {
                if inner.stored.queues.len() >= MAX_QUEUES {
                    return Err(invalid("at most 32 queues"));
                }
                inner.stored.queues.push(StoredQueue::new(def.clone()));
            }
        }
        if def.kind != QueueKind::Sync {
            inner.stored.watches.retain(|w| w.queue != def.id);
        }
        self.limits.set_queue(&def.id, def.limit_kib_s);
        self.settle(&mut inner, now, true).await
    }

    /// Delete a queue; its members leave it and the ids of those are returned.
    pub async fn delete(&self, id: &str) -> ApiResult<Vec<JobId>> {
        if id == MAIN_QUEUE {
            return Err(invalid("the built-in queue cannot be deleted"));
        }
        let mut inner = self.inner.lock().await;
        let now = (self.now)();
        if inner.stored.queue(id).is_none() {
            return Err(not_found("queue"));
        }
        inner.stored.queues.retain(|q| q.def.id != id);
        inner.stored.watches.retain(|w| w.queue != id);
        let mut released = Vec::new();
        let limits = self.limits.clone();
        inner.stored.members.retain(|m| {
            if m.queue == id {
                limits.set_member(&m.fingerprint, None);
                released.push(m.job_id);
                false
            } else {
                true
            }
        });
        if inner.stored.defaults.default_queue == id {
            inner.stored.defaults.default_queue = MAIN_QUEUE.into();
        }
        self.limits.remove_queue(id);
        self.save(&mut inner, now)?;
        Ok(released)
    }

    /// Change the limiter and/or the defaults.
    pub async fn configure(
        &self,
        limiter: Option<LimiterDef>,
        defaults: Option<QueueDefaults>,
    ) -> ApiResult<(LimiterView, QueueDefaults)> {
        if let Some(l) = &limiter {
            validate_limiter(l)?;
        }
        let mut inner = self.inner.lock().await;
        if let Some(d) = &defaults
            && inner.stored.queue(&d.default_queue).is_none()
        {
            return Err(invalid("default_queue names no queue"));
        }
        let now = (self.now)();
        if let Some(l) = limiter {
            inner.stored.limiter = l;
        }
        if let Some(d) = defaults {
            inner.stored.defaults = d;
        }
        self.apply_limiter(&inner.stored, now);
        self.save(&mut inner, now)?;
        Ok((
            Self::limiter_view(&inner.stored, now),
            inner.stored.defaults.clone(),
        ))
    }

    /// Apply a `queues.control` action. `visible` filters which job ids the caller may name; an id outside the view
    /// is refused exactly like a missing one. Returns the queue concerned and the jobs affected.
    pub async fn control(
        &self,
        action: QueueAction,
        visible: Visible<'_>,
    ) -> ApiResult<(Option<String>, Vec<JobId>)> {
        let port = self.port.get().cloned();
        let mut inner = self.inner.lock().await;
        let now = (self.now)();
        let mut affected = Vec::new();
        let queue = match action {
            QueueAction::Start { id } => {
                if port.as_ref().is_some_and(|p| p.kill_switch()) {
                    return Err(kill_switch_error());
                }
                let q = inner
                    .stored
                    .queue_mut(&id)
                    .ok_or_else(|| not_found("queue"))?;
                Self::start(q, now);
                for watch in &mut inner.stored.watches {
                    if watch.queue == id && watch.view.pending_job_id.is_none() {
                        watch.view.next_check_ms = now.max(0) as u64;
                    }
                }
                let members: Vec<_> = inner
                    .stored
                    .members
                    .iter()
                    .filter(|m| m.queue == id)
                    .map(|m| m.job_id)
                    .collect();
                inner
                    .stored
                    .manual_paused
                    .retain(|job| !members.contains(job));
                // A paused member is waiting for this: hold it so that release order applies.
                for m in &mut inner.stored.members {
                    if m.queue == id && !m.held && m.state == Some(JobState::Paused) {
                        m.held = true;
                    }
                }
                Some(id)
            }
            QueueAction::Stop { id } => {
                let q = inner
                    .stored
                    .queue_mut(&id)
                    .ok_or_else(|| not_found("queue"))?;
                q.running = false;
                q.draining = false;
                Some(id)
            }
            QueueAction::Assign { id, job_ids } => {
                if job_ids.is_empty() || job_ids.len() > 100 {
                    return Err(invalid("job_ids has 1 to 100 entries"));
                }
                if inner.stored.queue(&id).is_none() {
                    return Err(not_found("queue"));
                }
                let port = port.clone().ok_or_else(internal)?;
                for job_id in &job_ids {
                    if !visible(*job_id) {
                        return Err(not_found("job"));
                    }
                }
                for job_id in job_ids {
                    let observed = port.observe(job_id).ok_or_else(|| not_found("job"))?;
                    if observed.state == JobState::Completed
                        && inner
                            .stored
                            .queue(&id)
                            .is_some_and(|q| q.def.kind == QueueKind::Sync)
                    {
                        if inner.stored.watches.len() >= MAX_QUEUED_JOBS
                            && !inner.stored.watches.iter().any(|w| w.view.job_id == job_id)
                        {
                            return Err(invalid("too many watched downloads"));
                        }
                        if let Some(pos) =
                            inner.stored.members.iter().position(|m| m.job_id == job_id)
                        {
                            let old = inner.stored.members.remove(pos);
                            self.limits.set_member(&old.fingerprint, None);
                        }
                        Self::watch_completed(&mut inner.stored, &id, job_id, now);
                        affected.push(job_id);
                        continue;
                    }
                    if matches!(observed.state, JobState::Completed | JobState::Canceled) {
                        return Err(invalid("a finished download cannot join a queue"));
                    }
                    let fp = port.fingerprint(job_id).ok_or_else(|| not_found("job"))?;
                    if let Some(old) = inner.stored.members.iter().position(|m| m.job_id == job_id)
                    {
                        let gone = inner.stored.members.remove(old);
                        if gone.queue != id {
                            self.limits.set_member(&gone.fingerprint, None);
                        }
                    } else if inner.stored.members.len() >= MAX_QUEUED_JOBS {
                        return Err(invalid("too many queued downloads"));
                    }
                    self.limits.set_member(&fp, Some(&id));
                    inner.stored.manual_paused.retain(|job| *job != job_id);
                    // A paused job that is assigned waits for its queue.
                    let held = observed.state == JobState::Paused;
                    inner.stored.members.push(Member {
                        job_id,
                        queue: id.clone(),
                        fingerprint: fp,
                        held,
                        retries_used: 0,
                        retry_at_ms: None,
                        state: Some(observed.state),
                    });
                    if let Some(q) = inner.stored.queue_mut(&id)
                        && q.running
                    {
                        q.had_work = true;
                    }
                    affected.push(job_id);
                }
                Some(id)
            }
            QueueAction::Unassign { job_ids } => {
                if job_ids.is_empty() || job_ids.len() > 100 {
                    return Err(invalid("job_ids has 1 to 100 entries"));
                }
                for job_id in &job_ids {
                    if !visible(*job_id) {
                        return Err(not_found("job"));
                    }
                }
                for job_id in job_ids {
                    inner.stored.watches.retain(|w| {
                        w.view.job_id != job_id && w.view.pending_job_id != Some(job_id)
                    });
                    if let Some(pos) = inner.stored.members.iter().position(|m| m.job_id == job_id)
                    {
                        let gone = inner.stored.members.remove(pos);
                        self.limits.set_member(&gone.fingerprint, None);
                        affected.push(job_id);
                    }
                }
                None
            }
            QueueAction::Move { job_id, offset } => {
                if !visible(job_id) {
                    return Err(not_found("job"));
                }
                let pos = inner
                    .stored
                    .members
                    .iter()
                    .position(|m| m.job_id == job_id)
                    .ok_or_else(|| not_found("queued download"))?;
                let queue = inner.stored.members[pos].queue.clone();
                let slots: Vec<usize> = inner
                    .stored
                    .members
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.queue == queue)
                    .map(|(i, _)| i)
                    .collect();
                let at = slots.iter().position(|&i| i == pos).ok_or_else(internal)?;
                let to = (at as i64 + i64::from(offset)).clamp(0, slots.len() as i64 - 1) as usize;
                if to != at {
                    // Rotate the queue's members between the two places, leaving other queues' members alone.
                    let mut ids: Vec<Member> = slots
                        .iter()
                        .map(|&i| inner.stored.members[i].clone())
                        .collect();
                    let moved = ids.remove(at);
                    ids.insert(to, moved);
                    for (slot, member) in slots.iter().zip(ids) {
                        inner.stored.members[*slot] = member;
                    }
                    affected.push(job_id);
                }
                Some(queue)
            }
            QueueAction::AckFinish { id } => {
                let q = inner
                    .stored
                    .queue_mut(&id)
                    .ok_or_else(|| not_found("queue"))?;
                q.finish_action_pending = false;
                Some(id)
            }
        };
        self.settle(&mut inner, now, true).await?;
        Ok((queue, affected))
    }

    /// A download was enqueued with a queue label: record its membership and hold it if its queue is not running
    /// or has no free slot. Unknown queues are ignored (the caller validated the label first).
    pub async fn on_enqueued(&self, job_id: JobId, state: JobState, spec: &JobSpec) {
        let Some(queue) = spec.labels.get(QUEUE_LABEL) else {
            return;
        };
        let mut inner = self.inner.lock().await;
        let now = (self.now)();
        if inner.stored.queue(queue).is_none() || inner.stored.members.len() >= MAX_QUEUED_JOBS {
            return;
        }
        // The stored request is what the engine hands the transport later, so fingerprint that one.
        let fp = self
            .port
            .get()
            .and_then(|p| p.fingerprint(job_id))
            .unwrap_or_else(|| fingerprint(spec));
        self.limits.set_member(&fp, Some(queue));
        inner.stored.members.push(Member {
            job_id,
            queue: queue.clone(),
            fingerprint: fp,
            held: false,
            retries_used: 0,
            retry_at_ms: None,
            state: Some(state),
        });
        if let Some(q) = inner.stored.queue_mut(queue)
            && q.running
        {
            q.had_work = true;
        }
        if let Err(e) = self.settle(&mut inner, now, true).await {
            tracing::warn!(code = %e.envelope().code, "queue membership could not be saved");
        }
    }

    fn start(q: &mut StoredQueue, now: i64) {
        q.running = true;
        q.draining = false;
        q.last_started_ms = Some(now.max(0) as u64);
        q.finished_at_ms = None;
        q.finish_action_pending = false;
        q.had_work = false;
    }

    fn network_view(inner: &Inner) -> NetworkView {
        let cost = match inner.stored.defaults.network_mode {
            NetworkMode::Auto => inner.network_cost,
            NetworkMode::Unmetered => NetworkCost::Unmetered,
            NetworkMode::Metered => NetworkCost::Metered,
        };
        NetworkView {
            cost,
            paused: inner.stored.defaults.pause_on_metered
                && (cost == NetworkCost::Metered
                    || (cost == NetworkCost::Unknown && !inner.stored.metered_held.is_empty())),
            held_jobs: inner.stored.metered_held.len() as u32,
        }
    }

    async fn refresh_network(&self) {
        let now = (self.now)();
        {
            let inner = self.inner.lock().await;
            if inner.stored.defaults.network_mode != NetworkMode::Auto
                || inner
                    .network_checked_ms
                    .is_some_and(|last| now.saturating_sub(last) < 30_000)
            {
                return;
            }
        }
        let cost = match self.port.get() {
            Some(port) => port.network_cost().await,
            None => NetworkCost::Unknown,
        };
        let mut inner = self.inner.lock().await;
        inner.network_cost = cost;
        inner.network_checked_ms = Some(now);
    }

    /// Forget automatic ownership when a caller explicitly pauses, cancels or resumes a job.
    /// This is also called for an already-paused job, before the engine's idempotent control.
    pub async fn on_explicit_control(&self, id: JobId, pause: bool) -> ApiResult<()> {
        let mut inner = self.inner.lock().await;
        inner.stored.metered_held.retain(|j| *j != id);
        inner.stored.manual_paused.retain(|j| *j != id);
        if pause {
            inner.stored.manual_paused.push(id);
            if let Some(member) = inner.stored.members.iter_mut().find(|m| m.job_id == id) {
                member.held = false;
            }
        }
        self.save(&mut inner, (self.now)())?;
        Ok(())
    }

    async fn reconcile_network(
        &self,
        inner: &mut Inner,
        port: &dyn JobPort,
        kill: bool,
        blocked: bool,
    ) -> bool {
        if kill {
            // A global stop cannot later be undone by a network-cost change.
            let changed = !inner.stored.metered_held.is_empty();
            inner.stored.metered_held.clear();
            return changed;
        }
        let before = inner.stored.metered_held.len();
        inner.stored.metered_held.retain(|id| {
            port.observe(*id)
                .is_some_and(|o| o.state == JobState::Paused)
        });
        let mut changed = before != inner.stored.metered_held.len();
        inner.stored.manual_paused.retain(|id| {
            port.observe(*id).is_some_and(|o| {
                !matches!(
                    o.state,
                    JobState::Completed | JobState::Canceled | JobState::Failed
                )
            })
        });
        if blocked {
            for (id, state) in port.incomplete() {
                if holdable(state)
                    && !inner.stored.manual_paused.contains(&id)
                    && !inner.stored.metered_held.contains(&id)
                    && port.pause(id).await
                {
                    inner.stored.metered_held.push(id);
                    changed = true;
                }
            }
        } else {
            for id in std::mem::take(&mut inner.stored.metered_held) {
                changed = true;
                let queue_holds = inner.stored.members.iter().any(|m| {
                    m.job_id == id
                        && (m.held
                            || inner
                                .stored
                                .queue(&m.queue)
                                .is_some_and(|q| !q.running || q.draining))
                });
                if !queue_holds
                    && port
                        .observe(id)
                        .is_some_and(|o| o.state == JobState::Paused)
                {
                    let _ = port.resume(id).await;
                }
            }
        }
        changed
    }

    fn watch_completed(stored: &mut Stored, queue: &str, id: JobId, now: i64) {
        if let Some(watch) = stored
            .watches
            .iter_mut()
            .find(|w| w.view.pending_job_id == Some(id))
        {
            watch.view.job_id = id;
            watch.view.pending_job_id = None;
            watch.view.status = SyncStatus::Updated;
            return;
        }
        if let Some(watch) = stored.watches.iter_mut().find(|w| w.view.job_id == id) {
            watch.queue = queue.to_owned();
            return;
        }
        if stored.watches.len() >= MAX_QUEUED_JOBS {
            return;
        }
        stored.watches.push(Watch {
            queue: queue.to_owned(),
            view: SyncFileView {
                job_id: id,
                pending_job_id: None,
                last_checked_ms: None,
                next_check_ms: now.max(0) as u64,
                status: SyncStatus::Waiting,
            },
        });
    }

    fn sync_running(stored: &Stored, queue: &StoredQueue, now: u64) -> bool {
        queue.running
            && !queue.draining
            && queue.def.kind == QueueKind::Sync
            && !queue
                .last_started_ms
                .and_then(|started| {
                    Self::clock(stored).stop_after(&queue.def.schedule, started as i64)
                })
                .is_some_and(|stop| stop <= now as i64)
    }

    async fn sync_tick(&self) -> ApiResult<()> {
        let Some(port) = self.port.get() else {
            return Ok(());
        };
        let now = (self.now)().max(0) as u64;
        let candidate = {
            let inner = self.inner.lock().await;
            if port.kill_switch() || Self::network_view(&inner).paused {
                return Ok(());
            }
            inner
                .stored
                .watches
                .iter()
                .find(|w| {
                    w.view.pending_job_id.is_none()
                        && w.view.next_check_ms <= now
                        && inner
                            .stored
                            .queue(&w.queue)
                            .is_some_and(|q| Self::sync_running(&inner.stored, q, now))
                })
                .cloned()
        };
        let Some(watch) = candidate else {
            return Ok(());
        };
        // One bounded probe per serialized tick; queue controls remain responsive while it runs.
        let check = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            port.sync_probe(watch.view.job_id),
        )
        .await
        .unwrap_or_else(|_| SyncCheck::status(SyncStatus::Failed));
        let mut inner = self.inner.lock().await;
        if port.kill_switch() || Self::network_view(&inner).paused {
            return Ok(());
        }
        let Some(queue) = inner
            .stored
            .queue(&watch.queue)
            .cloned()
            .filter(|q| Self::sync_running(&inner.stored, q, now))
        else {
            return Ok(());
        };
        let Some(index) = inner.stored.watches.iter().position(|w| {
            w.queue == watch.queue
                && w.view.job_id == watch.view.job_id
                && w.view.pending_job_id.is_none()
        }) else {
            return Ok(());
        };
        inner.stored.watches[index].view.last_checked_ms = Some(now);
        inner.stored.watches[index].view.next_check_ms =
            now.saturating_add(u64::from(queue.def.sync_every_hours) * 3_600_000);
        inner.stored.watches[index].view.status = check.status;
        if let Some(version) = check.version {
            if inner.stored.members.len() >= MAX_QUEUED_JOBS {
                inner.stored.watches[index].view.status = SyncStatus::Failed;
            } else if let Some((id, state)) = port
                .sync_enqueue(watch.view.job_id, &watch.queue, &version)
                .await
            {
                inner.stored.watches[index].view.pending_job_id = Some(id);
                let fp = port.fingerprint(id).unwrap_or_default();
                self.limits.set_member(&fp, Some(&watch.queue));
                if !inner.stored.members.iter().any(|m| m.job_id == id) {
                    inner.stored.members.push(Member {
                        job_id: id,
                        queue: watch.queue,
                        fingerprint: fp,
                        held: state == JobState::Paused,
                        retries_used: 0,
                        retry_at_ms: None,
                        state: Some(state),
                    });
                }
            } else {
                inner.stored.watches[index].view.status = SyncStatus::Failed;
            }
        }
        self.save(&mut inner, now as i64)
    }

    // ---------------------------------------------------------------- the scheduler

    /// One scheduler tick: scheduled starts and stops, holding and releasing, retries, finished queues. Returns how
    /// long to wait for the next one: a quarter of a second while a running queue still has downloads to release
    /// (so a finished download is replaced at once), a second otherwise.
    pub async fn tick(&self) -> ApiResult<std::time::Duration> {
        let _tick = self.tick_gate.lock().await;
        self.refresh_network().await;
        self.sync_tick().await?;
        let mut inner = self.inner.lock().await;
        let now = (self.now)();
        self.apply_limiter(&inner.stored, now);
        if !inner.adopted
            && let Some(port) = self.port.get()
        {
            inner.adopted = true;
            for (id, state, spec) in port.labelled() {
                let Some(queue) = spec.labels.get(QUEUE_LABEL) else {
                    continue;
                };
                if inner.stored.queue(queue).is_none()
                    || inner.stored.members.iter().any(|m| m.job_id == id)
                    || inner.stored.members.len() >= MAX_QUEUED_JOBS
                {
                    continue;
                }
                let fp = fingerprint(&spec);
                self.limits.set_member(&fp, Some(queue));
                inner.stored.members.push(Member {
                    job_id: id,
                    queue: queue.clone(),
                    fingerprint: fp,
                    held: false,
                    retries_used: 0,
                    retry_at_ms: None,
                    state: Some(state),
                });
                inner.dirty = true;
            }
        }
        let prev = std::mem::replace(&mut inner.prev_tick_ms, now);
        let changed = self.reconcile(&mut inner, now, Some(prev)).await;
        if changed || inner.dirty || now - inner.last_saved_ms >= CLOCK_SAVE_MS {
            self.save(&mut inner, now)?;
        }
        let stored = &inner.stored;
        let busy = stored.queues.iter().any(|q| {
            q.draining
                || (q.running && stored.members.iter().any(|m| m.queue == q.def.id && m.held))
        });
        Ok(std::time::Duration::from_millis(if busy {
            250
        } else {
            1000
        }))
    }

    /// Reconcile after a change made by an API call (no scheduled starts), then save.
    async fn settle(&self, inner: &mut Inner, now: i64, force_save: bool) -> ApiResult<()> {
        let changed = self.reconcile(inner, now, None).await;
        self.apply_limiter(&inner.stored, now);
        if changed || force_save || inner.dirty {
            self.save(inner, now)?;
        }
        Ok(())
    }

    /// The scheduler's work. `scheduled` carries the previous tick's instant when scheduled starts and stops are to be
    /// considered. Returns whether anything changed.
    async fn reconcile(&self, inner: &mut Inner, now: i64, scheduled: Option<i64>) -> bool {
        let Some(port) = self.port.get().cloned() else {
            return false;
        };
        let mut changed = false;
        let kill = port.kill_switch();
        let blocked = Self::network_view(inner).paused;
        changed |= self
            .reconcile_network(inner, port.as_ref(), kill, blocked)
            .await;
        let clock = Self::clock(&inner.stored);

        // 1. Refresh every member; drop the ones that are done or gone or that someone else started.
        let mut done_queues: Vec<String> = Vec::new();
        let mut index = 0;
        while index < inner.stored.members.len() {
            let id = inner.stored.members[index].job_id;
            let queue_id = inner.stored.members[index].queue.clone();
            let observed = port.observe(id);
            let (retries, delay) = inner
                .stored
                .queue(&queue_id)
                .map_or((0, 30), |q| (q.def.retries, q.def.retry_delay_s));
            if observed
                .as_ref()
                .is_some_and(|o| o.state == JobState::Completed)
                && inner
                    .stored
                    .queue(&queue_id)
                    .is_some_and(|q| q.def.kind == QueueKind::Sync)
            {
                Self::watch_completed(&mut inner.stored, &queue_id, id, now);
            }
            let m = &mut inner.stored.members[index];
            let mut remove = false;
            match &observed {
                None => remove = true,
                Some(o) => {
                    if m.state != Some(o.state) {
                        m.state = Some(o.state);
                        changed = true;
                    }
                    match o.state {
                        JobState::Completed => {
                            remove = true;
                            done_queues.push(queue_id.clone());
                        }
                        JobState::Canceled => remove = true,
                        JobState::Failed => {
                            if o.retryable && m.retries_used < retries {
                                match m.retry_at_ms {
                                    None => {
                                        m.retry_at_ms = Some(now as u64 + u64::from(delay) * 1000);
                                        changed = true;
                                    }
                                    Some(at) if now as u64 >= at && !kill && !blocked => {
                                        m.retry_at_ms = None;
                                        m.retries_used += 1;
                                        changed = true;
                                        if !port.retry(id).await {
                                            remove = true;
                                        }
                                    }
                                    Some(_) => {}
                                }
                            } else {
                                remove = true;
                                done_queues.push(queue_id.clone());
                            }
                        }
                        state if m.held && state != JobState::Paused => {
                            // Someone started a held download by hand: it leaves the queue.
                            remove = true;
                        }
                        _ => {}
                    }
                }
            }
            if remove {
                let gone = inner.stored.members.remove(index);
                for w in &mut inner.stored.watches {
                    if w.view.pending_job_id == Some(gone.job_id) {
                        w.view.pending_job_id = None;
                        w.view.status = SyncStatus::Failed;
                    }
                }
                self.limits.set_member(&gone.fingerprint, None);
                changed = true;
            } else {
                index += 1;
            }
        }
        for id in done_queues {
            if let Some(q) = inner.stored.queue_mut(&id)
                && q.running
            {
                q.had_work = true;
            }
        }

        // 2. Queue by queue.
        let ids: Vec<String> = inner
            .stored
            .queues
            .iter()
            .map(|q| q.def.id.clone())
            .collect();
        for id in ids {
            let Some(q) = inner.stored.queue(&id).cloned() else {
                continue;
            };
            let mut q = q;
            if kill {
                if q.running || q.draining {
                    q.running = false;
                    q.draining = false;
                    changed = true;
                }
                Self::commit_queue(inner, &q);
                continue;
            }
            if let Some(prev) = scheduled {
                if !q.running && clock.start_due(&q.def.schedule, prev, now) {
                    Self::start(&mut q, now);
                    let manual = inner.stored.manual_paused.clone();
                    for m in &mut inner.stored.members {
                        if m.queue == id
                            && !m.held
                            && m.state == Some(JobState::Paused)
                            && !manual.contains(&m.job_id)
                        {
                            m.held = true;
                        }
                    }
                    changed = true;
                }
                if q.running
                    && !q.draining
                    && let Some(started) = q.last_started_ms
                    && let Some(stop) = clock.stop_after(&q.def.schedule, started as i64)
                    && stop <= now
                {
                    if inner.stored.defaults.stop_behaviour == StopBehaviour::FinishCurrent {
                        q.draining = true;
                    } else {
                        q.running = false;
                    }
                    changed = true;
                }
            }
            let members: Vec<usize> = inner
                .stored
                .members
                .iter()
                .enumerate()
                .filter(|(_, m)| m.queue == id)
                .map(|(i, _)| i)
                .collect();
            let mut active = members
                .iter()
                .filter(|&&i| {
                    let m = &inner.stored.members[i];
                    !m.held && m.state.is_some_and(uses_slot)
                })
                .count();
            if q.running && !q.draining && !blocked {
                // Release held members in order while slots are free.
                for &i in &members {
                    if active >= q.def.max_concurrent as usize {
                        break;
                    }
                    let m = &mut inner.stored.members[i];
                    if !m.held {
                        continue;
                    }
                    m.held = false;
                    changed = true;
                    let job = m.job_id;
                    if port.resume(job).await {
                        if let Some(m) = inner.stored.members.get_mut(i) {
                            m.state = Some(JobState::Queued);
                        }
                        active += 1;
                        q.had_work = true;
                    } else if let Some(m) = inner.stored.members.get_mut(i) {
                        // The engine no longer has it paused (finished, canceled, or already running).
                        m.state = port.observe(job).map(|o| o.state);
                        if m.state.is_some_and(uses_slot) {
                            active += 1;
                        }
                    }
                }
                // Members that started on their own beyond the queue's limit wait their turn.
                if active > q.def.max_concurrent as usize {
                    let mut excess = active - q.def.max_concurrent as usize;
                    for &i in members.iter().rev() {
                        if excess == 0 {
                            break;
                        }
                        let (job, state, held) = {
                            let m = &inner.stored.members[i];
                            (m.job_id, m.state, m.held)
                        };
                        if held || !state.is_some_and(holdable) {
                            continue;
                        }
                        if port.pause(job).await {
                            inner.stored.members[i].held = true;
                            inner.stored.members[i].state = Some(JobState::Paused);
                            changed = true;
                        }
                        excess -= 1;
                    }
                }
            } else if !q.running && !q.draining {
                // A queue that is not running holds its members.
                for &i in &members {
                    let (job, state, held) = {
                        let m = &inner.stored.members[i];
                        (m.job_id, m.state, m.held)
                    };
                    if held || !state.is_some_and(holdable) {
                        continue;
                    }
                    if port.pause(job).await {
                        inner.stored.members[i].held = true;
                        inner.stored.members[i].state = Some(JobState::Paused);
                        changed = true;
                    }
                }
            }
            // A draining queue stops once its running members are done.
            if q.draining {
                let still = members.iter().any(|&i| {
                    let m = &inner.stored.members[i];
                    !m.held && m.state.is_some_and(uses_slot)
                });
                if !still {
                    q.draining = false;
                    q.running = false;
                    changed = true;
                }
            }
            // A running queue with no members left, after doing work, has finished.
            if q.running
                && !q.draining
                && members.is_empty()
                && q.had_work
                && !inner.stored.watches.iter().any(|w| w.queue == id)
            {
                q.running = false;
                q.had_work = false;
                q.finished_at_ms = Some(now.max(0) as u64);
                q.finish_action_pending = q.def.on_finish != OnFinish::Nothing;
                if q.def.on_finish == OnFinish::StopLimiter && inner.stored.limiter.enabled {
                    inner.stored.limiter.enabled = false;
                    self.apply_limiter(&inner.stored, now);
                }
                changed = true;
            }
            Self::commit_queue(inner, &q);
        }
        changed
    }

    /// Store a queue's working copy back if it differs.
    fn commit_queue(inner: &mut Inner, working: &StoredQueue) {
        if let Some(slot) = inner.stored.queue_mut(&working.def.id) {
            *slot = working.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use zeus_api::methods::queues::{QueueSchedule, Repeat, Timezone};
    use zeus_types::{HttpUrl, Source, SourceKind};

    /// A scripted engine: jobs with states, and the calls made to them.
    #[derive(Default)]
    struct Fake {
        jobs: StdMutex<HashMap<JobId, (JobState, bool)>>,
        calls: StdMutex<Vec<(&'static str, JobId)>>,
        kill: StdMutex<bool>,
        sync_changed: StdMutex<bool>,
        block_probe: std::sync::atomic::AtomicBool,
        probe_started: tokio::sync::Notify,
        probe_release: tokio::sync::Notify,
    }

    impl Fake {
        fn add(&self, state: JobState) -> JobId {
            let id = zeus_types::IdGenerator::new().job_id();
            self.jobs.lock().unwrap().insert(id, (state, false));
            id
        }
        fn set(&self, id: JobId, state: JobState, retryable: bool) {
            self.jobs.lock().unwrap().insert(id, (state, retryable));
        }
        fn state(&self, id: JobId) -> JobState {
            self.jobs.lock().unwrap()[&id].0
        }
        fn calls(&self, what: &str) -> usize {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.0 == what)
                .count()
        }
    }

    #[async_trait]
    impl JobPort for Fake {
        fn incomplete(&self) -> Vec<(JobId, JobState)> {
            self.jobs
                .lock()
                .unwrap()
                .iter()
                .map(|(id, (state, _))| (*id, *state))
                .collect()
        }
        async fn sync_probe(&self, id: JobId) -> SyncCheck {
            self.calls.lock().unwrap().push(("probe", id));
            if self.block_probe.load(std::sync::atomic::Ordering::SeqCst) {
                self.probe_started.notify_one();
                self.probe_release.notified().await;
            }
            if *self.sync_changed.lock().unwrap() {
                SyncCheck {
                    status: SyncStatus::Downloading,
                    version: Some("version-two".into()),
                }
            } else {
                SyncCheck::status(SyncStatus::Unchanged)
            }
        }
        async fn sync_enqueue(
            &self,
            id: JobId,
            _queue: &str,
            _version: &str,
        ) -> Option<(JobId, JobState)> {
            self.calls.lock().unwrap().push(("sync_enqueue", id));
            Some((self.add(JobState::Queued), JobState::Queued))
        }
        fn observe(&self, id: JobId) -> Option<Observed> {
            self.jobs
                .lock()
                .unwrap()
                .get(&id)
                .map(|(state, retryable)| Observed {
                    state: *state,
                    retryable: *retryable,
                })
        }
        fn fingerprint(&self, id: JobId) -> Option<String> {
            Some(format!("fp-{id}"))
        }
        async fn pause(&self, id: JobId) -> bool {
            self.calls.lock().unwrap().push(("pause", id));
            let mut jobs = self.jobs.lock().unwrap();
            match jobs.get_mut(&id) {
                Some((s, _)) if holdable(*s) => {
                    *s = JobState::Paused;
                    true
                }
                _ => false,
            }
        }
        async fn resume(&self, id: JobId) -> bool {
            self.calls.lock().unwrap().push(("resume", id));
            let mut jobs = self.jobs.lock().unwrap();
            match jobs.get_mut(&id) {
                Some((s, _)) if *s == JobState::Paused => {
                    *s = JobState::Downloading;
                    true
                }
                _ => false,
            }
        }
        async fn retry(&self, id: JobId) -> bool {
            self.calls.lock().unwrap().push(("retry", id));
            let mut jobs = self.jobs.lock().unwrap();
            match jobs.get_mut(&id) {
                Some((s, _)) if *s == JobState::Failed => {
                    *s = JobState::Downloading;
                    true
                }
                _ => false,
            }
        }
        fn kill_switch(&self) -> bool {
            *self.kill.lock().unwrap()
        }
    }

    struct Harness {
        service: Arc<QueueService>,
        fake: Arc<Fake>,
        clock: Arc<std::sync::atomic::AtomicI64>,
        _dir: tempfile::TempDir,
        limits: Arc<Limits>,
    }

    fn all(_: JobId) -> bool {
        true
    }

    fn ms_utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        use chrono::TimeZone;
        chrono::Utc
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    }

    fn harness_in(dir: tempfile::TempDir, start: i64, fake: Arc<Fake>) -> Harness {
        let clock = Arc::new(std::sync::atomic::AtomicI64::new(start));
        let c = clock.clone();
        let limits = Limits::new();
        let service = QueueService::open_with(
            dir.path(),
            limits.clone(),
            Arc::new(move || c.load(std::sync::atomic::Ordering::SeqCst)),
        )
        .unwrap();
        service.attach(fake.clone());
        Harness {
            service,
            fake,
            clock,
            _dir: dir,
            limits,
        }
    }

    async fn harness() -> Harness {
        let h = harness_in(
            tempfile::tempdir().unwrap(),
            ms_utc(2026, 10, 25, 14, 0),
            Arc::new(Fake::default()),
        );
        // Schedules follow UTC in tests, whatever the machine's zone.
        h.service
            .configure(
                None,
                Some(QueueDefaults {
                    timezone: Timezone::Utc,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        h
    }

    impl Harness {
        fn advance(&self, ms: i64) {
            self.clock
                .fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
        }
        async fn view(&self, id: &str) -> QueueView {
            self.service.queue_view(id, &all).await.unwrap()
        }
        async fn enqueue(&self, queue: &str, state: JobState) -> JobId {
            let id = self.fake.add(state);
            let mut spec = JobSpec::new(
                HttpUrl::parse(&format!("https://files.example.org/{id}.bin")).unwrap(),
                Source::new(SourceKind::Agent),
            );
            spec.labels.insert(QUEUE_LABEL.into(), queue.into());
            self.service.on_enqueued(id, state, &spec).await;
            id
        }
        async fn queue(&self, id: &str, max: u32) {
            self.service
                .put(QueueDef {
                    id: id.into(),
                    name: id.into(),
                    max_concurrent: max,
                    ..main_queue()
                })
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn metered_holds_only_its_own_jobs_and_never_releases_a_global_stop() {
        let h = harness().await;
        let active = h.fake.add(JobState::Downloading);
        let manual = h.fake.add(JobState::Paused);
        let explicit = h.fake.add(JobState::Downloading);
        h.service
            .configure(
                None,
                Some(QueueDefaults {
                    network_mode: NetworkMode::Metered,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.state(active), JobState::Paused);
        assert_eq!(h.service.list(&all).await.network.held_jobs, 2);
        h.service.on_explicit_control(explicit, true).await.unwrap();
        h.service
            .configure(None, Some(QueueDefaults::default()))
            .await
            .unwrap();
        h.service.tick().await.unwrap();
        assert_eq!(
            h.service.list(&all).await.network.cost,
            NetworkCost::Unknown
        );
        assert!(h.service.list(&all).await.network.paused);
        assert_eq!(h.fake.state(active), JobState::Paused);

        h.service
            .configure(
                None,
                Some(QueueDefaults {
                    network_mode: NetworkMode::Unmetered,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.state(active), JobState::Downloading);
        assert_eq!(h.fake.state(manual), JobState::Paused);
        assert_eq!(h.fake.state(explicit), JobState::Paused);
        h.service
            .configure(
                None,
                Some(QueueDefaults {
                    network_mode: NetworkMode::Metered,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        h.service.tick().await.unwrap();
        *h.fake.kill.lock().unwrap() = true;
        h.service.tick().await.unwrap();
        *h.fake.kill.lock().unwrap() = false;
        h.service
            .configure(
                None,
                Some(QueueDefaults {
                    network_mode: NetworkMode::Unmetered,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.state(active), JobState::Paused);
    }

    #[tokio::test]
    async fn stopping_during_a_probe_prevents_its_new_download_without_blocking_controls() {
        let h = harness().await;
        h.service
            .put(QueueDef {
                id: "watch".into(),
                kind: QueueKind::Sync,
                ..main_queue()
            })
            .await
            .unwrap();
        let old = h.fake.add(JobState::Completed);
        h.service
            .control(
                QueueAction::Assign {
                    id: "watch".into(),
                    job_ids: vec![old],
                },
                &all,
            )
            .await
            .unwrap();
        h.service
            .control(QueueAction::Start { id: "watch".into() }, &all)
            .await
            .unwrap();
        *h.fake.sync_changed.lock().unwrap() = true;
        h.fake
            .block_probe
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let service = h.service.clone();
        let tick = tokio::spawn(async move {
            service.tick().await.unwrap();
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            h.fake.probe_started.notified(),
        )
        .await
        .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            h.service
                .control(QueueAction::Stop { id: "watch".into() }, &all),
        )
        .await
        .unwrap()
        .unwrap();
        h.fake.probe_release.notify_one();
        tick.await.unwrap();
        assert_eq!(h.fake.calls("sync_enqueue"), 0);
        assert!(!h.view("watch").await.running);
    }

    #[tokio::test]
    async fn sync_watches_are_durable_bounded_and_keep_the_previous_version_on_failure() {
        let h = harness().await;
        h.service
            .put(QueueDef {
                id: "watch".into(),
                kind: QueueKind::Sync,
                sync_every_hours: 1,
                ..main_queue()
            })
            .await
            .unwrap();
        let old = h.fake.add(JobState::Completed);
        h.service
            .control(
                QueueAction::Assign {
                    id: "watch".into(),
                    job_ids: vec![old],
                },
                &all,
            )
            .await
            .unwrap();
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.calls("probe"), 0);
        h.service
            .control(QueueAction::Start { id: "watch".into() }, &all)
            .await
            .unwrap();
        h.service.tick().await.unwrap();
        assert_eq!(
            h.view("watch").await.sync_files[0].status,
            SyncStatus::Unchanged
        );
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.calls("probe"), 1);
        let reopened = QueueService::open(h._dir.path(), Limits::new()).unwrap();
        assert_eq!(
            reopened.queue_view("watch", &all).await.unwrap().sync_files[0].job_id,
            old
        );
        *h.fake.sync_changed.lock().unwrap() = true;
        h.advance(3_600_000);
        h.service.tick().await.unwrap();
        let pending = h.view("watch").await.sync_files[0].pending_job_id.unwrap();
        assert_ne!(pending, old);
        assert_eq!(h.fake.state(old), JobState::Completed);
        h.fake.set(pending, JobState::Failed, false);
        h.service.tick().await.unwrap();
        let watch = h.view("watch").await.sync_files[0].clone();
        assert_eq!(watch.job_id, old);
        assert_eq!(watch.status, SyncStatus::Failed);
        assert!(watch.pending_job_id.is_none());
        h.advance(3_600_000);
        h.service
            .configure(
                None,
                Some(QueueDefaults {
                    network_mode: NetworkMode::Metered,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.calls("probe"), 2);
    }

    #[tokio::test]
    async fn members_of_a_stopped_queue_are_held_and_released_in_order_up_to_the_limit() {
        let h = harness().await;
        h.queue("night", 2).await;
        let a = h.enqueue("night", JobState::Queued).await;
        let b = h.enqueue("night", JobState::Queued).await;
        let c = h.enqueue("night", JobState::Probing).await;
        for id in [a, b, c] {
            assert_eq!(h.fake.state(id), JobState::Paused, "held while stopped");
        }
        let v = h.view("night").await;
        assert_eq!((v.running, v.waiting, v.active), (false, 3, 0));
        assert_eq!(v.order, vec![a, b, c]);
        assert_eq!(v.held_job_ids, vec![a, b, c]);

        h.service
            .control(QueueAction::Start { id: "night".into() }, &all)
            .await
            .unwrap();
        assert_eq!(h.fake.state(a), JobState::Downloading);
        assert_eq!(h.fake.state(b), JobState::Downloading);
        assert_eq!(h.fake.state(c), JobState::Paused, "only two at a time");
        let v = h.view("night").await;
        assert_eq!((v.running, v.waiting, v.active), (true, 1, 2));
        assert_eq!(
            h.service.tick().await.unwrap(),
            std::time::Duration::from_millis(250),
            "a running queue with downloads left to release ticks fast"
        );

        // When one finishes the next is released on the next tick.
        h.fake.set(a, JobState::Completed, false);
        h.advance(1000);
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.state(c), JobState::Downloading);
        let v = h.view("night").await;
        assert_eq!(v.order, vec![b, c]);

        // When everything is done the queue has finished and asks for its on-finish action to be acknowledged.
        h.fake.set(b, JobState::Completed, false);
        h.fake.set(c, JobState::Completed, false);
        h.advance(1000);
        assert_eq!(
            h.service.tick().await.unwrap(),
            std::time::Duration::from_secs(1)
        );
        let v = h.view("night").await;
        assert!(!v.running && v.order.is_empty());
        assert!(v.finished_at_ms.is_some());
        assert!(!v.finish_action_pending, "on_finish is nothing");
    }

    #[tokio::test]
    async fn a_running_queue_caps_members_that_started_on_their_own() {
        let h = harness().await;
        h.queue("busy", 1).await;
        h.service
            .control(QueueAction::Start { id: "busy".into() }, &all)
            .await
            .unwrap();
        let a = h.enqueue("busy", JobState::Probing).await;
        let b = h.enqueue("busy", JobState::Probing).await;
        let c = h.enqueue("busy", JobState::Queued).await;
        assert_eq!(h.fake.state(a), JobState::Probing);
        assert_eq!(h.fake.state(b), JobState::Paused);
        assert_eq!(h.fake.state(c), JobState::Paused);
        let v = h.view("busy").await;
        assert_eq!((v.waiting, v.active), (2, 1));
    }

    #[tokio::test]
    async fn stopping_pauses_running_members_and_a_scheduled_stop_can_let_them_finish() {
        let h = harness().await;
        h.queue("night", 3).await;
        let a = h.enqueue("night", JobState::Queued).await;
        let b = h.enqueue("night", JobState::Queued).await;
        h.service
            .control(QueueAction::Start { id: "night".into() }, &all)
            .await
            .unwrap();
        assert_eq!(h.fake.state(a), JobState::Downloading);
        h.service
            .control(QueueAction::Stop { id: "night".into() }, &all)
            .await
            .unwrap();
        assert_eq!(h.fake.state(a), JobState::Paused);
        assert_eq!(h.fake.state(b), JobState::Paused);
        assert_eq!(h.view("night").await.waiting, 2);

        // A schedule that stops at 15:00 with finish_current leaves the running member alone.
        h.service
            .put(QueueDef {
                schedule: QueueSchedule {
                    start_enabled: false,
                    stop_enabled: true,
                    stop_time: "15:00".into(),
                    ..Default::default()
                },
                ..h.view("night").await.def
            })
            .await
            .unwrap();
        h.service
            .configure(
                None,
                Some(QueueDefaults {
                    timezone: Timezone::Utc,
                    stop_behaviour: StopBehaviour::FinishCurrent,
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        h.service
            .control(QueueAction::Start { id: "night".into() }, &all)
            .await
            .unwrap();
        assert_eq!(h.fake.state(a), JobState::Downloading);
        h.advance(61 * 60_000);
        h.service.tick().await.unwrap();
        let v = h.view("night").await;
        assert!(
            v.running,
            "draining: the queue is still finishing its running downloads"
        );
        assert_eq!(h.fake.state(a), JobState::Downloading);
        h.fake.set(a, JobState::Completed, false);
        h.fake.set(b, JobState::Completed, false);
        h.advance(1000);
        h.service.tick().await.unwrap();
        assert!(!h.view("night").await.running);
    }

    #[tokio::test]
    async fn a_schedule_starts_the_queue_on_its_day_and_time() {
        let h = harness().await;
        h.service
            .put(QueueDef {
                schedule: QueueSchedule {
                    start_enabled: true,
                    start_time: "23:00".into(),
                    repeat: Repeat::Days,
                    days: vec![0, 1, 2, 3, 4, 5, 6],
                    stop_enabled: true,
                    stop_time: "06:00".into(),
                    ..Default::default()
                },
                ..main_queue()
            })
            .await
            .unwrap();
        let a = h.enqueue(MAIN_QUEUE, JobState::Queued).await;
        assert_eq!(h.fake.state(a), JobState::Paused);
        let v = h.view(MAIN_QUEUE).await;
        assert_eq!(v.next_start_ms, Some(ms_utc(2026, 10, 25, 23, 0) as u64));
        // Nothing happens before 23:00.
        h.advance(8 * 3_600_000);
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.state(a), JobState::Paused);
        // At 23:00 it starts.
        h.advance(60 * 60_000 + 1000);
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.state(a), JobState::Downloading);
        let v = h.view(MAIN_QUEUE).await;
        assert!(v.running);
        assert_eq!(v.next_stop_ms, Some(ms_utc(2026, 10, 26, 6, 0) as u64));
        // Past 06:00 the scheduled stop pauses what is left.
        h.advance(7 * 3_600_000);
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.state(a), JobState::Paused);
        assert!(!h.view(MAIN_QUEUE).await.running);
    }

    #[tokio::test]
    async fn the_kill_switch_stops_queues_and_blocks_starting_them() {
        let h = harness().await;
        h.queue("night", 2).await;
        let a = h.enqueue("night", JobState::Queued).await;
        h.service
            .control(QueueAction::Start { id: "night".into() }, &all)
            .await
            .unwrap();
        assert_eq!(h.fake.state(a), JobState::Downloading);
        *h.fake.kill.lock().unwrap() = true;
        h.advance(1000);
        h.service.tick().await.unwrap();
        assert!(!h.view("night").await.running, "a kill stops queues");
        let err = h
            .service
            .control(QueueAction::Start { id: "night".into() }, &all)
            .await
            .unwrap_err();
        assert_eq!(err.envelope().code.as_str(), "policy.kill_switch_engaged");
        // Even a schedule that falls due does not start it.
        h.service
            .put(QueueDef {
                id: "night".into(),
                name: "night".into(),
                schedule: QueueSchedule {
                    start_enabled: true,
                    start_time: "14:01".into(),
                    days: vec![0, 1, 2, 3, 4, 5, 6],
                    ..Default::default()
                },
                ..main_queue()
            })
            .await
            .unwrap();
        h.advance(120_000);
        h.service.tick().await.unwrap();
        assert!(!h.view("night").await.running);
    }

    #[tokio::test]
    async fn retryable_failures_are_retried_with_a_delay_up_to_the_queue_setting() {
        let h = harness().await;
        h.service
            .put(QueueDef {
                id: "flaky".into(),
                name: "flaky".into(),
                retries: 2,
                retry_delay_s: 10,
                ..main_queue()
            })
            .await
            .unwrap();
        h.service
            .control(QueueAction::Start { id: "flaky".into() }, &all)
            .await
            .unwrap();
        let a = h.enqueue("flaky", JobState::Downloading).await;
        h.fake.set(a, JobState::Failed, true);
        h.advance(1000);
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.calls("retry"), 0, "waits for the delay");
        h.advance(10_000);
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.calls("retry"), 1);
        assert_eq!(h.fake.state(a), JobState::Downloading);
        // Fails again, retried a second time, then given up.
        h.fake.set(a, JobState::Failed, true);
        h.advance(1000);
        h.service.tick().await.unwrap();
        h.advance(10_000);
        h.service.tick().await.unwrap();
        assert_eq!(h.fake.calls("retry"), 2);
        h.fake.set(a, JobState::Failed, true);
        h.advance(1000);
        h.service.tick().await.unwrap();
        assert!(
            h.view("flaky").await.order.is_empty(),
            "retries are used up"
        );
        // A failure that is not retryable leaves the queue at once.
        let b = h.enqueue("flaky", JobState::Downloading).await;
        h.fake.set(b, JobState::Failed, false);
        h.advance(1000);
        h.service.tick().await.unwrap();
        assert!(h.view("flaky").await.order.is_empty());
        assert_eq!(h.fake.calls("retry"), 2);
    }

    #[tokio::test]
    async fn assigning_moving_and_unassigning_change_the_order() {
        let h = harness().await;
        h.queue("night", 1).await;
        let a = h.fake.add(JobState::Queued);
        let b = h.fake.add(JobState::Paused);
        let c = h.fake.add(JobState::Queued);
        h.service
            .control(
                QueueAction::Assign {
                    id: "night".into(),
                    job_ids: vec![a, b, c],
                },
                &all,
            )
            .await
            .unwrap();
        assert_eq!(h.view("night").await.order, vec![a, b, c]);
        assert_eq!(
            h.fake.state(a),
            JobState::Paused,
            "an assigned running job is held"
        );
        h.service
            .control(
                QueueAction::Move {
                    job_id: c,
                    offset: -2,
                },
                &all,
            )
            .await
            .unwrap();
        assert_eq!(h.view("night").await.order, vec![c, a, b]);
        h.service
            .control(
                QueueAction::Move {
                    job_id: c,
                    offset: 99,
                },
                &all,
            )
            .await
            .unwrap();
        assert_eq!(h.view("night").await.order, vec![a, b, c]);
        h.service
            .control(QueueAction::Unassign { job_ids: vec![b] }, &all)
            .await
            .unwrap();
        assert_eq!(h.view("night").await.order, vec![a, c]);
        // Visibility: a caller who may not see a job cannot name it.
        let only_a = |id: JobId| id == a;
        assert!(
            h.service
                .control(QueueAction::Unassign { job_ids: vec![c] }, &only_a)
                .await
                .is_err()
        );
        assert_eq!(
            h.service.queue_view("night", &only_a).await.unwrap().order,
            vec![a]
        );
    }

    #[tokio::test]
    async fn a_download_started_by_hand_leaves_its_queue() {
        let h = harness().await;
        h.queue("night", 1).await;
        let a = h.enqueue("night", JobState::Queued).await;
        assert_eq!(h.fake.state(a), JobState::Paused);
        // Someone resumes the held download directly.
        h.fake.set(a, JobState::Downloading, false);
        h.advance(1000);
        h.service.tick().await.unwrap();
        assert!(h.view("night").await.order.is_empty());
        assert_eq!(h.fake.state(a), JobState::Downloading, "it is left alone");
    }

    #[tokio::test]
    async fn queues_membership_and_held_state_survive_a_restart() {
        let h = harness().await;
        h.queue("night", 2).await;
        let a = h.enqueue("night", JobState::Queued).await;
        let b = h.enqueue("night", JobState::Queued).await;
        h.service
            .configure(
                Some(LimiterDef {
                    enabled: true,
                    limit_kib_s: 256,
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap();
        let dir = h._dir.path().to_path_buf();
        let fake = h.fake.clone();
        let start = h.clock.load(std::sync::atomic::Ordering::SeqCst);
        drop(h.service);
        // Reopen over the same directory.
        let limits = Limits::new();
        let service =
            QueueService::open_with(&dir, limits.clone(), Arc::new(move || start + 5000)).unwrap();
        service.attach(fake.clone());
        let v = service.queue_view("night", &all).await.unwrap();
        assert_eq!(v.order, vec![a, b]);
        assert_eq!(v.held_job_ids, vec![a, b]);
        assert_eq!(v.def.max_concurrent, 2);
        assert!(!v.running);
        assert_eq!(
            limits.global_rate(),
            Some(256 * 1024),
            "the limiter is applied again"
        );
        // The scheduler continues: starting releases the held members.
        service
            .control(QueueAction::Start { id: "night".into() }, &all)
            .await
            .unwrap();
        assert_eq!(fake.state(a), JobState::Downloading);
        assert_eq!(fake.state(b), JobState::Downloading);
    }

    #[tokio::test]
    async fn the_limiter_follows_its_window_and_a_finished_queue_can_switch_it_off() {
        let h = harness().await;
        h.service
            .configure(
                Some(LimiterDef {
                    enabled: true,
                    limit_kib_s: 100,
                    schedule_enabled: true,
                    from: "15:00".into(),
                    to: "16:00".into(),
                    days: vec![0, 1, 2, 3, 4, 5, 6],
                }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(h.limits.global_rate(), None, "14:00 is outside the window");
        h.advance(61 * 60_000);
        h.service.tick().await.unwrap();
        assert_eq!(h.limits.global_rate(), Some(100 * 1024));
        h.advance(60 * 60_000);
        h.service.tick().await.unwrap();
        assert_eq!(h.limits.global_rate(), None);

        // stop_limiter: the queue turns the limiter off when it finishes.
        h.service
            .put(QueueDef {
                id: "once".into(),
                name: "once".into(),
                on_finish: OnFinish::StopLimiter,
                ..main_queue()
            })
            .await
            .unwrap();
        h.service
            .configure(
                Some(LimiterDef {
                    enabled: true,
                    limit_kib_s: 100,
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(h.limits.global_rate(), Some(100 * 1024));
        h.service
            .control(QueueAction::Start { id: "once".into() }, &all)
            .await
            .unwrap();
        let a = h.enqueue("once", JobState::Queued).await;
        h.fake.set(a, JobState::Completed, false);
        h.advance(1000);
        h.service.tick().await.unwrap();
        assert_eq!(h.limits.global_rate(), None, "the limiter was switched off");
        let listing = h.service.list(&all).await;
        assert!(!listing.limiter.def.enabled);
        assert!(h.view("once").await.finish_action_pending);
        h.service
            .control(QueueAction::AckFinish { id: "once".into() }, &all)
            .await
            .unwrap();
        assert!(!h.view("once").await.finish_action_pending);
    }

    #[tokio::test]
    async fn validation_and_deletion_rules() {
        let h = harness().await;
        let bad = |f: &dyn Fn(&mut QueueDef)| {
            let mut def = main_queue();
            def.id = "x".into();
            f(&mut def);
            validate_def(&def).is_err()
        };
        assert!(bad(&|d| d.id = "Has Space".into()));
        assert!(bad(&|d| d.id = String::new()));
        assert!(bad(&|d| d.max_concurrent = 0));
        assert!(bad(&|d| d.max_concurrent = 17));
        assert!(bad(&|d| d.retries = 51));
        assert!(bad(&|d| d.retry_delay_s = 0));
        assert!(bad(&|d| d.limit_kib_s = Some(0)));
        assert!(bad(&|d| d.schedule.start_time = "25:00".into()));
        assert!(bad(&|d| d.schedule.days = vec![9]));
        assert!(bad(&|d| {
            d.schedule.start_enabled = true;
            d.schedule.repeat = Repeat::Once;
            d.schedule.start_date = "soon".into();
        }));
        assert!(!bad(&|_| {}));
        assert!(h.service.delete(MAIN_QUEUE).await.is_err());
        assert!(h.service.delete("ghost").await.is_err());
        h.queue("night", 1).await;
        let a = h.enqueue("night", JobState::Queued).await;
        h.service
            .configure(
                None,
                Some(QueueDefaults {
                    timezone: Timezone::Utc,
                    default_queue: "night".into(),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        let released = h.service.delete("night").await.unwrap();
        assert_eq!(released, vec![a]);
        assert_eq!(
            h.fake.state(a),
            JobState::Paused,
            "a held download stays paused"
        );
        let listing = h.service.list(&all).await;
        assert_eq!(listing.queues.len(), 1);
        assert_eq!(listing.defaults.default_queue, MAIN_QUEUE);
        assert!(
            h.service
                .configure(
                    None,
                    Some(QueueDefaults {
                        default_queue: "ghost".into(),
                        ..Default::default()
                    })
                )
                .await
                .is_err()
        );
    }

    #[test]
    fn a_damaged_store_is_set_aside_instead_of_stopping_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("queues.json"), b"{ not json").unwrap();
        let service = QueueService::open(dir.path(), Limits::new()).unwrap();
        assert!(dir.path().join("queues.json.damaged").exists());
        drop(service);
    }
}
