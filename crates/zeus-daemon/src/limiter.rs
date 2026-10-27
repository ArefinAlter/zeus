//! The speed limiter: token buckets behind a throttling [`HttpTransport`].
//!
//! Every download's response body passes through [`ThrottledTransport`], which asks the global bucket and the bucket
//! of the job's queue how long it must wait before releasing each piece of the body, and sleeps that long. The
//! effective rate is therefore the lower of the two. A piece is released only after its wait, so pausing or
//! cancelling a job (which drops the body stream) takes effect at once, even in the middle of a long wait.
//!
//! The buckets are shared by all jobs, so the rate is shared between concurrent transfers in arrival order. Settings
//! change live: the queue service calls [`Limits::set_global`] and [`Limits::set_queue`] and the next piece uses the
//! new rate. A bucket holds at most [`BURST`] of its rate, so a short pause does not allow a long burst and a
//! measured transfer matches the limit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use zeus_http::{HttpRequest, HttpResponse, HttpTransport, ResponseBody, TransportError};
use zeus_types::JobSpec;

use zeus_api::methods::queues::QUEUE_LABEL;

/// How much of its rate a bucket may save up while idle.
pub const BURST: Duration = Duration::from_millis(100);

/// The smallest and largest piece a limited body is released in.
const MIN_PIECE: u64 = 1024;
const MAX_PIECE: u64 = 64 * 1024;

#[derive(Debug)]
struct BucketState {
    /// Bytes per second; `None` means unlimited.
    rate: Option<u64>,
    /// Bytes available now; negative while callers are waiting for tokens they already reserved.
    tokens: f64,
    last: Instant,
}

/// A token bucket whose rate can change at any time.
#[derive(Debug)]
pub struct Bucket {
    state: Mutex<BucketState>,
}

impl Bucket {
    /// A bucket limited to `rate` bytes per second (`None`: unlimited).
    pub fn new(rate: Option<u64>) -> Self {
        Self::new_at(rate, Instant::now())
    }

    /// A bucket whose clock starts at `now`, for tests that drive time themselves.
    pub fn new_at(rate: Option<u64>, now: Instant) -> Self {
        Self {
            state: Mutex::new(BucketState {
                rate: rate.filter(|r| *r > 0),
                tokens: 0.0,
                last: now,
            }),
        }
    }

    /// Change the rate. Saved-up tokens and debts carry over.
    pub fn set_rate(&self, rate: Option<u64>) {
        if let Ok(mut s) = self.state.lock() {
            let rate = rate.filter(|r| *r > 0);
            if s.rate != rate {
                s.last = Instant::now();
                if rate.is_none() {
                    s.tokens = 0.0;
                }
                s.rate = rate;
            }
        }
    }

    /// The current rate in bytes per second.
    pub fn rate(&self) -> Option<u64> {
        self.state.lock().ok().and_then(|s| s.rate)
    }

    /// Take `bytes` from the bucket at `now` and return how long the caller must wait before using them. The debt is
    /// recorded at once, so concurrent callers queue behind each other.
    pub fn reserve(&self, bytes: u64, now: Instant) -> Duration {
        let Ok(mut s) = self.state.lock() else {
            return Duration::ZERO;
        };
        let Some(rate) = s.rate else {
            s.last = now;
            s.tokens = 0.0;
            return Duration::ZERO;
        };
        let rate = rate as f64;
        let elapsed = now.saturating_duration_since(s.last).as_secs_f64();
        s.tokens = (s.tokens + elapsed * rate).min(rate * BURST.as_secs_f64());
        s.last = now;
        s.tokens -= bytes as f64;
        if s.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-s.tokens / rate)
        }
    }
}

/// The identity a job has for limiting: its queue label, or the fingerprint of what it downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    /// `zeus.queue` label, when the job was enqueued with one.
    pub label: Option<String>,
    /// Fingerprint of the request, for jobs assigned to a queue later.
    pub fingerprint: String,
}

/// A stable fingerprint of what a job downloads and where it goes. It never stores or reveals the address: only a
/// hash of it.
pub fn fingerprint(spec: &JobSpec) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"zeus-queue-fingerprint-v1\0");
    hasher.update(spec.url.expose().as_bytes());
    hasher.update(b"\0");
    hasher.update(spec.dest_dir.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"\0");
    hasher.update(spec.filename.as_deref().unwrap_or("").as_bytes());
    hasher.finalize().to_hex().to_string()
}

impl Membership {
    /// The membership of a job request.
    pub fn of(spec: &JobSpec) -> Self {
        Self {
            label: spec.labels.get(QUEUE_LABEL).cloned(),
            fingerprint: fingerprint(spec),
        }
    }
}

/// The global bucket, the per-queue buckets and which job belongs to which queue.
#[derive(Debug, Default)]
pub struct Limits {
    global: Bucket,
    queues: RwLock<HashMap<String, Arc<Bucket>>>,
    members: RwLock<HashMap<String, String>>,
}

impl Default for Bucket {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Limits {
    /// A set of buckets with no limits.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The global limit in KiB/s (`None`: unlimited).
    pub fn set_global(&self, kib_s: Option<u32>) {
        self.global.set_rate(kib_s.map(|k| u64::from(k) * 1024));
    }

    /// The global rate in bytes per second.
    pub fn global_rate(&self) -> Option<u64> {
        self.global.rate()
    }

    /// A queue's limit in KiB/s (`None`: unlimited).
    pub fn set_queue(&self, queue: &str, kib_s: Option<u32>) {
        let rate = kib_s.map(|k| u64::from(k) * 1024);
        if let Ok(mut queues) = self.queues.write() {
            match queues.get(queue) {
                Some(bucket) => bucket.set_rate(rate),
                None => {
                    queues.insert(queue.to_owned(), Arc::new(Bucket::new(rate)));
                }
            }
        }
    }

    /// Forget a deleted queue.
    pub fn remove_queue(&self, queue: &str) {
        if let Ok(mut queues) = self.queues.write() {
            queues.remove(queue);
        }
    }

    /// The queue's rate in bytes per second.
    pub fn queue_rate(&self, queue: &str) -> Option<u64> {
        self.queues
            .read()
            .ok()
            .and_then(|q| q.get(queue).and_then(|b| b.rate()))
    }

    /// Record (or with `None` forget) which queue the job with this fingerprint belongs to.
    pub fn set_member(&self, fingerprint: &str, queue: Option<&str>) {
        if let Ok(mut members) = self.members.write() {
            match queue {
                Some(q) => {
                    members.insert(fingerprint.to_owned(), q.to_owned());
                }
                None => {
                    members.remove(fingerprint);
                }
            }
        }
    }

    /// The queue a job belongs to right now: the store's record first (assignment can move a job), then its label.
    pub fn queue_of(&self, membership: &Membership) -> Option<String> {
        self.members
            .read()
            .ok()
            .and_then(|m| m.get(&membership.fingerprint).cloned())
            .or_else(|| membership.label.clone())
    }

    /// How long to wait before releasing `bytes` of a job in `queue`, and the piece size to release them in.
    pub fn reserve(&self, queue: Option<&str>, bytes: u64, now: Instant) -> Duration {
        let mut wait = self.global.reserve(bytes, now);
        if let Some(bucket) = queue.and_then(|q| self.queues.read().ok()?.get(q).cloned()) {
            wait = wait.max(bucket.reserve(bytes, now));
        }
        wait
    }

    /// The tighter rate that applies to a job in `queue`, if any.
    pub fn effective_rate(&self, queue: Option<&str>) -> Option<u64> {
        let queue_rate = queue.and_then(|q| self.queue_rate(q));
        match (self.global.rate(), queue_rate) {
            (Some(g), Some(q)) => Some(g.min(q)),
            (g, q) => g.or(q),
        }
    }
}

/// The body of a response, released in paced pieces.
fn throttle(body: ResponseBody, limits: Arc<Limits>, membership: Membership) -> ResponseBody {
    struct Paced {
        body: ResponseBody,
        pending: Bytes,
        limits: Arc<Limits>,
        membership: Membership,
        failed: bool,
    }
    let state = Paced {
        body,
        pending: Bytes::new(),
        limits,
        membership,
        failed: false,
    };
    Box::pin(futures::stream::unfold(state, |mut s| async move {
        if s.failed {
            return None;
        }
        if s.pending.is_empty() {
            match s.body.next().await {
                None => return None,
                Some(Err(e)) => {
                    s.failed = true;
                    return Some((Err(e), s));
                }
                Some(Ok(chunk)) => s.pending = chunk,
            }
        }
        let queue = s.limits.queue_of(&s.membership);
        let Some(rate) = s.limits.effective_rate(queue.as_deref()) else {
            let all = std::mem::take(&mut s.pending);
            return Some((Ok(all), s));
        };
        // About 50 ms of data per piece keeps the flow smooth at any rate.
        let piece = (rate / 20).clamp(MIN_PIECE, MAX_PIECE) as usize;
        let take = piece.min(s.pending.len());
        let out = s.pending.split_to(take);
        let wait = s
            .limits
            .reserve(queue.as_deref(), out.len() as u64, Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        Some((Ok(out), s))
    }))
}

/// A transport that paces the bodies of the responses of another transport.
pub struct ThrottledTransport {
    inner: Arc<dyn HttpTransport>,
    limits: Arc<Limits>,
    membership: Membership,
}

impl ThrottledTransport {
    /// Wrap `inner` for one job.
    pub fn new(inner: Arc<dyn HttpTransport>, limits: Arc<Limits>, membership: Membership) -> Self {
        Self {
            inner,
            limits,
            membership,
        }
    }
}

#[async_trait]
impl HttpTransport for ThrottledTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut response = self.inner.send(request).await?;
        let body = std::mem::replace(&mut response.body, Box::pin(futures::stream::empty()));
        response.body = throttle(body, self.limits.clone(), self.membership.clone());
        Ok(response)
    }
}

/// Installs the limiter on every job: wraps whatever transport the inner factory chose (the workflow network for
/// workflow jobs), or the default one for ordinary downloads.
pub struct LimitedFactory {
    inner: Arc<dyn zeus_engine::JobTransportFactory>,
    default: Arc<dyn HttpTransport>,
    limits: Arc<Limits>,
}

impl LimitedFactory {
    /// Compose the limiter with `inner`; `default` carries ordinary downloads.
    pub fn new(
        inner: Arc<dyn zeus_engine::JobTransportFactory>,
        default: Arc<dyn HttpTransport>,
        limits: Arc<Limits>,
    ) -> Self {
        Self {
            inner,
            default,
            limits,
        }
    }
}

impl zeus_engine::JobTransportFactory for LimitedFactory {
    fn for_job(
        &self,
        spec: &JobSpec,
    ) -> Result<Option<Arc<dyn HttpTransport>>, zeus_types::ErrorEnvelope> {
        let base = self
            .inner
            .for_job(spec)?
            .unwrap_or_else(|| self.default.clone());
        Ok(Some(Arc::new(ThrottledTransport::new(
            base,
            self.limits.clone(),
            Membership::of(spec),
        ))))
    }

    fn validate_commit(
        &self,
        spec: &JobSpec,
        txn: &dyn zeus_journal::ReadTxn,
        range: zeus_types::ByteRange,
    ) -> Result<(), zeus_types::ErrorEnvelope> {
        self.inner.validate_commit(spec, txn, range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlimited_bucket_never_waits() {
        let bucket = Bucket::new(None);
        assert_eq!(bucket.reserve(10_000_000, Instant::now()), Duration::ZERO);
    }

    #[test]
    fn a_limited_bucket_charges_debt_in_arrival_order() {
        let t0 = Instant::now();
        let bucket = Bucket::new_at(Some(1000), t0);
        // Nothing is saved up at the start: 500 bytes cost half a second.
        let first = bucket.reserve(500, t0);
        assert!((first.as_secs_f64() - 0.5).abs() < 1e-6, "{first:?}");
        // A second caller at the same instant waits behind the first.
        let second = bucket.reserve(500, t0);
        assert!((second.as_secs_f64() - 1.0).abs() < 1e-6, "{second:?}");
        // Time passing pays the debt back.
        let later = bucket.reserve(0, t0 + Duration::from_secs(1));
        assert!(later.is_zero(), "{later:?}");
    }

    #[test]
    fn idle_time_saves_at_most_the_burst() {
        let t0 = Instant::now();
        let bucket = Bucket::new_at(Some(10_000), t0);
        // After a long idle period only 100 ms of rate (1000 bytes) is available.
        assert!(bucket.reserve(1000, t0 + Duration::from_secs(60)).is_zero());
        let wait = bucket.reserve(1000, t0 + Duration::from_secs(60));
        assert!((wait.as_secs_f64() - 0.1).abs() < 1e-6, "{wait:?}");
    }

    #[test]
    fn the_rate_changes_live_and_the_tighter_limit_wins() {
        let limits = Limits::new();
        assert_eq!(limits.effective_rate(Some("night")), None);
        limits.set_global(Some(1000));
        limits.set_queue("night", Some(100));
        assert_eq!(limits.effective_rate(Some("night")), Some(100 * 1024));
        assert_eq!(limits.effective_rate(Some("other")), Some(1000 * 1024));
        assert_eq!(limits.effective_rate(None), Some(1000 * 1024));
        limits.set_global(None);
        assert_eq!(limits.effective_rate(None), None);
        limits.remove_queue("night");
        assert_eq!(limits.effective_rate(Some("night")), None);
    }

    #[test]
    fn membership_prefers_the_stored_assignment_over_the_label() {
        let limits = Limits::new();
        let m = Membership {
            label: Some("a".into()),
            fingerprint: "fp".into(),
        };
        assert_eq!(limits.queue_of(&m).as_deref(), Some("a"));
        limits.set_member("fp", Some("b"));
        assert_eq!(limits.queue_of(&m).as_deref(), Some("b"));
        limits.set_member("fp", None);
        assert_eq!(limits.queue_of(&m).as_deref(), Some("a"));
    }
}
