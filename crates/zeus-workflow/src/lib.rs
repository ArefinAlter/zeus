//! Zeus workflow plans: the typed plan IR (version 0), the template expression language, the validator, the
//! dry-run and run report formats, and estimators.
//!
//! Design: `docs/ARCHITECTURE.md` section 8 (input for the planned spec 08). A plan is **data, not code**: the
//! only expression form is a [`Template`] with a closed filter set; budgets are mandatory; every URL must be
//! inside `allowed_domains`; robots.txt is honoured by default. Plans contain no floating-point numbers so their
//! JCS + BLAKE3 hash ([`Plan::plan_hash`]) is stable and can bind an approval.
//!
//! The wire types and pure validation are always available. Selector engines (HTML, JSONPath, regex) sit behind
//! the `runtime` feature so thin clients (`zeus-api`) do not link them.

#![forbid(unsafe_code)]

pub mod canonical;
pub mod expr;
pub mod plan;
pub mod planner;
pub mod report;
pub mod validate;

pub use expr::{ExprError, Template};
pub use plan::{
    Budgets, ConvertStep, Extract, Extraction, FetchPolicy, Field, Filter, ManifestFormat,
    ManifestSink, PLAN_VERSION, Pagination, Plan, PlanId, Post, Selector, SessionRequirement,
    Sinks, Source, StopRules,
};
pub use planner::{DryRunPlanner, Estimate, NotImplementedPlanner, PageFetcher, PageSample};
pub use report::{
    Checkpoint, Diagnostic, DryRunReport, PlannedItem, RunReport, RunStatus, Severity,
};
pub use validate::{has_errors, validate_plan, validate_samples};
