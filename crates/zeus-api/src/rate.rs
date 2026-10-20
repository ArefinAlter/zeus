//! Per-credential token buckets (05 section 13.2), applied by the router at authorisation step 8.
//!
//! All connections of one credential share its buckets, so an unattended client cannot multiply its budget by
//! opening connections, and each service credential has buckets of its own. A bucket holds twice its rate per
//! second (or per minute) and refills continuously. `jobs.enqueue` costs one `mutating` token per job, and each
//! job naming a `session_origin` also costs a `session_request` token. The `decide` and `admin` buckets of 13.2 are
//! applied by the daemon, which knows when a decision is an approval and which calls are credential changes.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{ApiError, codes};
use crate::policy::MethodSpec;

/// One bucket's rate: `tokens` per `per`, with a burst of twice that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    /// Tokens added per period.
    pub tokens: u32,
    /// The period.
    pub per: Duration,
}

impl Rate {
    const fn per_second(tokens: u32) -> Self {
        Self {
            tokens,
            per: Duration::from_secs(1),
        }
    }
    const fn per_minute(tokens: u32) -> Self {
        Self {
            tokens,
            per: Duration::from_secs(60),
        }
    }
}

/// The router-applied buckets and their rates. Defaults are those of 05 section 13.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimits {
    /// Every mutating method; `jobs.enqueue` costs one token per job.
    pub mutating: Rate,
    /// Every other method except `jobs.wait` and the handshake.
    pub read: Rate,
    /// `jobs.probe`, `jobs.verify`, `audit.verify`.
    pub probe: Rate,
    /// `sessions.request`, and enqueued jobs that name a `session_origin`.
    pub session_request: Rate,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            mutating: Rate::per_second(50),
            read: Rate::per_second(200),
            probe: Rate::per_second(10),
            session_request: Rate::per_minute(10),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    level: f64,
    at: Instant,
}

impl Bucket {
    fn take(&mut self, rate: Rate, cost: u32, now: Instant) -> Result<(), Duration> {
        let capacity = f64::from(rate.tokens) * 2.0;
        let per_token = rate.per.as_secs_f64() / f64::from(rate.tokens.max(1));
        let refill = now.saturating_duration_since(self.at).as_secs_f64() / per_token;
        self.level = (self.level + refill).min(capacity);
        self.at = now;
        let cost = f64::from(cost);
        if cost > capacity {
            // A single request larger than the burst can never pass; report the longest honest wait.
            return Err(Duration::from_secs_f64(capacity * per_token));
        }
        if self.level >= cost {
            self.level -= cost;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((cost - self.level) * per_token))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Mutating,
    Read,
    Probe,
    SessionRequest,
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Self::Mutating => "mutating",
            Self::Read => "read",
            Self::Probe => "probe",
            Self::SessionRequest => "session_request",
        }
    }
}

/// Buckets of every credential seen, keyed by credential id. Bounded: idle entries are dropped first.
#[derive(Debug, Default)]
pub struct RateLimiter {
    limits: RateLimits,
    buckets: Mutex<HashMap<(String, Kind), Bucket>>,
}

/// Entries kept before idle buckets are pruned.
const MAX_ENTRIES: usize = 4096;

impl RateLimiter {
    /// A limiter with these rates.
    pub fn new(limits: RateLimits) -> Self {
        Self {
            limits,
            buckets: Mutex::default(),
        }
    }

    fn rate(&self, kind: Kind) -> Rate {
        match kind {
            Kind::Mutating => self.limits.mutating,
            Kind::Read => self.limits.read,
            Kind::Probe => self.limits.probe,
            Kind::SessionRequest => self.limits.session_request,
        }
    }

    /// What one call costs, by bucket.
    fn costs(spec: &MethodSpec, params: &Value) -> Vec<(Kind, u32)> {
        let mut costs = Vec::new();
        match spec.name {
            "jobs.wait" => {}
            "jobs.enqueue" => {
                let jobs = params.get("jobs").and_then(Value::as_array);
                let count = jobs.map_or(1, |j| j.len().clamp(1, 100) as u32);
                costs.push((Kind::Mutating, count));
                let sessions = jobs.map_or(0, |j| {
                    j.iter()
                        .filter(|job| job.get("session_origin").is_some_and(|o| !o.is_null()))
                        .count() as u32
                });
                if sessions > 0
                    && !params
                        .get("dry_run")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                {
                    costs.push((Kind::SessionRequest, sessions));
                }
            }
            "sessions.request" => {
                costs.push((Kind::Mutating, 1));
                costs.push((Kind::SessionRequest, 1));
            }
            "jobs.probe" | "jobs.verify" | "audit.verify" => costs.push((Kind::Probe, 1)),
            _ if spec.mutating => costs.push((Kind::Mutating, 1)),
            _ => costs.push((Kind::Read, 1)),
        }
        costs
    }

    /// Charge one call to `credential` at `now`, or refuse it with `api.rate_limited` without charging anything.
    pub fn check_at(
        &self,
        credential: &str,
        spec: &MethodSpec,
        params: &Value,
        now: Instant,
    ) -> Result<(), ApiError> {
        let costs = Self::costs(spec, params);
        let mut buckets = self
            .buckets
            .lock()
            .map_err(|_| ApiError::internal("rate limiter unavailable"))?;
        if buckets.len() >= MAX_ENTRIES {
            buckets.retain(|_, b| now.saturating_duration_since(b.at) < Duration::from_secs(120));
            if buckets.len() >= MAX_ENTRIES {
                buckets.clear();
            }
        }
        // Check every bucket on a copy first, so a refused call charges none of them.
        let mut updated = Vec::new();
        for (kind, cost) in costs {
            let rate = self.rate(kind);
            let key = (credential.to_owned(), kind);
            let mut bucket = buckets.get(&key).copied().unwrap_or(Bucket {
                level: f64::from(rate.tokens) * 2.0,
                at: now,
            });
            if let Err(wait) = bucket.take(rate, cost, now) {
                return Err(
                    ApiError::new(codes::RATE_LIMITED, "too many calls; retry later")
                        .with_detail("limit", kind.name())
                        .with_detail(
                            "retry_after_ms",
                            u64::try_from(wait.as_millis()).unwrap_or(u64::MAX).max(1),
                        ),
                );
            }
            updated.push((key, bucket));
        }
        for (key, bucket) in updated {
            buckets.insert(key, bucket);
        }
        Ok(())
    }

    /// Charge one call now.
    pub fn check(
        &self,
        credential: &str,
        spec: &MethodSpec,
        params: &Value,
    ) -> Result<(), ApiError> {
        self.check_at(credential, spec, params, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::lookup;

    #[test]
    fn mutating_calls_share_a_per_credential_burst_and_refill() {
        let limiter = RateLimiter::default();
        let start = Instant::now();
        let control = lookup("jobs.control").unwrap();
        for _ in 0..100 {
            limiter.check_at("a", control, &Value::Null, start).unwrap();
        }
        let refused = limiter
            .check_at("a", control, &Value::Null, start)
            .unwrap_err();
        let details = refused.envelope().details.clone().unwrap();
        assert_eq!(refused.envelope().code.as_str(), codes::RATE_LIMITED);
        assert_eq!(details["limit"], "mutating");
        assert_eq!(details["retry_after_ms"], 20);
        // Another credential has its own bucket; time refills the first.
        limiter.check_at("b", control, &Value::Null, start).unwrap();
        limiter
            .check_at(
                "a",
                control,
                &Value::Null,
                start + Duration::from_millis(40),
            )
            .unwrap();
    }

    #[test]
    fn enqueue_costs_one_token_per_job_and_session_jobs_spend_the_session_bucket() {
        let limiter = RateLimiter::default();
        let now = Instant::now();
        let enqueue = lookup("jobs.enqueue").unwrap();
        let jobs: Vec<_> = (0..20)
            .map(|_| serde_json::json!({"url":"https://a.example/x","session_origin":"https://a.example"}))
            .collect();
        limiter
            .check_at(
                "a",
                enqueue,
                &serde_json::json!({ "jobs": jobs[..20] }),
                now,
            )
            .unwrap();
        // The 10-per-minute session bucket (burst 20) is now empty, and refusing charged nothing.
        let refused = limiter
            .check_at("a", enqueue, &serde_json::json!({ "jobs": jobs[..1] }), now)
            .unwrap_err();
        assert_eq!(
            refused.envelope().details.as_ref().unwrap()["limit"],
            "session_request"
        );
        let anonymous = serde_json::json!({"jobs":[{"url":"https://a.example/x"}]});
        limiter.check_at("a", enqueue, &anonymous, now).unwrap();
        // Dry runs raise no approval and spend no session tokens.
        let dry = serde_json::json!({ "jobs": jobs[..5], "dry_run": true });
        limiter.check_at("a", enqueue, &dry, now).unwrap();
    }

    #[test]
    fn probes_have_their_own_small_bucket_and_waits_are_free() {
        let limiter = RateLimiter::default();
        let now = Instant::now();
        let probe = lookup("jobs.probe").unwrap();
        for _ in 0..20 {
            limiter.check_at("a", probe, &Value::Null, now).unwrap();
        }
        assert!(limiter.check_at("a", probe, &Value::Null, now).is_err());
        let wait = lookup("jobs.wait").unwrap();
        for _ in 0..1000 {
            limiter.check_at("a", wait, &Value::Null, now).unwrap();
        }
        limiter
            .check_at("a", lookup("jobs.list").unwrap(), &Value::Null, now)
            .unwrap();
    }
}
