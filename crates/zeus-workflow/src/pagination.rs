//! Deterministic bounded pagination with byte-preserving query replacement.
use crate::{
    extract,
    plan::{DateFormat, DateOrder, Pagination, Plan, Selector, Source},
    report::Diagnostic,
};
use serde::{Deserialize, Serialize};
use url::Url;

/// One serializable discovery chain. Full URLs belong only in private durable state.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    /// Source position.
    pub source_index: usize,
    /// First URL after date-window substitution.
    pub first_url: String,
    /// Next document.
    pub next_url: Option<String>,
    /// Resource depth; pagination preserves it.
    pub depth: u8,
    /// Documents durably processed in this chain.
    pub pages: u32,
    /// Pagination local to this chain.
    pub pagination: Option<Pagination>,
    /// Hashes of full document URLs, for loop detection.
    pub visited: std::collections::BTreeSet<String>,
    /// Last 64 item fingerprints.
    pub fingerprints: Vec<String>,
    /// Consecutive documents with no new item.
    pub empty_pages: u32,
    /// Explicit count hint from the first document.
    pub count_hint: Option<u64>,
    /// Number of item matches so far.
    pub items_seen: u64,
}
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::error("plan.pagination_invalid", "/pagination", message)
}
fn encoded(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char)
        } else {
            out.push_str(&format!("%{b:02X}"))
        }
    }
    out
}
/// Set only the selected query member, preserving every other byte and dropping duplicate members.
pub fn set_parameter(input: &str, name: &str, value: &str) -> String {
    let input = input.split('#').next().unwrap_or(input);
    let (base, query) = input.split_once('?').unwrap_or((input, ""));
    let mut replaced = false;
    let mut pairs = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let raw = pair.split('=').next().unwrap_or(pair);
        let decoded = url::form_urlencoded::parse(format!("{raw}=").as_bytes())
            .next()
            .map(|(k, _)| k.into_owned())
            .unwrap_or_default();
        if decoded == name {
            if !replaced {
                pairs.push(format!("{}={}", raw, encoded(value)));
                replaced = true;
            }
        } else {
            pairs.push(pair.into());
        }
    }
    if !replaced {
        pairs.push(format!("{}={}", encoded(name), encoded(value)));
    }
    format!("{base}?{}", pairs.join("&"))
}
fn synthetic(first: &str, p: &Pagination, index: u32) -> Option<String> {
    match p {
        Pagination::PageParam {
            param,
            start,
            step,
            end,
            max_pages,
        } => {
            if index >= *max_pages {
                return None;
            }
            let value = start.checked_add(index.checked_mul(*step)?)?;
            if end.is_some_and(|e| value > e) {
                return None;
            }
            Some(set_parameter(first, param, &value.to_string()))
        }
        Pagination::OffsetLimit {
            offset_param,
            limit_param,
            limit,
            start_offset,
            max_pages,
        } => {
            if index >= *max_pages {
                return None;
            }
            let offset = start_offset.checked_add(index.checked_mul(*limit)?)?;
            Some(set_parameter(
                &set_parameter(first, offset_param, &offset.to_string()),
                limit_param,
                &limit.to_string(),
            ))
        }
        _ => Some(first.into()),
    }
}
fn chain(source_index: usize, url: String, pagination: Option<Pagination>) -> Chain {
    let next_url = pagination
        .as_ref()
        .and_then(|p| synthetic(&url, p, 0))
        .or_else(|| Some(url.clone()));
    Chain {
        source_index,
        first_url: url,
        next_url,
        depth: 0,
        pages: 0,
        pagination,
        visited: Default::default(),
        fingerprints: vec![],
        empty_pages: 0,
        count_hint: None,
        items_seen: 0,
    }
}
/// Build a bounded frontier in source order; unsupported sources fail before network access.
pub fn frontier(plan: &Plan) -> Result<Vec<Chain>, Diagnostic> {
    let mut chains = Vec::new();
    for (index, source) in plan.sources.iter().enumerate() {
        let urls = match source {
            Source::StartUrls { urls } => urls.clone(),
            Source::ApiEndpoint { url, .. } => vec![url.clone()],
            Source::Sitemap { .. } => {
                return Err(Diagnostic::error(
                    "workflow.capability_missing",
                    "/sources",
                    "sitemap execution requires an XML document provider",
                ));
            }
        };
        for url in urls {
            if let Some(Pagination::DateRange {
                from_param,
                to_param,
                from,
                to,
                chunk_days,
                date_format,
                order,
                inner,
            }) = &plan.pagination
            {
                let format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")
                    .map_err(|_| invalid("invalid date format"))?;
                let mut date =
                    time::Date::parse(from, &format).map_err(|_| invalid("invalid date"))?;
                let last = time::Date::parse(to, &format).map_err(|_| invalid("invalid date"))?;
                let mut windows = Vec::new();
                if *order == DateOrder::Descending {
                    let days = (last - date).whole_days();
                    let count = days / i64::from(*chunk_days);
                    date = date
                        .checked_add(time::Duration::days(count * i64::from(*chunk_days)))
                        .ok_or_else(|| invalid("date overflow"))?;
                    let first =
                        time::Date::parse(from, &format).map_err(|_| invalid("invalid date"))?;
                    while date >= first && windows.len() < plan.budgets.max_pages as usize {
                        let end = date
                            .checked_add(time::Duration::days(i64::from(*chunk_days) - 1))
                            .unwrap_or(last)
                            .min(last);
                        windows.push((date, end));
                        let Some(previous) =
                            date.checked_sub(time::Duration::days(i64::from(*chunk_days)))
                        else {
                            break;
                        };
                        date = previous;
                    }
                } else {
                    while date <= last && windows.len() < plan.budgets.max_pages as usize {
                        let end = date
                            .checked_add(time::Duration::days(i64::from(*chunk_days) - 1))
                            .unwrap_or(last)
                            .min(last);
                        windows.push((date, end));
                        let Some(next) = end.next_day() else { break };
                        date = next;
                    }
                }
                for (start, end) in windows {
                    let render = |d: time::Date| {
                        let s = d.to_string();
                        if *date_format == DateFormat::BasicDate {
                            s.replace('-', "")
                        } else {
                            s
                        }
                    };
                    let u = set_parameter(
                        &set_parameter(&url, from_param, &render(start)),
                        to_param,
                        &render(end),
                    );
                    chains.push(chain(index, u, Some(*inner.clone())));
                }
            } else {
                if matches!(plan.pagination, Some(Pagination::InfiniteScroll { .. })) {
                    return Err(Diagnostic::error(
                        "workflow.capability_missing",
                        "/pagination",
                        "browser page rendering is not available",
                    ));
                }
                chains.push(chain(index, url, plan.pagination.clone()));
            }
            if chains.len() > 100_000 {
                return Err(invalid("frontier exceeds bounded capacity"));
            }
        }
    }
    Ok(chains)
}
/// Compute the next URL; the caller enforces domain and previously-seen URL guards.
pub(crate) fn next(
    chain: &Chain,
    body: &str,
    base: &Url,
    headers: &std::collections::BTreeMap<String, String>,
    items: usize,
) -> Option<String> {
    let pagination = chain.pagination.as_ref()?;
    let max = match pagination {
        Pagination::RelNext { max_pages }
        | Pagination::PageParam { max_pages, .. }
        | Pagination::OffsetLimit { max_pages, .. }
        | Pagination::Cursor { max_pages, .. }
        | Pagination::NextSelector { max_pages, .. } => *max_pages,
        _ => return None,
    };
    if chain.pages >= max {
        return None;
    }
    match pagination {
        Pagination::PageParam { .. } => {
            if items == 0 {
                None
            } else {
                synthetic(&chain.first_url, pagination, chain.pages)
            }
        }
        Pagination::OffsetLimit { limit, .. } => {
            if items < *limit as usize {
                None
            } else {
                synthetic(&chain.first_url, pagination, chain.pages)
            }
        }
        Pagination::Cursor {
            cursor_path, param, ..
        } => {
            let cursor = extract::document_value(
                body,
                &Selector::JsonPath {
                    query: cursor_path.clone(),
                },
                false,
            )?;
            if cursor.is_empty() {
                None
            } else {
                Some(set_parameter(&chain.first_url, param, &cursor))
            }
        }
        Pagination::NextSelector { selector, .. } => {
            extract::document_value(body, selector, true).and_then(|v| extract::resolve(base, &v))
        }
        Pagination::RelNext { .. } => {
            if let Some(link) = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("link"))
                .map(|(_, v)| v)
            {
                for entry in link.split(',') {
                    let mut parts = entry.split(';');
                    let target = parts
                        .next()?
                        .trim()
                        .strip_prefix('<')
                        .and_then(|v| v.strip_suffix('>'));
                    let rel = parts.any(|p| {
                        p.trim().split_once('=').is_some_and(|(k, v)| {
                            k.eq_ignore_ascii_case("rel")
                                && v.trim_matches(['\"', '\'', ' '])
                                    .split_whitespace()
                                    .any(|v| v.eq_ignore_ascii_case("next"))
                        })
                    });
                    if rel && let Some(value) = target.and_then(|v| extract::resolve(base, v)) {
                        return Some(value);
                    }
                }
            }
            let html = scraper::Html::parse_document(body);
            for tag in ["link[rel][href]", "a[rel][href]"] {
                let selector = scraper::Selector::parse(tag).ok()?;
                for node in html.select(&selector) {
                    if node.attr("rel").is_some_and(|v| {
                        v.split_whitespace().any(|v| v.eq_ignore_ascii_case("next"))
                    }) {
                        return node.attr("href").and_then(|v| extract::resolve(base, v));
                    }
                }
            }
            None
        }
        _ => None,
    }
}
