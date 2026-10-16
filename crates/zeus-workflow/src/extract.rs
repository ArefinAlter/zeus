//! Bounded declarative extraction. Page text is always data.
use crate::{
    plan::{Extraction, Field, Selector},
    report::Diagnostic,
};
use indexmap::IndexMap;
use jsonpath_rust::JsonPath;
use scraper::{ElementRef, Html};
use serde_json::Value;
use std::collections::BTreeMap;
use url::Url;

pub(crate) type Fields = BTreeMap<String, String>;
pub(crate) struct Extracted {
    pub items: Vec<Fields>,
    pub total: usize,
    pub invalid: usize,
    pub base: Url,
}
enum Node<'a> {
    Html(ElementRef<'a>),
    Json(&'a Value),
    Text(&'a str),
}
fn text(node: &Node<'_>) -> String {
    match node {
        Node::Html(n) => n.text().collect::<Vec<_>>().join(" "),
        Node::Json(v) => scalar(v).unwrap_or_else(|| v.to_string()),
        Node::Text(s) => (*s).into(),
    }
}
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(v) => Some(v.to_string()),
        Value::Number(v) => Some(v.to_string()),
        _ => None,
    }
}
pub(crate) fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .filter(|c| !matches!(*c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(4096)
        .collect()
}
fn regex(pattern: &str) -> Result<regex::Regex, Diagnostic> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map_err(|_| Diagnostic::error("plan.selector_syntax", "/extract", "invalid bounded regex"))
}
fn pick(
    node: &Node<'_>,
    selector: Option<&Selector>,
    extraction: &Extraction,
) -> Result<Option<String>, Diagnostic> {
    let Some(selector) = selector else {
        return Ok(match (node, extraction) {
            (Node::Html(n), Extraction::Attr { name }) => n.attr(name).map(str::to_owned),
            (_, Extraction::Text) => Some(text(node)),
            _ => None,
        });
    };
    Ok(match selector {
        Selector::Css { query } => {
            let Node::Html(n) = node else { return Ok(None) };
            let selector = scraper::Selector::parse(query).map_err(|_| {
                Diagnostic::error("plan.selector_syntax", "/extract", "invalid CSS selector")
            })?;
            n.select(&selector).next().and_then(|n| match extraction {
                Extraction::Text => Some(n.text().collect::<Vec<_>>().join(" ")),
                Extraction::Attr { name } => n.attr(name).map(str::to_owned),
            })
        }
        Selector::JsonPath { query } => {
            let Node::Json(v) = node else { return Ok(None) };
            v.query(query)
                .ok()
                .and_then(|hits| hits.first().and_then(|v| scalar(v)))
        }
        Selector::Regex { pattern, group } => regex(pattern)?
            .captures(&text(node))
            .and_then(|m| m.get(*group as usize).map(|m| m.as_str().to_owned())),
        Selector::Xpath { .. } => {
            return Err(Diagnostic::error(
                "plan.selector_unsupported",
                "/extract",
                "XPath is not implemented in workflow v0",
            ));
        }
    })
}
pub(crate) fn resolve(base: &Url, value: &str) -> Option<String> {
    let mut url = base.join(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_fragment(None);
    Some(url.into())
}
fn fields(
    node: &Node<'_>,
    map: &IndexMap<String, Field>,
    base: &Url,
) -> Result<Option<Fields>, Diagnostic> {
    let mut out = Fields::new();
    for dependent in [false, true] {
        for (name, field) in map {
            if field.from_field.is_some() != dependent {
                continue;
            }
            let value = if let Some(source) = &field.from_field {
                out.get(source)
                    .map(|v| pick(&Node::Text(v), field.selector.as_ref(), &field.extract))
                    .transpose()?
                    .flatten()
            } else {
                pick(node, field.selector.as_ref(), &field.extract)?
            };
            let value = value
                .map(|v| clean(&v))
                .filter(|v| !v.is_empty())
                .and_then(|v| {
                    if field.resolve_url || name == "url" {
                        resolve(base, &v)
                    } else {
                        Some(v)
                    }
                });
            if let Some(value) = value {
                out.insert(name.clone(), value);
            } else if field.required || name == "url" {
                return Ok(None);
            }
        }
    }
    Ok(Some(out))
}
pub(crate) fn extract(
    body: &str,
    url: &str,
    selector: &Selector,
    map: &IndexMap<String, Field>,
) -> Result<Extracted, Diagnostic> {
    if body.len() > 8 * 1024 * 1024 {
        return Err(Diagnostic::error(
            "workflow.page_too_large",
            "/",
            "document exceeds 8 MiB",
        ));
    }
    let mut base = Url::parse(url)
        .map_err(|_| Diagnostic::error("plan.url_invalid", "/", "invalid final document URL"))?;
    let mut result = Extracted {
        items: vec![],
        total: 0,
        invalid: 0,
        base: base.clone(),
    };
    match selector {
        Selector::Css { query } => {
            let html = Html::parse_document(body);
            let sel = scraper::Selector::parse(query).map_err(|_| {
                Diagnostic::error(
                    "plan.selector_syntax",
                    "/extract/items",
                    "invalid CSS selector",
                )
            })?;
            if let Some(href) = html
                .select(&scraper::Selector::parse("base[href]").expect("static CSS"))
                .next()
                .and_then(|n| n.attr("href"))
                && let Some(u) = resolve(&base, href)
            {
                base = Url::parse(&u).expect("resolved URL");
            }
            for node in html.select(&sel).take(10_001) {
                result.total += 1;
                if result.total > 10_000 {
                    break;
                }
                match fields(&Node::Html(node), map, &base)? {
                    Some(v) => result.items.push(v),
                    None => result.invalid += 1,
                }
            }
        }
        Selector::JsonPath { query } => {
            let json: Value = serde_json::from_str(body).map_err(|_| {
                Diagnostic::warning(
                    "plan.drift_content_type",
                    "/extract/items",
                    "expected a JSON document",
                )
            })?;
            let nodes = json.query(query).map_err(|_| {
                Diagnostic::error("plan.selector_syntax", "/extract/items", "invalid JSONPath")
            })?;
            for node in nodes.into_iter().take(10_001) {
                result.total += 1;
                if result.total > 10_000 {
                    break;
                }
                match fields(&Node::Json(node), map, &base)? {
                    Some(v) => result.items.push(v),
                    None => result.invalid += 1,
                }
            }
        }
        Selector::Regex { pattern, group } => {
            for captures in regex(pattern)?.captures_iter(body).take(10_001) {
                result.total += 1;
                if result.total > 10_000 {
                    break;
                }
                if let Some(item) = captures.get(*group as usize) {
                    match fields(&Node::Text(item.as_str()), map, &base)? {
                        Some(v) => result.items.push(v),
                        None => result.invalid += 1,
                    }
                } else {
                    result.invalid += 1
                }
            }
        }
        Selector::Xpath { .. } => {
            return Err(Diagnostic::error(
                "plan.selector_unsupported",
                "/extract/items",
                "XPath is not implemented",
            ));
        }
    }
    result.base = base;
    Ok(result)
}
pub(crate) fn detail(
    body: &str,
    url: &str,
    content_type: &str,
    map: &IndexMap<String, Field>,
) -> Result<Option<Fields>, Diagnostic> {
    let base = Url::parse(url).map_err(|_| {
        Diagnostic::error(
            "plan.url_invalid",
            "/extract/follow",
            "invalid document URL",
        )
    })?;
    if body.len() > 8 * 1024 * 1024 {
        return Err(Diagnostic::error(
            "workflow.page_too_large",
            "/extract/follow",
            "document exceeds 8 MiB",
        ));
    }
    if content_type.contains("json") {
        let json: Value = serde_json::from_str(body).map_err(|_| {
            Diagnostic::warning(
                "plan.drift_content_type",
                "/extract/follow",
                "expected JSON",
            )
        })?;
        fields(&Node::Json(&json), map, &base)
    } else {
        let html = Html::parse_document(body);
        fields(&Node::Html(html.root_element()), map, &base)
    }
}
pub(crate) fn document_value(body: &str, selector: &Selector, attribute: bool) -> Option<String> {
    let extraction = if attribute {
        Extraction::Attr {
            name: "href".into(),
        }
    } else {
        Extraction::Text
    };
    match selector {
        Selector::Css { .. } => {
            let html = Html::parse_document(body);
            pick(
                &Node::Html(html.root_element()),
                Some(selector),
                &extraction,
            )
            .ok()
            .flatten()
        }
        Selector::JsonPath { .. } => {
            let json: Value = serde_json::from_str(body).ok()?;
            pick(&Node::Json(&json), Some(selector), &Extraction::Text)
                .ok()
                .flatten()
        }
        _ => pick(&Node::Text(body), Some(selector), &Extraction::Text)
            .ok()
            .flatten(),
    }
}
pub(crate) fn guessed_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "zst" => "application/zstd",
        "json" => "application/json",
        "csv" => "text/csv",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "epub" => "application/epub+zip",
        _ => return None,
    })
}
pub(crate) fn mime_matches(value: &str, allowed: &[String]) -> bool {
    let value = value
        .split(';')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase();
    allowed
        .iter()
        .any(|a| a == &value || a.strip_suffix('*').is_some_and(|p| value.starts_with(p)))
}
