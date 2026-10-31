use crate::{runtime::*, *};

#[tokio::test]
async fn sitemap_dry_run_negotiates_xml_then_html_and_keeps_signed_locations() {
    struct Negotiated(std::sync::Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl PageFetcher for Negotiated {
        async fn fetch(&self, _: &str) -> Result<PageSample, crate::planner::PlannerError> {
            Err(crate::planner::PlannerError::Fetch(
                "typed request hook was not used".into(),
            ))
        }
        async fn fetch_document_request(
            &self,
            request: &FetchRequest,
        ) -> Result<Document, crate::planner::PlannerError> {
            self.0.lock().unwrap().push(request.url.clone());
            let (body, content_type) = match (request.url.as_str(), request.accept.as_str()) {
                ("https://portal.example.org/map.xml", "application/xml,text/xml;q=0.9") => (
                    "<urlset><url><loc>https://portal.example.org/listing?sig=A%2Bb&amp;x=1</loc></url></urlset>".into(), "application/xml"),
                ("https://portal.example.org/listing?sig=A%2Bb&x=1", "text/html,application/xhtml+xml;q=0.9,*/*;q=0.1") => (page("Negotiated paper", "/one.pdf"), "text/html"),
                _ => return Err(crate::planner::PlannerError::Fetch("content negotiation rejected".into())),
            };
            Ok(Document {
                final_url: request.url.clone(),
                status: 200,
                content_type: content_type.into(),
                body,
                headers: Default::default(),
            })
        }
    }
    let mut p = plan();
    p.pagination = None;
    p.sources = vec![Source::Sitemap {
        url: "https://portal.example.org/map.xml".into(),
        loc_pattern: None,
    }];
    let fetcher = Negotiated(Default::default());
    let report = BoundedPlanner.dry_run(&p, &fetcher, 5).await.unwrap();
    assert!(report.complete);
    assert_eq!(report.sampled_pages, 2);
    assert_eq!(report.sample.len(), 1);
    assert_eq!(
        *fetcher.0.lock().unwrap(),
        [
            "https://portal.example.org/map.xml",
            "https://portal.example.org/listing?sig=A%2Bb&x=1"
        ]
    );
}
use async_trait::async_trait;
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
struct Memory {
    approved: AtomicBool,
    fail_commit: AtomicBool,
    state: Mutex<Option<RunSnapshot>>,
    jobs: Mutex<Vec<JobIntent>>,
    pages: Mutex<BTreeMap<String, String>>,
    requests: Mutex<Vec<String>>,
}
impl Memory {
    fn new() -> Self {
        Self {
            approved: AtomicBool::new(true),
            fail_commit: AtomicBool::new(false),
            state: Mutex::new(None),
            jobs: Mutex::new(vec![]),
            pages: Mutex::new(BTreeMap::new()),
            requests: Mutex::new(vec![]),
        }
    }
}
#[async_trait]
impl RuntimePort for Memory {
    async fn authorize(&self, _: zeus_types::RunId, _: &str) -> RuntimeResult<()> {
        if self.approved.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(RuntimeError::Policy("policy.approval_required"))
        }
    }
    async fn fetch(&self, r: FetchRequest) -> RuntimeResult<Document> {
        self.requests.lock().unwrap().push(r.url.clone());
        let body = self
            .pages
            .lock()
            .unwrap()
            .get(&r.url)
            .cloned()
            .unwrap_or_default();
        Ok(Document {
            final_url: r.url,
            status: 200,
            content_type: "text/html".into(),
            body,
            headers: BTreeMap::new(),
        })
    }
    async fn job_totals(&self, _: zeus_types::RunId) -> RuntimeResult<JobTotals> {
        Ok(JobTotals {
            completed: self.jobs.lock().unwrap().len() as u32,
            ..Default::default()
        })
    }
    async fn commit(
        &self,
        expected: u64,
        next: &RunSnapshot,
        jobs: &[JobIntent],
    ) -> RuntimeResult<()> {
        if self.fail_commit.swap(false, Ordering::SeqCst) {
            return Err(RuntimeError::Port("test.commit_interrupted"));
        }
        let mut stored = self.state.lock().unwrap();
        if stored.as_ref().map_or(0, |s| s.checkpoint_seq) != expected {
            return Err(RuntimeError::CheckpointConflict);
        }
        self.jobs.lock().unwrap().extend_from_slice(jobs);
        *stored = Some(next.clone());
        Ok(())
    }
    async fn control_jobs(&self, _: zeus_types::RunId, _: RunAction) -> RuntimeResult<()> {
        Ok(())
    }
    async fn finish(&self, _: &Plan, _: &RunSnapshot) -> RuntimeResult<()> {
        Ok(())
    }
    async fn validate_environment(&self, _: &Plan) -> RuntimeResult<()> {
        Ok(())
    }
}
fn plan() -> Plan {
    let mut p = crate::plan::example_plan();
    p.session = None;
    p.pagination = Some(Pagination::PageParam {
        param: "page".into(),
        start: 1,
        end: Some(2),
        step: 1,
        max_pages: 2,
    });
    p.fetch.min_interval_ms = 200;
    p.budgets.max_pages = 2;
    p.post.rename = Some("{title|slug}.pdf".into());
    p
}
fn page(title: &str, file: &str) -> String {
    format!("<li class='paper'><h3>{title}</h3><a class='pdf' href='{file}'>Download</a></li>")
}
#[tokio::test(start_paused = true)]
async fn report_counters_commit_once_with_discovery_and_survive_restore() {
    let mut p = plan();
    p.pagination = None;
    p.filters = vec![Filter::UrlRegex {
        pattern: "\\.pdf$".into(),
        invert: false,
    }];
    let mut runtime =
        WorkflowRuntime::new(p.clone(), zeus_types::IdGenerator::new().run_id()).unwrap();
    assert!(runtime.report().started_at.is_none());
    assert!(runtime.report().created_at.is_some());
    let port = Memory::new();
    port.pages.lock().unwrap().insert(
        "https://portal.example.org/papers?year=2026".into(),
        format!(
            "{}{}{}{}{}",
            page("First", "/one.pdf"),
            page("Duplicate", "/one.pdf"),
            page("Filtered", "/readme.txt"),
            page("Outside", "https://elsewhere.example/file.pdf"),
            "<li class='paper'><h3>No URL</h3></li>"
        ),
    );
    port.fail_commit.store(true, Ordering::SeqCst);
    assert!(runtime.step(&port).await.is_err());
    assert_eq!(runtime.report().counts.as_ref().unwrap().items_seen, 0);
    runtime.step(&port).await.unwrap();
    let counts = runtime.report().counts.as_ref().unwrap();
    assert_eq!(
        (
            counts.pages_fetched,
            counts.items_seen,
            counts.items_duplicate,
            counts.items_filtered,
            counts.items_out_of_domain,
            counts.items_invalid
        ),
        (1, 5, 1, 1, 1, 1)
    );
    assert_eq!(counts.files_enqueued, 1);
    assert!(runtime.report().started_at.is_some());
    let mut restored = WorkflowRuntime::restore(p, runtime.snapshot().clone()).unwrap();
    restored.step(&port).await.unwrap();
    let counts = restored.report().counts.as_ref().unwrap();
    assert_eq!(counts.items_seen, 5);
    assert_eq!(counts.files_completed, 1);
    assert_eq!(restored.report().status, RunStatus::Completed);
    assert!(!restored.report().possibly_incomplete);
    assert!(
        !serde_json::to_string(restored.report())
            .unwrap()
            .contains("https://")
    );
}

#[test]
fn legacy_reports_retain_unknown_counters_instead_of_backfilling_zero() {
    let runtime = WorkflowRuntime::new(plan(), zeus_types::IdGenerator::new().run_id()).unwrap();
    let mut value = serde_json::to_value(runtime.report()).unwrap();
    for name in [
        "report_version",
        "plan_id",
        "approval_id",
        "grant_id",
        "created_at",
        "updated_at",
        "elapsed_ms",
        "counts",
        "pause_reason",
        "possibly_incomplete",
        "last_error",
    ] {
        value.as_object_mut().unwrap().remove(name);
    }
    value["started_at"] = serde_json::json!(zeus_types::Timestamp::now());
    let old: RunReport = serde_json::from_value(value).unwrap();
    assert_eq!(old.report_version, 0);
    assert!(old.counts.is_none());
    assert!(old.created_at.is_none());
}
#[tokio::test(start_paused = true)]
async fn authorization_precedes_fetch_and_atomic_retry_does_not_lose_an_item() {
    let p = plan();
    let mut runtime =
        WorkflowRuntime::new(p.clone(), zeus_types::IdGenerator::new().run_id()).unwrap();
    let port = Memory::new();
    port.pages.lock().unwrap().insert(
        "https://portal.example.org/papers?year=2026&page=1".into(),
        page("First", "/one.pdf"),
    );
    port.pages.lock().unwrap().insert(
        "https://portal.example.org/papers?year=2026&page=2".into(),
        page("Again", "/one.pdf"),
    );
    port.approved.store(false, Ordering::SeqCst);
    assert!(runtime.step(&port).await.is_err());
    assert!(port.requests.lock().unwrap().is_empty());
    port.approved.store(true, Ordering::SeqCst);
    port.fail_commit.store(true, Ordering::SeqCst);
    assert!(runtime.step(&port).await.is_err());
    assert_eq!(runtime.snapshot().checkpoint_seq, 0);
    assert!(port.jobs.lock().unwrap().is_empty());
    runtime.step(&port).await.unwrap();
    assert_eq!(port.jobs.lock().unwrap().len(), 1);
    let saved = runtime.snapshot().clone();
    let mut resumed = WorkflowRuntime::restore(p.clone(), saved.clone()).unwrap();
    resumed.step(&port).await.unwrap();
    resumed.step(&port).await.unwrap();
    assert_eq!(port.jobs.lock().unwrap().len(), 1);
    assert_eq!(resumed.report().status, RunStatus::Completed);
    let mut changed = p;
    changed.budgets.max_files += 1;
    assert!(WorkflowRuntime::restore(changed, saved).is_err());
}
#[tokio::test(start_paused = true)]
async fn no_match_pauses_for_drift_and_resume_retries_the_same_document() {
    let mut p = plan();
    p.pagination = None;
    let mut runtime = WorkflowRuntime::new(p, zeus_types::IdGenerator::new().run_id()).unwrap();
    let port = Memory::new();
    runtime.step(&port).await.unwrap();
    assert_eq!(runtime.report().status, RunStatus::Paused);
    port.pages.lock().unwrap().insert(
        "https://portal.example.org/papers?year=2026".into(),
        page("Recovered", "/ok.pdf"),
    );
    runtime.control(&port, RunAction::Resume).await.unwrap();
    runtime.step(&port).await.unwrap();
    assert_eq!(port.jobs.lock().unwrap().len(), 1);
}
#[tokio::test(start_paused = true)]
async fn file_budget_ends_discovery_without_fetching_an_extra_page() {
    let mut p = plan();
    p.budgets.max_files = 1;
    let mut runtime = WorkflowRuntime::new(p, zeus_types::IdGenerator::new().run_id()).unwrap();
    let port = Memory::new();
    port.pages.lock().unwrap().insert(
        "https://portal.example.org/papers?year=2026&page=1".into(),
        format!("{}{}", page("one", "/one.pdf"), page("two", "/two.pdf")),
    );
    runtime.step(&port).await.unwrap();
    runtime.step(&port).await.unwrap();
    assert_eq!(port.jobs.lock().unwrap().len(), 1);
    assert_eq!(port.requests.lock().unwrap().len(), 1);
    assert_eq!(runtime.report().status, RunStatus::BudgetExhausted);
}
#[test]
fn strict_plan_and_published_normalization_vectors() {
    let mut value = serde_json::to_value(plan()).unwrap();
    value["fetch"]["hidden_network_rule"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Plan>(value).is_err());
    let spec = include_str!("../../../docs/spec/08-workflow-plan-v0.md");
    let re = regex::Regex::new(
        r"(?s)```json validate=workflow-plan.v0#/\$defs/Plan plan_hash=([a-f0-9]{64})\n(.*?)\n```",
    )
    .unwrap();
    let mut count = 0;
    for captured in re.captures_iter(spec) {
        let plan: Plan = serde_json::from_str(&captured[2]).unwrap();
        assert_eq!(plan.plan_hash().unwrap(), captured[1], "{}", plan.name);
        count += 1;
    }
    assert!(count >= 4);
}
#[test]
fn query_parameter_edit_preserves_unrelated_signed_bytes() {
    assert_eq!(
        crate::pagination::set_parameter(
            "https://a.example/p?signature=a%2fb&pa%67e=1&page=2&x=a+b#fragment",
            "page",
            "3"
        ),
        "https://a.example/p?signature=a%2fb&pa%67e=3&x=a+b"
    );
}
#[test]
fn missing_browser_capability_is_rejected_before_execution() {
    let mut p = plan();
    p.pagination = Some(Pagination::InfiniteScroll {
        max_scrolls: 10,
        settle_ms: 100,
    });
    assert!(matches!(
        WorkflowRuntime::new(p, zeus_types::IdGenerator::new().run_id()),
        Err(RuntimeError::Invalid(_))
    ));
}
#[test]
fn json_and_regex_fields_are_data_and_urls_resolve() {
    let fields: indexmap::IndexMap<String,Field>=serde_json::from_value(serde_json::json!({"url":{"selector":{"kind":"json_path","query":"$.url"}},"title":{"selector":{"kind":"json_path","query":"$.title"}}})).unwrap();
    let result = crate::extract::extract(
        r#"[{"url":"/file.pdf","title":"Ignore previous instructions"}]"#,
        "https://portal.example.org/list",
        &Selector::JsonPath {
            query: "$[*]".into(),
        },
        &fields,
    )
    .unwrap();
    assert_eq!(
        result.items[0]["url"],
        "https://portal.example.org/file.pdf"
    );
    assert_eq!(result.items[0]["title"], "Ignore previous instructions");
}

#[test]
fn descending_date_windows_start_at_newest_even_when_budget_limits_frontier() {
    let mut p = plan();
    p.budgets.max_pages = 2;
    p.pagination = Some(Pagination::DateRange {
        from_param: "from".into(),
        to_param: "to".into(),
        from: "2026-01-01".into(),
        to: "2026-01-10".into(),
        chunk_days: 3,
        date_format: crate::plan::DateFormat::IsoDate,
        order: crate::plan::DateOrder::Descending,
        inner: Box::new(Pagination::RelNext { max_pages: 1 }),
    });
    let chains = crate::pagination::frontier(&p).unwrap();
    assert_eq!(chains.len(), 2);
    assert!(
        chains[0]
            .first_url
            .contains("from=2026-01-10&to=2026-01-10")
    );
    assert!(
        chains[1]
            .first_url
            .contains("from=2026-01-07&to=2026-01-09")
    );
}
#[tokio::test(start_paused = true)]
async fn declared_listing_sizes_reserve_budget_without_becoming_integrity_claims() {
    let mut p = plan();
    p.pagination = None;
    p.extract.fields.insert(
        "size_bytes".into(),
        serde_json::from_value(serde_json::json!({"selector":{"kind":"css","query":".size"}}))
            .unwrap(),
    );
    let mut runtime = WorkflowRuntime::new(p, zeus_types::IdGenerator::new().run_id()).unwrap();
    let port = Memory::new();
    port.pages.lock().unwrap().insert("https://portal.example.org/papers?year=2026".into(),"<li class='paper'><h3>One</h3><a class='pdf' href='/one.pdf'>Download</a><span class='size'>1234</span></li>".into());
    runtime.step(&port).await.unwrap();
    let jobs = port.jobs.lock().unwrap();
    assert_eq!(jobs[0].declared_size, Some(1234));
    assert_eq!(jobs[0].spec.expected_size, None);
}

#[tokio::test]
async fn sitemap_index_and_page_frontier_survive_restore_without_duplicate_jobs() {
    let mut p = plan();
    p.pagination = None;
    p.budgets.max_pages = 10;
    p.sources = vec![Source::Sitemap {
        url: "https://portal.example.org/map.xml".into(),
        loc_pattern: Some("/listing/".into()),
    }];
    let port = Memory::new();
    port.pages.lock().unwrap().extend([
        ("https://portal.example.org/map.xml".into(),"<sitemapindex><sitemap><loc>https://portal.example.org/part.xml</loc></sitemap><sitemap><loc>https://portal.example.org/map.xml</loc></sitemap></sitemapindex>".into()),
        ("https://portal.example.org/part.xml".into(),"<urlset><url><loc>https://portal.example.org/listing/one?sig=A%2Bb&amp;x=1</loc></url><url><loc>https://portal.example.org/skipped</loc></url><url><loc>https://portal.example.org/listing/one?sig=A%2Bb&amp;x=1</loc></url></urlset>".into()),
        ("https://portal.example.org/listing/one?sig=A%2Bb&x=1".into(),page("Paper one","/one.pdf")),
    ]);
    let mut runtime =
        WorkflowRuntime::new(p.clone(), zeus_types::IdGenerator::new().run_id()).unwrap();
    runtime.step(&port).await.unwrap();
    assert_eq!(runtime.report().pages_fetched, 1);
    assert_eq!(port.jobs.lock().unwrap().len(), 0);
    runtime = WorkflowRuntime::restore(p.clone(), runtime.snapshot().clone()).unwrap();
    runtime.step(&port).await.unwrap();
    assert_eq!(runtime.report().pages_fetched, 2);
    runtime = WorkflowRuntime::restore(p, runtime.snapshot().clone()).unwrap();
    runtime.step(&port).await.unwrap();
    runtime.step(&port).await.unwrap();
    assert_eq!(runtime.report().status, RunStatus::Completed);
    assert_eq!(runtime.report().pages_fetched, 3);
    assert_eq!(port.jobs.lock().unwrap().len(), 1);
    let requests = port.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2],
        "https://portal.example.org/listing/one?sig=A%2Bb&x=1"
    );
}
#[tokio::test]
async fn sitemap_bootstrap_commit_failure_never_loses_or_executes_a_partial_frontier() {
    let mut p = plan();
    p.pagination = None;
    p.budgets.max_pages = 5;
    p.sources = vec![Source::Sitemap {
        url: "https://portal.example.org/map.xml".into(),
        loc_pattern: None,
    }];
    let port = Memory::new();
    port.pages.lock().unwrap().insert(
        "https://portal.example.org/map.xml".into(),
        "<urlset><url><loc>https://portal.example.org/listing</loc></url></urlset>".into(),
    );
    let mut runtime = WorkflowRuntime::new(p, zeus_types::IdGenerator::new().run_id()).unwrap();
    port.fail_commit.store(true, Ordering::SeqCst);
    assert!(runtime.step(&port).await.is_err());
    assert_eq!(runtime.report().pages_fetched, 0);
    assert_eq!(runtime.snapshot().chains.len(), 1);
    assert!(port.jobs.lock().unwrap().is_empty());
    runtime.step(&port).await.unwrap();
    assert_eq!(runtime.report().pages_fetched, 1);
    assert_eq!(runtime.snapshot().chains.len(), 2);
}
#[tokio::test]
async fn sitemap_scope_failure_pauses_and_page_budget_counts_bootstrap_requests() {
    let mut p = plan();
    p.pagination = None;
    p.budgets.max_pages = 1;
    p.sources = vec![Source::Sitemap {
        url: "https://portal.example.org/map.xml".into(),
        loc_pattern: None,
    }];
    let port = Memory::new();
    port.pages.lock().unwrap().insert(
        "https://portal.example.org/map.xml".into(),
        "<urlset><url><loc>https://portal.example.org/listing</loc></url></urlset>".into(),
    );
    let mut runtime =
        WorkflowRuntime::new(p.clone(), zeus_types::IdGenerator::new().run_id()).unwrap();
    runtime.step(&port).await.unwrap();
    runtime.step(&port).await.unwrap();
    runtime.step(&port).await.unwrap();
    assert_eq!(runtime.report().status, RunStatus::BudgetExhausted);
    assert_eq!(
        runtime.report().stop_reason.as_deref(),
        Some("budget_pages")
    );
    assert_eq!(port.requests.lock().unwrap().len(), 1);
    assert!(port.jobs.lock().unwrap().is_empty());
    let port = Memory::new();
    port.pages.lock().unwrap().insert(
        "https://portal.example.org/map.xml".into(),
        "<urlset><url><loc>https://private.example/forbidden</loc></url></urlset>".into(),
    );
    let mut runtime = WorkflowRuntime::new(p, zeus_types::IdGenerator::new().run_id()).unwrap();
    assert!(runtime.step(&port).await.is_err());
    assert_eq!(runtime.report().status, RunStatus::Paused);
    assert_eq!(runtime.snapshot().chains.len(), 1);
    assert!(port.jobs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn item_sink_metadata_is_atomic_redacted_and_ordinal_survives_skip_restore() {
    let mut p = plan();
    p.sinks.items = Some(ItemsSink {
        format: ItemsFormat::Jsonl,
    });
    p.filters.clear();
    let port = Memory::new();
    port.pages.lock().unwrap().insert(
        "https://portal.example.org/papers?year=2026&page=1".into(),
        page("First", "/one.pdf?token=ZEUS-CANARY-ITEM"),
    );
    port.pages.lock().unwrap().insert(
        "https://portal.example.org/papers?year=2026&page=2".into(),
        page("Second", "/two.pdf?token=ZEUS-CANARY-ITEM"),
    );
    let mut runtime =
        WorkflowRuntime::new(p.clone(), zeus_types::IdGenerator::new().run_id()).unwrap();
    port.fail_commit.store(true, Ordering::SeqCst);
    assert!(runtime.step(&port).await.is_err());
    assert!(port.jobs.lock().unwrap().is_empty());
    assert_eq!(runtime.snapshot().item_index, 0);
    runtime.step(&port).await.unwrap();
    {
        let jobs = port.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 1);
        let item = jobs[0].item.as_ref().unwrap();
        let value = serde_json::to_string(item).unwrap();
        assert!(!value.contains("ZEUS-CANARY-ITEM"));
        assert!(item.url_redacted);
        assert_eq!(jobs[0].index, 1);
    }
    let mut snapshot = runtime.snapshot().clone();
    snapshot.report.files_enqueued = 0;
    snapshot
        .report
        .counts
        .as_mut()
        .unwrap()
        .items_skipped_existing = 1;
    *port.state.lock().unwrap() = Some(snapshot.clone());
    let mut restored = WorkflowRuntime::restore(p, snapshot).unwrap();
    restored.step(&port).await.unwrap();
    let jobs = port.jobs.lock().unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[1].index, 2);
    assert_eq!(restored.snapshot().item_index, 2);
}

#[tokio::test]
async fn item_sink_metadata_bound_pauses_without_partially_committing_the_document() {
    let mut p = plan();
    p.pagination = None;
    p.filters.clear();
    p.sinks.items = Some(ItemsSink {
        format: ItemsFormat::Jsonl,
    });
    p.budgets.max_files = 100;
    let field = p.extract.fields.get("title").unwrap().clone();
    for n in 0..30 {
        p.extract.fields.insert(format!("extra_{n}"), field.clone());
    }
    let port = Memory::new();
    let title = "x".repeat(4096);
    let body = (0..80)
        .map(|n| page(&title, &format!("/file-{n}.pdf")))
        .collect::<String>();
    port.pages
        .lock()
        .unwrap()
        .insert("https://portal.example.org/papers?year=2026".into(), body);
    let mut runtime = WorkflowRuntime::new(p, zeus_types::IdGenerator::new().run_id()).unwrap();
    assert!(matches!(
        runtime.step(&port).await,
        Err(RuntimeError::Policy("workflow.items_limit"))
    ));
    assert_eq!(runtime.report().status, RunStatus::Paused);
    assert!(port.jobs.lock().unwrap().is_empty());
    assert_eq!(runtime.snapshot().item_index, 0);
    runtime.step(&port).await.unwrap();
    assert_eq!(port.requests.lock().unwrap().len(), 1);
}
