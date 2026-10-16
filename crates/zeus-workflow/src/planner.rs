//! Dry-run planning contracts and estimators.
//!
//! The planner fetches a few pages through an injected [`PageFetcher`] (so tests are hermetic and the daemon
//! controls politeness and sessions) and produces a [`crate::report::DryRunReport`]. With the runtime
//! feature, `BoundedPlanner` executes the same selectors and pagination through sampling-only ports.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::plan::Plan;
use crate::report::{Diagnostic, DryRunReport};

/// A fetched page, truncated to what validation needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PageSample {
    /// The page URL after redaction.
    pub url_redacted: String,
    /// `Content-Type` as received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Up to 512 KiB of the body. Untrusted content: it is parsed, never interpreted as instructions.
    pub body: String,
}

/// Failures of planning.
#[derive(Debug, Error)]
pub enum PlannerError {
    /// Not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// A page could not be fetched.
    #[error("fetch failed: {0}")]
    Fetch(String),
    /// The plan has errors.
    #[error("the plan has {} error(s)", .0.len())]
    Invalid(Vec<Diagnostic>),
}

/// Source of page samples. The daemon implements it with rate limiting, robots checks and session leases.
#[async_trait]
pub trait PageFetcher: Send + Sync {
    /// Fetch one page for sampling.
    async fn fetch(&self, url: &str) -> Result<PageSample, PlannerError>;
    /// Private full response for correct redirect bases and Link pagination during dry runs.
    #[cfg(feature = "runtime")]
    async fn fetch_document(&self, url: &str) -> Result<crate::runtime::Document, PlannerError> {
        let sample = self.fetch(url).await?;
        Ok(crate::runtime::Document {
            final_url: url.into(),
            status: 200,
            content_type: sample.content_type.unwrap_or_else(|| "text/html".into()),
            body: sample.body,
            headers: Default::default(),
        })
    }
}

/// Produces dry-run reports.
#[async_trait]
pub trait DryRunPlanner: Send + Sync {
    /// Sample the plan's first pages and report what a run would do. `sample_items` is how many items to list
    /// (at most 100).
    async fn dry_run(
        &self,
        plan: &Plan,
        fetcher: &dyn PageFetcher,
        sample_items: u32,
    ) -> Result<DryRunReport, PlannerError>;
}

/// Placeholder planner that returns [`PlannerError::NotImplemented`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NotImplementedPlanner;

#[async_trait]
impl DryRunPlanner for NotImplementedPlanner {
    async fn dry_run(
        &self,
        _plan: &Plan,
        _fetcher: &dyn PageFetcher,
        _sample_items: u32,
    ) -> Result<DryRunReport, PlannerError> {
        Err(PlannerError::NotImplemented("dry-run planner"))
    }
}

/// An estimate with an uncertainty band, in integer units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Estimate {
    /// Optimistic bound.
    pub low: u64,
    /// Best guess.
    pub point: u64,
    /// Pessimistic bound.
    pub high: u64,
}

impl Estimate {
    /// Extrapolate the total number of items from per-page counts of the sampled pages and the expected number
    /// of pages. The point estimate is the sample mean times `total_pages`; the band is the smallest and largest
    /// observed page times `total_pages`. With a single sample there is no spread to observe, so the band is
    /// half to double the point estimate. `None` when there is nothing to extrapolate from.
    pub fn from_page_counts(items_per_page: &[u64], total_pages: u64) -> Option<Self> {
        if items_per_page.is_empty() || total_pages == 0 {
            return None;
        }
        let n = items_per_page.len() as u128;
        let sum: u128 = items_per_page.iter().map(|&c| u128::from(c)).sum();
        let total = u128::from(total_pages);
        let point = sum * total / n;
        let (min, max) = items_per_page
            .iter()
            .fold((u64::MAX, 0u64), |(lo, hi), &c| (lo.min(c), hi.max(c)));
        let (low, high) = if items_per_page.len() == 1 {
            (point / 2, point.saturating_mul(2))
        } else {
            (u128::from(min) * total, u128::from(max) * total)
        };
        let clamp = |v: u128| u64::try_from(v).unwrap_or(u64::MAX);
        Some(Self {
            low: clamp(low),
            point: clamp(point),
            high: clamp(high),
        })
    }

    /// Duration of a run in milliseconds at one request per `min_interval_ms` per host, for `requests` requests
    /// (pages plus files) spread over `hosts` hosts with `concurrency` simultaneous requests per host.
    pub fn duration_ms(requests: u64, min_interval_ms: u64, hosts: u64, concurrency: u64) -> u64 {
        let lanes = hosts.max(1).saturating_mul(concurrency.max(1));
        requests.div_ceil(lanes).saturating_mul(min_interval_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn estimate_from_several_pages() {
        let e = Estimate::from_page_counts(&[20, 18, 22], 50).unwrap();
        assert_eq!(e.point, 1000);
        assert_eq!((e.low, e.high), (900, 1100));
    }

    #[test]
    fn estimate_from_one_page_is_wide() {
        let e = Estimate::from_page_counts(&[20], 10).unwrap();
        assert_eq!((e.low, e.point, e.high), (100, 200, 400));
    }

    #[test]
    fn nothing_to_extrapolate() {
        assert!(Estimate::from_page_counts(&[], 10).is_none());
        assert!(Estimate::from_page_counts(&[5], 0).is_none());
    }

    #[test]
    fn duration_uses_ceiling_division() {
        assert_eq!(Estimate::duration_ms(10, 2000, 1, 2), 10_000);
        assert_eq!(Estimate::duration_ms(11, 2000, 1, 2), 12_000);
        assert_eq!(Estimate::duration_ms(0, 2000, 1, 1), 0);
    }

    proptest! {
        #[test]
        fn band_brackets_the_point(counts in proptest::collection::vec(0u64..1000, 2..20), pages in 1u64..10_000) {
            let e = Estimate::from_page_counts(&counts, pages).unwrap();
            prop_assert!(e.low <= e.point && e.point <= e.high);
        }
    }
}

/// Concrete bounded planner using the same selectors, filters, naming and pagination as execution.
#[cfg(feature = "runtime")]
#[derive(Debug, Clone, Copy, Default)]
pub struct BoundedPlanner;

#[cfg(feature = "runtime")]
#[async_trait]
impl DryRunPlanner for BoundedPlanner {
    async fn dry_run(
        &self,
        plan: &Plan,
        fetcher: &dyn PageFetcher,
        sample_items: u32,
    ) -> Result<DryRunReport, PlannerError> {
        use crate::runtime::*;
        use std::sync::Mutex;
        struct Samples<'a> {
            fetcher: &'a dyn PageFetcher,
            items: Mutex<Vec<JobIntent>>,
            counts: Mutex<Vec<u64>>,
        }
        #[async_trait]
        impl RuntimePort for Samples<'_> {
            async fn authorize(&self, _: zeus_types::RunId, _: &str) -> RuntimeResult<()> {
                Ok(())
            }
            async fn validate_environment(&self, _: &Plan) -> RuntimeResult<()> {
                Ok(())
            }
            async fn fetch(&self, r: FetchRequest) -> RuntimeResult<Document> {
                let document = self
                    .fetcher
                    .fetch_document(&r.url)
                    .await
                    .map_err(|_| RuntimeError::Port("workflow.sample_fetch_failed"))?;
                if document.body.len() > 512 * 1024 {
                    return Err(RuntimeError::Policy("workflow.sample_too_large"));
                }
                Ok(document)
            }
            async fn job_totals(&self, _: zeus_types::RunId) -> RuntimeResult<JobTotals> {
                Ok(JobTotals::default())
            }
            async fn commit(
                &self,
                _: u64,
                _: &RunSnapshot,
                jobs: &[JobIntent],
            ) -> RuntimeResult<()> {
                if !jobs.is_empty() {
                    self.counts
                        .lock()
                        .map_err(|_| RuntimeError::Port("workflow.sample_state"))?
                        .push(jobs.len() as u64);
                }
                self.items
                    .lock()
                    .map_err(|_| RuntimeError::Port("workflow.sample_state"))?
                    .extend_from_slice(jobs);
                Ok(())
            }
            async fn control_jobs(&self, _: zeus_types::RunId, _: RunAction) -> RuntimeResult<()> {
                Ok(())
            }
            async fn finish(&self, _: &Plan, _: &RunSnapshot) -> RuntimeResult<()> {
                Ok(())
            }
        }
        let diagnostics = crate::validate::validate_plan(plan);
        if crate::validate::has_errors(&diagnostics) {
            return Err(PlannerError::Invalid(diagnostics));
        }
        let mut sampled = plan.clone();
        sampled.budgets.max_pages = sampled.budgets.max_pages.min(10);
        sampled.budgets.max_files = sampled.budgets.max_files.min(sample_items.clamp(1, 100));
        sampled.budgets.max_duration_ms = sampled.budgets.max_duration_ms.min(60_000);
        fn limit(p: &mut crate::Pagination) {
            match p {
                crate::Pagination::RelNext { max_pages }
                | crate::Pagination::PageParam { max_pages, .. }
                | crate::Pagination::OffsetLimit { max_pages, .. }
                | crate::Pagination::Cursor { max_pages, .. }
                | crate::Pagination::NextSelector { max_pages, .. } => {
                    *max_pages = (*max_pages).min(3)
                }
                crate::Pagination::DateRange { inner, .. } => limit(inner),
                _ => {}
            }
        }
        if let Some(p) = &mut sampled.pagination {
            limit(p);
        }
        let mut runner = WorkflowRuntime::new(sampled, zeus_types::IdGenerator::new().run_id())
            .map_err(|e| match e {
                RuntimeError::Invalid(d) => PlannerError::Invalid(d),
                _ => PlannerError::Fetch("workflow.sample_invalid".into()),
            })?;
        let port = Samples {
            fetcher,
            items: Mutex::new(vec![]),
            counts: Mutex::new(vec![]),
        };
        let work = async {
            for _ in 0..32 {
                runner.step(&port).await?;
                if !matches!(
                    runner.report().status,
                    crate::RunStatus::Running | crate::RunStatus::AwaitingApproval
                ) {
                    break;
                }
            }
            Ok::<(), RuntimeError>(())
        };
        let mut warnings = diagnostics;
        let complete = match tokio::time::timeout(std::time::Duration::from_secs(60), work).await {
            Ok(Ok(())) => true,
            Ok(Err(RuntimeError::Invalid(d))) => return Err(PlannerError::Invalid(d)),
            Ok(Err(_)) => return Err(PlannerError::Fetch("workflow.sample_fetch_failed".into())),
            Err(_) => {
                warnings.push(Diagnostic::warning(
                    "plan.dry_run_partial",
                    "/fetch",
                    "sampling did not finish within the 60-second deadline",
                ));
                false
            }
        };
        warnings.extend(runner.report().drift.clone());
        if plan.session.is_some() {
            warnings.push(Diagnostic::warning(
                "plan.tos_logged_in",
                "/session",
                "this plan uses a logged-in session; the site's automation terms apply",
            ));
        }
        let intents = port
            .items
            .into_inner()
            .map_err(|_| PlannerError::Fetch("workflow.sample_state".into()))?;
        let counts = port
            .counts
            .into_inner()
            .map_err(|_| PlannerError::Fetch("workflow.sample_state".into()))?;
        let mut sample = Vec::new();
        let mut paths = std::collections::BTreeMap::new();
        let mut collisions = 0;
        for item in intents
            .into_iter()
            .take(sample_items.clamp(1, 100) as usize)
        {
            let filename = item.spec.filename.unwrap_or_else(|| "download".into());
            let path = format!("{}/{}", item.spec.dest_dir.unwrap_or_default(), filename);
            let index = sample.len() as u32 + 1;
            let prior = paths.insert(path, index);
            if prior.is_some() {
                collisions += 1;
            }
            sample.push(crate::PlannedItem {
                index,
                url_redacted: display_url(item.spec.url.expose()),
                file_name: filename,
                size_bytes: item.declared_size,
                collides_with: prior,
            });
        }
        let files =
            Estimate::from_page_counts(&counts, u64::from(plan.budgets.max_pages)).map(|e| {
                Estimate {
                    low: e.low.min(plan.budgets.max_files as u64),
                    point: e.point.min(plan.budgets.max_files as u64),
                    high: e.high.min(plan.budgets.max_files as u64),
                }
            });
        let sizes = sample
            .iter()
            .filter_map(|i| i.size_bytes)
            .collect::<Vec<_>>();
        let bytes = files.filter(|_| !sizes.is_empty()).map(|e| {
            let mean = sizes.iter().fold(0u64, |a, b| a.saturating_add(*b)) / sizes.len() as u64;
            Estimate {
                low: e.low.saturating_mul(mean).min(plan.budgets.max_bytes),
                point: e.point.saturating_mul(mean).min(plan.budgets.max_bytes),
                high: e.high.saturating_mul(mean).min(plan.budgets.max_bytes),
            }
        });
        let duration = files.map(|e| {
            let estimate = |n: u64| {
                n.saturating_add(plan.budgets.max_pages as u64)
                    .saturating_mul(plan.fetch.min_interval_ms)
            };
            Estimate {
                low: estimate(e.low),
                point: estimate(e.point),
                high: estimate(e.high),
            }
        });
        Ok(DryRunReport {
            plan_hash: plan.plan_hash().map_err(|_| {
                PlannerError::Invalid(vec![Diagnostic::error(
                    "workflow.plan_invalid",
                    "/",
                    "plan cannot be canonicalized",
                )])
            })?,
            generated_at: zeus_types::Timestamp::now(),
            complete,
            sampled_pages: runner.report().pages_fetched,
            sample,
            name_collisions: collisions,
            estimate_files: files,
            estimate_bytes: bytes,
            estimate_duration_ms: duration,
            warnings,
        })
    }
}
