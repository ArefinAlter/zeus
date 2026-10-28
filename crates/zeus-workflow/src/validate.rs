//! Plan validation.
//!
//! [`validate_plan`] checks a plan on its own (structure, bounds, domains, templates, selector and pattern syntax
//! when the `runtime` feature is on). [`validate_samples`] checks selectors against real page samples: a selector
//! that matches nothing, or implausibly many nodes, is rejected before any approval (ARCHITECTURE section 8).

use std::collections::BTreeSet;

use url::Url;
use zeus_types::GrantScope;

use crate::expr::Template;
use crate::plan::{Filter, Pagination, Plan, Selector, Source};
use crate::planner::PageSample;
use crate::report::{Diagnostic, Severity};

/// Hard maximum of `budgets.max_pages`.
pub const MAX_PAGES_HARD: u32 = 100_000;
/// Hard maximum of `budgets.max_files`.
pub const MAX_FILES_HARD: u32 = 1_000_000;
/// Hard maximum of `budgets.max_bytes` (1 PiB).
pub const MAX_BYTES_HARD: u64 = 1 << 50;
/// Hard maximum of `budgets.max_duration_ms` (7 days).
pub const MAX_DURATION_HARD_MS: u64 = 7 * 24 * 3600 * 1000;
/// Hard maximum of `budgets.max_depth`.
pub const MAX_DEPTH_HARD: u8 = 8;
/// Smallest allowed `fetch.min_interval_ms`.
pub const MIN_INTERVAL_FLOOR_MS: u64 = 200;
/// Most fields in `extract.fields`.
pub const MAX_FIELDS: usize = 32;
/// Most start URLs.
pub const MAX_START_URLS: usize = 100;
/// Longest selector, pattern or JSONPath in bytes.
pub const MAX_SELECTOR_BYTES: usize = 1024;
/// A page matching more item nodes than this is rejected as too broad.
pub const MAX_ITEMS_PER_PAGE: usize = 10_000;
/// Template fields that exist without being declared in `extract.fields`.
pub const BUILTIN_FIELDS: &[&str] = &["url", "filename", "ext", "index", "page"];

/// True when `host` is allowed by an `allowed_domains` entry: an exact match, or `*.example.com` matching any
/// proper subdomain of `example.com` (not the apex itself).
pub fn host_allowed(host: &str, allowed: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    allowed.iter().any(|entry| match entry.strip_prefix("*.") {
        Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
        None => host == *entry,
    })
}

fn valid_domain_entry(entry: &str) -> bool {
    let name = entry.strip_prefix("*.").unwrap_or(entry);
    !name.is_empty()
        && name.len() <= 253
        && name.contains('.')
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

fn valid_field_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Validate a plan in isolation. An empty result, or one with no [`Severity::Error`], means the plan can proceed
/// to a dry run.
pub fn validate_plan(plan: &Plan) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if plan.plan_version != crate::plan::PLAN_VERSION {
        out.push(Diagnostic::error(
            "plan.unsupported_version",
            "/plan_version",
            format!(
                "this build reads plan_version {}",
                crate::plan::PLAN_VERSION
            ),
        ));
    }
    if plan.name.trim().is_empty() || plan.name.len() > 120 {
        out.push(Diagnostic::error(
            "plan.name_invalid",
            "/name",
            "name must be 1 to 120 characters",
        ));
    }
    if plan.sources.len() > 16
        || plan.filters.len() > 32
        || plan.goal.as_ref().is_some_and(|v| v.len() > 2000)
        || crate::canonical::jcs_of(plan).is_ok_and(|v| v.len() > 65_536)
    {
        out.push(Diagnostic::error(
            "plan.limit_exceeded",
            "/",
            "plan exceeds structural limits",
        ));
    }
    if !(1..=8).contains(&plan.fetch.max_connections_per_file) {
        out.push(Diagnostic::error(
            "plan.fetch_concurrency",
            "/fetch/max_connections_per_file",
            "file connections must be 1 to 8",
        ));
    }
    if let Some(follow) = &plan.extract.follow {
        if follow.fields.is_empty()
            || follow.fields.len() > 32
            || follow
                .fields
                .keys()
                .any(|k| plan.extract.fields.contains_key(k))
        {
            out.push(Diagnostic::error(
                "plan.follow_fields",
                "/extract/follow",
                "detail fields must be nonempty and distinct from listing fields",
            ));
        }
        check_template(&follow.url, plan, "/extract/follow/url", &mut out);
        for (name, field) in &follow.fields {
            if let Some(selector) = &field.selector {
                check_selector(
                    selector,
                    &format!("/extract/follow/fields/{name}/selector"),
                    &mut out,
                );
            }
        }
        if plan.budgets.max_depth < 2 {
            out.push(Diagnostic::error(
                "plan.depth_exceeded",
                "/budgets/max_depth",
                "detail documents require a file depth of at least 2",
            ));
        }
    }
    check_domains(plan, &mut out);
    check_session(plan, &mut out);
    check_sources(plan, &mut out);
    check_pagination(plan, &mut out);
    check_extract(plan, &mut out);
    check_filters(plan, &mut out);
    check_fetch(plan, &mut out);
    check_budgets(plan, &mut out);
    check_post_and_sinks(plan, &mut out);
    out
}

/// True when the diagnostics contain an error.
pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

fn check_domains(plan: &Plan, out: &mut Vec<Diagnostic>) {
    if plan.allowed_domains.is_empty() {
        out.push(Diagnostic::error(
            "plan.allowed_domains_empty",
            "/allowed_domains",
            "at least one allowed domain is required",
        ));
    }
    for (i, entry) in plan.allowed_domains.iter().enumerate() {
        if !valid_domain_entry(entry) {
            out.push(Diagnostic::error(
                "plan.allowed_domain_invalid",
                &format!("/allowed_domains/{i}"),
                "use a lowercase host name such as files.example.com or *.example.com",
            ));
        }
    }
}

fn check_session(plan: &Plan, out: &mut Vec<Diagnostic>) {
    let Some(session) = &plan.session else { return };
    if !host_allowed(session.origin.host(), &plan.allowed_domains) {
        out.push(Diagnostic::error(
            "plan.session_origin_not_allowed",
            "/session/origin",
            "the session origin must be covered by allowed_domains",
        ));
    }
    match session.scope {
        GrantScope::Never => out.push(Diagnostic::error(
            "plan.session_scope_invalid",
            "/session/scope",
            "scope never cannot be requested",
        )),
        GrantScope::Workflow => {}
        _ => out.push(Diagnostic::warning(
            "plan.session_scope_unusual",
            "/session/scope",
            "workflow plans normally use the workflow scope",
        )),
    }
}

fn check_url(url: &str, path: &str, plan: &Plan, out: &mut Vec<Diagnostic>) {
    match Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {
            let host = parsed.host_str().unwrap_or_default();
            if !host_allowed(host, &plan.allowed_domains) {
                out.push(Diagnostic::error(
                    "plan.url_outside_allowed_domains",
                    path,
                    "the URL's host is not covered by allowed_domains",
                ));
            }
        }
        _ => out.push(Diagnostic::error(
            "plan.url_invalid",
            path,
            "expected an absolute http(s) URL",
        )),
    }
}

fn check_sources(plan: &Plan, out: &mut Vec<Diagnostic>) {
    if plan.sources.is_empty() {
        out.push(Diagnostic::error(
            "plan.sources_empty",
            "/sources",
            "at least one source is required",
        ));
    }
    for (i, source) in plan.sources.iter().enumerate() {
        match source {
            Source::StartUrls { urls } => {
                if urls.is_empty() || urls.len() > MAX_START_URLS {
                    out.push(Diagnostic::error(
                        "plan.start_urls_count",
                        &format!("/sources/{i}/urls"),
                        format!("1 to {MAX_START_URLS} start URLs are allowed"),
                    ));
                }
                for (j, url) in urls.iter().enumerate() {
                    check_url(url, &format!("/sources/{i}/urls/{j}"), plan, out);
                }
            }
            Source::Sitemap { url, loc_pattern } => {
                check_url(url, &format!("/sources/{i}/url"), plan, out);
                if let Some(pattern) = loc_pattern {
                    check_regex(pattern, &format!("/sources/{i}/loc_pattern"), out);
                }
            }
            Source::ApiEndpoint { url, .. } => {
                check_url(url, &format!("/sources/{i}/url"), plan, out);
            }
        }
    }
}

fn check_pagination(plan: &Plan, out: &mut Vec<Diagnostic>) {
    let Some(pagination) = &plan.pagination else {
        return;
    };
    let limit = |pages: u32, path: &str, out: &mut Vec<Diagnostic>| {
        if pages == 0 || pages > plan.budgets.max_pages {
            out.push(Diagnostic::error(
                "plan.pagination_max_pages",
                path,
                "max_pages must be between 1 and budgets.max_pages",
            ));
        }
    };
    match pagination {
        Pagination::RelNext { max_pages } => limit(*max_pages, "/pagination/max_pages", out),
        Pagination::PageParam {
            param,
            step,
            max_pages,
            ..
        } => {
            limit(*max_pages, "/pagination/max_pages", out);
            check_param(param, "/pagination/param", out);
            if *step == 0 {
                out.push(Diagnostic::error(
                    "plan.pagination_step",
                    "/pagination/step",
                    "step must be at least 1",
                ));
            }
        }
        Pagination::OffsetLimit {
            offset_param,
            limit_param,
            limit: size,
            max_pages,
            ..
        } => {
            limit(*max_pages, "/pagination/max_pages", out);
            check_param(offset_param, "/pagination/offset_param", out);
            check_param(limit_param, "/pagination/limit_param", out);
            if *size == 0 || *size > 1000 {
                out.push(Diagnostic::error(
                    "plan.pagination_limit",
                    "/pagination/limit",
                    "limit must be 1 to 1000",
                ));
            }
        }
        Pagination::Cursor {
            cursor_path,
            param,
            max_pages,
        } => {
            limit(*max_pages, "/pagination/max_pages", out);
            check_param(param, "/pagination/param", out);
            check_selector(
                &Selector::JsonPath {
                    query: cursor_path.clone(),
                },
                "/pagination/cursor_path",
                out,
            );
        }
        Pagination::NextSelector {
            selector,
            max_pages,
        } => {
            limit(*max_pages, "/pagination/max_pages", out);
            check_selector(selector, "/pagination/selector", out);
        }
        Pagination::InfiniteScroll {
            max_scrolls,
            settle_ms,
        } => {
            if *max_scrolls == 0 || *max_scrolls > 1000 || *settle_ms > 60_000 {
                out.push(Diagnostic::error(
                    "plan.pagination_scroll",
                    "/pagination",
                    "max_scrolls must be 1 to 1000 and settle_ms at most 60000",
                ));
            }
            out.push(Diagnostic::info(
                "plan.needs_browser",
                "/pagination",
                "infinite scroll runs in the user's browser and needs the extension",
            ));
        }
        Pagination::DateRange {
            from_param,
            to_param,
            from,
            to,
            chunk_days,
            inner,
            ..
        } => {
            if matches!(
                inner.as_ref(),
                Pagination::DateRange { .. } | Pagination::InfiniteScroll { .. }
            ) {
                out.push(Diagnostic::error(
                    "plan.pagination_inner",
                    "/pagination/inner",
                    "date windows require a bounded inner pagination strategy",
                ));
            }
            let mut inner_plan = plan.clone();
            inner_plan.pagination = Some(*inner.clone());
            check_pagination(&inner_plan, out);
            check_param(from_param, "/pagination/from_param", out);
            check_param(to_param, "/pagination/to_param", out);
            if !valid_ymd(from) || !valid_ymd(to) || from > to {
                out.push(Diagnostic::error(
                    "plan.pagination_dates",
                    "/pagination",
                    "from and to must be YYYY-MM-DD dates with from not after to",
                ));
            }
            if *chunk_days == 0 || *chunk_days > 366 {
                out.push(Diagnostic::error(
                    "plan.pagination_chunk",
                    "/pagination/chunk_days",
                    "chunk_days must be 1 to 366",
                ));
            }
        }
    }
}

fn check_param(name: &str, path: &str, out: &mut Vec<Diagnostic>) {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'[' | b']'));
    if !ok {
        out.push(Diagnostic::error(
            "plan.parameter_invalid",
            path,
            "parameter names use letters, digits and _ - . [ ]",
        ));
    }
}

fn valid_ymd(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && matches!(text[5..7].parse::<u8>(), Ok(1..=12))
        && matches!(text[8..10].parse::<u8>(), Ok(1..=31))
}

fn check_extract(plan: &Plan, out: &mut Vec<Diagnostic>) {
    check_selector(&plan.extract.items, "/extract/items", out);
    if plan.extract.fields.is_empty() || plan.extract.fields.len() > MAX_FIELDS {
        out.push(Diagnostic::error(
            "plan.fields_count",
            "/extract/fields",
            format!("1 to {MAX_FIELDS} fields are required"),
        ));
    }
    let detail_url = plan
        .extract
        .follow
        .as_ref()
        .is_some_and(|f| f.fields.contains_key("url"));
    if plan.extract.fields.contains_key("url") == detail_url {
        out.push(Diagnostic::error(
            "plan.field_url_missing",
            "/extract/fields/url",
            "a field named url (the download URL) is required",
        ));
    }
    for (name, field) in &plan.extract.fields {
        if !valid_field_name(name) {
            out.push(Diagnostic::error(
                "plan.field_name_invalid",
                &format!("/extract/fields/{name}"),
                "field names are [a-z][a-z0-9_]{0,31}",
            ));
        }
        if let Some(selector) = &field.selector {
            check_selector(selector, &format!("/extract/fields/{name}/selector"), out);
        }
        if let Some(source) = &field.from_field
            && (!matches!(field.selector, Some(Selector::Regex { .. }))
                || plan
                    .extract
                    .fields
                    .get(source)
                    .is_none_or(|f| f.from_field.is_some()))
        {
            out.push(Diagnostic::error(
                "plan.from_field_invalid",
                &format!("/extract/fields/{name}"),
                "from_field requires a regex and an independent declared source field",
            ));
        }
    }
}

fn check_filters(plan: &Plan, out: &mut Vec<Diagnostic>) {
    for (i, filter) in plan.filters.iter().enumerate() {
        let base = format!("/filters/{i}");
        match filter {
            Filter::UrlRegex { pattern, .. } => {
                check_regex(pattern, &format!("{base}/pattern"), out)
            }
            Filter::FieldRegex { field, pattern, .. } => {
                check_regex(pattern, &format!("{base}/pattern"), out);
                if !plan.extract.fields.contains_key(field)
                    && !plan
                        .extract
                        .follow
                        .as_ref()
                        .is_some_and(|f| f.fields.contains_key(field))
                {
                    out.push(Diagnostic::error(
                        "plan.filter_field_unknown",
                        &format!("{base}/field"),
                        "the filter names a field that extract.fields does not define",
                    ));
                }
            }
            Filter::Mime { allow, .. } => {
                if allow.is_empty()
                    || allow
                        .iter()
                        .any(|m| m.is_empty() || m.len() > 127 || !m.contains('/'))
                {
                    out.push(Diagnostic::error(
                        "plan.filter_mime",
                        &format!("{base}/allow"),
                        "use type/subtype or type/*",
                    ));
                }
            }
            Filter::MaxSize { bytes } | Filter::MinSize { bytes } => {
                if *bytes > MAX_BYTES_HARD {
                    out.push(Diagnostic::error(
                        "plan.filter_size",
                        &format!("{base}/bytes"),
                        "size is out of range",
                    ));
                }
            }
            Filter::Dedupe { key } => check_template(key, plan, &format!("{base}/key"), out),
        }
    }
}

fn check_fetch(plan: &Plan, out: &mut Vec<Diagnostic>) {
    if !(MIN_INTERVAL_FLOOR_MS..=3_600_000).contains(&plan.fetch.min_interval_ms) {
        out.push(Diagnostic::error(
            "plan.fetch_interval",
            "/fetch/min_interval_ms",
            format!("min_interval_ms must be at least {MIN_INTERVAL_FLOOR_MS}"),
        ));
    }
    if !(1..=8).contains(&plan.fetch.max_concurrency_per_host) {
        out.push(Diagnostic::error(
            "plan.fetch_concurrency",
            "/fetch/max_concurrency_per_host",
            "max_concurrency_per_host must be 1 to 8",
        ));
    }
    if !plan.fetch.respect_robots {
        out.push(Diagnostic::error(
            "plan.robots_override_unsupported",
            "/fetch/respect_robots",
            "workflow version 0 requires robots.txt enforcement",
        ));
    }
}

fn check_budgets(plan: &Plan, out: &mut Vec<Diagnostic>) {
    let b = &plan.budgets;
    let mut bad = |name: &str, ok: bool| {
        if !ok {
            out.push(Diagnostic::error(
                "plan.budget_invalid",
                &format!("/budgets/{name}"),
                format!("{name} must be at least 1 and within the hard maximum"),
            ));
        }
    };
    bad("max_pages", (1..=MAX_PAGES_HARD).contains(&b.max_pages));
    bad("max_files", (1..=MAX_FILES_HARD).contains(&b.max_files));
    bad("max_bytes", (1..=MAX_BYTES_HARD).contains(&b.max_bytes));
    bad(
        "max_duration_ms",
        (1..=MAX_DURATION_HARD_MS).contains(&b.max_duration_ms),
    );
    bad("max_depth", (1..=MAX_DEPTH_HARD).contains(&b.max_depth));
}

fn check_post_and_sinks(plan: &Plan, out: &mut Vec<Diagnostic>) {
    if let Some(rename) = &plan.post.rename {
        check_template(rename, plan, "/post/rename", out);
    }
    if let Some(subdir) = &plan.post.subdir {
        check_template(subdir, plan, "/post/subdir", out);
    }
    for (i, step) in plan.post.convert.iter().enumerate() {
        if step.from.is_empty() || step.to.is_empty() || step.to.contains(['.', '/', '\\']) {
            out.push(Diagnostic::error(
                "plan.convert_invalid",
                &format!("/post/convert/{i}"),
                "name source extensions and a target extension without dots or separators",
            ));
        }
    }
    let dir = &plan.sinks.dest_dir;
    let traversal = dir.split(['/', '\\']).any(|part| part == "..") || dir.contains('\0');
    if dir.is_empty() || dir.len() > 4096 || traversal {
        out.push(Diagnostic::error(
            "plan.dest_dir_invalid",
            "/sinks/dest_dir",
            "dest_dir must be non-empty, without .. components, and inside a configured write root",
        ));
    }
}

fn check_template(text: &str, plan: &Plan, path: &str, out: &mut Vec<Diagnostic>) {
    match Template::parse(text) {
        Err(error) => out.push(Diagnostic::error(
            "plan.template_invalid",
            path,
            error.to_string(),
        )),
        Ok(template) => {
            for field in template.fields() {
                if !plan.extract.fields.contains_key(field)
                    && !plan
                        .extract
                        .follow
                        .as_ref()
                        .is_some_and(|f| f.fields.contains_key(field))
                    && !BUILTIN_FIELDS.contains(&field)
                {
                    out.push(Diagnostic::error(
                        "plan.template_unknown_field",
                        path,
                        format!("the template reads `{field}`, which is not a declared field or a built-in"),
                    ));
                }
            }
        }
    }
}

fn check_selector(selector: &Selector, path: &str, out: &mut Vec<Diagnostic>) {
    let text = match selector {
        Selector::Css { query } | Selector::JsonPath { query } | Selector::Xpath { query } => query,
        Selector::Regex { pattern, .. } => pattern,
    };
    if text.is_empty() || text.len() > MAX_SELECTOR_BYTES {
        out.push(Diagnostic::error(
            "plan.selector_invalid",
            path,
            "selectors are 1 to 1024 bytes",
        ));
        return;
    }
    match selector {
        Selector::Xpath { .. } => out.push(Diagnostic::error(
            "plan.selector_unsupported",
            path,
            "xpath is reserved and not supported in plan version 0",
        )),
        Selector::Css { query } => {
            #[cfg(feature = "runtime")]
            if scraper::Selector::parse(query).is_err() {
                out.push(Diagnostic::error(
                    "plan.selector_syntax",
                    path,
                    "not a valid CSS selector",
                ));
            }
            #[cfg(not(feature = "runtime"))]
            let _ = query;
        }
        Selector::JsonPath { query } => {
            if !query.starts_with('$') {
                out.push(Diagnostic::error(
                    "plan.selector_syntax",
                    path,
                    "a JSONPath expression starts with $",
                ));
            }
            #[cfg(feature = "runtime")]
            if query.starts_with('$') && jsonpath_rust::parser::parse_json_path(query).is_err() {
                out.push(Diagnostic::error(
                    "plan.selector_syntax",
                    path,
                    "not a valid JSONPath expression",
                ));
            }
        }
        Selector::Regex { pattern, group } => check_regex_group(pattern, *group, path, out),
    }
}

fn check_regex(pattern: &str, path: &str, out: &mut Vec<Diagnostic>) {
    if pattern.is_empty() || pattern.len() > MAX_SELECTOR_BYTES {
        out.push(Diagnostic::error(
            "plan.pattern_invalid",
            path,
            "patterns are 1 to 1024 bytes",
        ));
        return;
    }
    check_regex_group(pattern, 0, path, out);
}

#[cfg(feature = "runtime")]
fn check_regex_group(pattern: &str, group: u32, path: &str, out: &mut Vec<Diagnostic>) {
    match regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
    {
        Err(_) => out.push(Diagnostic::error(
            "plan.pattern_syntax",
            path,
            "not a valid regular expression",
        )),
        Ok(re) => {
            if group as usize >= re.captures_len() {
                out.push(Diagnostic::error(
                    "plan.pattern_group",
                    path,
                    "the capture group does not exist",
                ));
            }
        }
    }
}

#[cfg(not(feature = "runtime"))]
fn check_regex_group(_pattern: &str, _group: u32, _path: &str, _out: &mut Vec<Diagnostic>) {}

/// Validate selectors against page samples. Requires the `runtime` feature; without it the result is a single
/// warning saying the selectors were not checked.
pub fn validate_samples(plan: &Plan, samples: &[PageSample]) -> Vec<Diagnostic> {
    #[cfg(feature = "runtime")]
    {
        samples_runtime::check(plan, samples)
    }
    #[cfg(not(feature = "runtime"))]
    {
        let _ = (plan, samples);
        vec![Diagnostic::warning(
            "plan.samples_unchecked",
            "/extract",
            "selectors were not checked against samples (runtime feature disabled)",
        )]
    }
}

#[cfg(feature = "runtime")]
mod samples_runtime {
    use super::*;
    use crate::plan::Extraction;

    pub(super) fn check(plan: &Plan, samples: &[PageSample]) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if samples.is_empty() {
            out.push(Diagnostic::error(
                "plan.no_samples",
                "/extract/items",
                "at least one page sample is needed to validate selectors",
            ));
            return out;
        }
        let mut matched_any = false;
        for (index, sample) in samples.iter().enumerate() {
            match count_items(&plan.extract.items, sample) {
                None => out.push(Diagnostic::info(
                    "plan.sample_format_mismatch",
                    "/extract/items",
                    format!("sample {index} is not in the format the item selector reads"),
                )),
                Some(0) => out.push(Diagnostic::error(
                    "plan.selector_no_match",
                    "/extract/items",
                    format!("the item selector matched nothing on sample {index}"),
                )),
                Some(n) if n > MAX_ITEMS_PER_PAGE => out.push(Diagnostic::error(
                    "plan.selector_too_broad",
                    "/extract/items",
                    format!("the item selector matched {n} nodes on sample {index}; the limit is {MAX_ITEMS_PER_PAGE}"),
                )),
                Some(_) => matched_any = true,
            }
            if let Selector::Css { .. } = &plan.extract.items {
                check_css_fields(plan, index, sample, &mut out);
            }
        }
        if !matched_any && !has_errors(&out) {
            out.push(Diagnostic::error(
                "plan.selector_no_match",
                "/extract/items",
                "no sample was in a format the item selector can read",
            ));
        }
        out
    }

    fn count_items(selector: &Selector, sample: &PageSample) -> Option<usize> {
        match selector {
            Selector::Css { query } => {
                let parsed = scraper::Selector::parse(query).ok()?;
                let html = scraper::Html::parse_document(&sample.body);
                Some(html.select(&parsed).count())
            }
            Selector::JsonPath { query } => {
                use jsonpath_rust::JsonPath;
                let value: serde_json::Value = serde_json::from_str(&sample.body).ok()?;
                Some(value.query(query).map(|hits| hits.len()).unwrap_or(0))
            }
            Selector::Regex { pattern, .. } => {
                let re = regex::RegexBuilder::new(pattern)
                    .size_limit(1 << 20)
                    .build()
                    .ok()?;
                Some(re.find_iter(&sample.body).count())
            }
            Selector::Xpath { .. } => None,
        }
    }

    fn check_css_fields(plan: &Plan, index: usize, sample: &PageSample, out: &mut Vec<Diagnostic>) {
        let Selector::Css { query } = &plan.extract.items else {
            return;
        };
        let Ok(item_selector) = scraper::Selector::parse(query) else {
            return;
        };
        let html = scraper::Html::parse_document(&sample.body);
        let items: Vec<_> = html.select(&item_selector).collect();
        if items.is_empty() {
            return;
        }
        for (name, field) in &plan.extract.fields {
            let Some(Selector::Css { query }) = &field.selector else {
                continue;
            };
            let Ok(field_selector) = scraper::Selector::parse(query) else {
                continue;
            };
            let with_match = items
                .iter()
                .filter(|item| {
                    item.select(&field_selector)
                        .any(|node| match &field.extract {
                            Extraction::Text => node.text().any(|t| !t.trim().is_empty()),
                            Extraction::Attr { name } => {
                                node.attr(name).is_some_and(|v| !v.is_empty())
                            }
                        })
                })
                .count();
            if with_match == 0 && field.required {
                out.push(Diagnostic::error(
                    "plan.field_no_match",
                    &format!("/extract/fields/{name}"),
                    format!(
                        "the required field matched in none of {} items on sample {index}",
                        items.len()
                    ),
                ));
            } else if with_match == 0 {
                out.push(Diagnostic::warning(
                    "plan.field_no_match",
                    &format!("/extract/fields/{name}"),
                    format!(
                        "the field matched in none of {} items on sample {index}",
                        items.len()
                    ),
                ));
            }
        }
    }
}

/// Collect the set of codes in a diagnostics list (handy in tests and for UIs).
pub fn codes(diagnostics: &[Diagnostic]) -> BTreeSet<&str> {
    diagnostics.iter().map(|d| d.code.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{FetchPolicy, example_plan};

    #[test]
    fn the_example_plan_is_valid() {
        let diagnostics = validate_plan(&example_plan());
        assert!(!has_errors(&diagnostics), "{diagnostics:?}");
    }

    #[test]
    fn budgets_and_domains_are_mandatory_in_substance() {
        let mut plan = example_plan();
        plan.allowed_domains.clear();
        plan.budgets.max_files = 0;
        plan.budgets.max_bytes = MAX_BYTES_HARD + 1;
        let found = codes(&validate_plan(&plan))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(found.contains(&"plan.allowed_domains_empty".to_string()));
        assert!(found.contains(&"plan.budget_invalid".to_string()));
    }

    #[test]
    fn urls_outside_allowed_domains_are_rejected() {
        let mut plan = example_plan();
        plan.sources = vec![Source::StartUrls {
            urls: vec!["https://evil.example.net/x".into()],
        }];
        assert!(codes(&validate_plan(&plan)).contains("plan.url_outside_allowed_domains"));
    }

    #[test]
    fn wildcard_matches_subdomains_only() {
        let allowed = vec!["*.example.com".to_string(), "files.example.org".to_string()];
        assert!(host_allowed("a.example.com", &allowed));
        assert!(host_allowed("a.b.example.com", &allowed));
        assert!(!host_allowed("example.com", &allowed));
        assert!(!host_allowed("notexample.com", &allowed));
        assert!(host_allowed("FILES.example.org", &allowed));
        assert!(!host_allowed("x.files.example.org", &allowed));
    }

    #[test]
    fn politeness_floor_and_robots() {
        let mut plan = example_plan();
        plan.fetch = FetchPolicy {
            min_interval_ms: 50,
            max_concurrency_per_host: 9,
            respect_robots: false,
            max_connections_per_file: 1,
            priority: zeus_types::Priority::Low,
            download_accept: None,
        };
        let found = codes(&validate_plan(&plan))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(found.contains(&"plan.fetch_interval".to_string()));
        assert!(found.contains(&"plan.fetch_concurrency".to_string()));
        assert!(found.contains(&"plan.robots_override_unsupported".to_string()));
    }

    #[test]
    fn templates_must_reference_known_fields() {
        let mut plan = example_plan();
        plan.post.rename = Some("{nope}.pdf".into());
        assert!(codes(&validate_plan(&plan)).contains("plan.template_unknown_field"));
        plan.post.rename = Some("{url|nope}".into());
        assert!(codes(&validate_plan(&plan)).contains("plan.template_invalid"));
    }

    #[test]
    fn xpath_and_traversal_are_rejected() {
        let mut plan = example_plan();
        plan.extract.items = Selector::Xpath {
            query: "//li".into(),
        };
        plan.sinks.dest_dir = "../outside".into();
        let found = codes(&validate_plan(&plan))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(found.contains(&"plan.selector_unsupported".to_string()));
        assert!(found.contains(&"plan.dest_dir_invalid".to_string()));
    }

    #[cfg(feature = "runtime")]
    mod runtime {
        use super::*;

        fn sample(body: &str) -> PageSample {
            PageSample {
                url_redacted: "https://portal.example.org/papers?…".into(),
                content_type: Some("text/html".into()),
                body: body.into(),
            }
        }

        #[test]
        fn bad_syntax_is_found() {
            let mut plan = example_plan();
            plan.extract.items = Selector::Css {
                query: "li[".into(),
            };
            plan.filters = vec![Filter::UrlRegex {
                pattern: "(".into(),
                invert: false,
            }];
            let found = codes(&validate_plan(&plan))
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            assert!(found.contains(&"plan.selector_syntax".to_string()));
            assert!(found.contains(&"plan.pattern_syntax".to_string()));
        }

        #[test]
        fn matching_samples_pass_and_missing_ones_fail() {
            let plan = example_plan();
            let good = sample(
                r#"<ul><li class="paper"><h3>A</h3><a class="pdf" href="/a.pdf">pdf</a></li></ul>"#,
            );
            assert!(!has_errors(&validate_samples(&plan, &[good])));

            let none = sample("<ul><li class=\"other\">x</li></ul>");
            assert!(codes(&validate_samples(&plan, &[none])).contains("plan.selector_no_match"));

            let no_url = sample(r#"<ul><li class="paper"><h3>A</h3></li></ul>"#);
            assert!(codes(&validate_samples(&plan, &[no_url])).contains("plan.field_no_match"));

            assert!(codes(&validate_samples(&plan, &[])).contains("plan.no_samples"));
        }

        #[test]
        fn too_broad_selectors_are_rejected() {
            let mut plan = example_plan();
            plan.extract.items = Selector::Css { query: "i".into() };
            let body = "<i></i>".repeat(MAX_ITEMS_PER_PAGE + 1);
            assert!(
                codes(&validate_samples(&plan, &[sample(&body)]))
                    .contains("plan.selector_too_broad")
            );
        }

        #[test]
        fn json_path_items_are_counted() {
            let mut plan = example_plan();
            plan.extract.items = Selector::JsonPath {
                query: "$.items[*]".into(),
            };
            let json = PageSample {
                url_redacted: "x".into(),
                content_type: Some("application/json".into()),
                body: r#"{"items":[{"a":1},{"a":2}]}"#.into(),
            };
            let found = validate_samples(&plan, &[json]);
            assert!(!codes(&found).contains("plan.selector_no_match"));
        }
    }
}
