//! Dry-run planning contracts and estimators.
//!
//! The planner fetches a few pages through an injected [`PageFetcher`] (so tests are hermetic and the daemon
//! controls politeness and sessions) and produces a [`crate::report::DryRunReport`]. The skeleton ships the trait
//! contracts and the pure estimators; the daemon-side implementation is a later task.

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
