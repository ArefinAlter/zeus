//! Text-free page outlines for model drafting under the `metadata_only` privacy tier (09).
//!
//! An outline keeps what a selector needs and drops what a person wrote: element names, ids, classes, attribute
//! names and link targets without query or fragment, never text nodes or other attribute values. JSON samples
//! become key paths with value types. Runs of siblings with the same signature collapse into one line with a
//! count, and the whole outline is cut at a byte bound.
use std::fmt::Write;

use crate::planner::PageSample;

/// Attributes whose values are kept (link targets, after redaction); every other value is dropped.
const LINK_ATTRIBUTES: &[&str] = &["href", "src"];
/// Elements whose content is never structure for a selector.
const SKIPPED: &[&str] = &["script", "style", "noscript", "template", "svg", "head"];

/// A link's shape: no query or fragment, and the last path segment replaced by `*` (its extension kept), so
/// item lists collapse and file names stay out of the prompt.
fn redact_link(value: &str) -> String {
    let cut = value.find(['?', '#']).unwrap_or(value.len());
    let link = &value[..cut];
    // The path starts after `scheme://host`; a bare origin has no file name to hide.
    let path_start = match link.find("://") {
        Some(i) => link[i + 3..].find('/').map_or(link.len(), |p| i + 3 + p),
        None => 0,
    };
    let shape = match link.rfind('/') {
        Some(slash) if slash >= path_start && slash + 1 < link.len() => {
            let last = &link[slash + 1..];
            match last.rfind('.') {
                Some(dot) if dot > 0 => format!("{}*{}", &link[..=slash], &last[dot..]),
                _ => format!("{}*", &link[..=slash]),
            }
        }
        _ => link.to_owned(),
    };
    shape.chars().take(120).collect()
}

fn signature(element: &scraper::node::Element) -> String {
    let mut line = element.name().to_owned();
    if let Some(id) = element.id() {
        line.push('#');
        line.extend(id.chars().take(48));
    }
    for class in element.classes().take(6) {
        line.push('.');
        line.extend(class.chars().take(48));
    }
    let mut names = Vec::new();
    for (name, value) in element.attrs() {
        if LINK_ATTRIBUTES.contains(&name) {
            names.push(format!("{name}={}", redact_link(value)));
        } else if name != "id" && name != "class" {
            names.push(name.chars().take(32).collect());
        }
    }
    names.truncate(8);
    if !names.is_empty() {
        line.push_str(" [");
        line.push_str(&names.join(" "));
        line.push(']');
    }
    line
}

fn html_outline(body: &str, max_bytes: usize) -> String {
    let document = scraper::Html::parse_document(body);
    // Each distinct (depth, signature) once, in first-seen order, with how often it occurs: a list of a
    // thousand items is one `li.item x1000` line, which is what a selector needs.
    let mut lines: Vec<((usize, String), usize)> = Vec::new();
    let mut index: std::collections::HashMap<(usize, String), usize> = Default::default();
    let mut bytes = 0usize;
    // Pre-order: each element once, in document order.
    for node in document.root_element().descendants() {
        let Some(element) = node.value().as_element() else {
            continue;
        };
        if node.ancestors().any(|a| {
            a.value()
                .as_element()
                .is_some_and(|e| SKIPPED.contains(&e.name()))
        }) || SKIPPED.contains(&element.name())
        {
            continue;
        }
        let key = (node.ancestors().count(), signature(element));
        if let Some(&at) = index.get(&key) {
            lines[at].1 += 1;
            continue;
        }
        if bytes >= max_bytes {
            continue;
        }
        bytes += key.1.len() + 2 * key.0.min(24) + 1;
        index.insert(key.clone(), lines.len());
        lines.push((key, 1));
    }
    let mut out = String::new();
    for ((depth, line), count) in lines {
        let _ = write!(out, "{}{}", "  ".repeat(depth.min(24)), line);
        if count > 1 {
            let _ = write!(out, " x{count}");
        }
        out.push('\n');
    }
    out
}

fn json_outline(value: &serde_json::Value, path: &str, out: &mut String, max_bytes: usize) {
    if out.len() >= max_bytes {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            let _ = writeln!(out, "{path}: object");
            for (key, child) in map.iter().take(64) {
                let key: String = key.chars().take(48).collect();
                json_outline(child, &format!("{path}.{key}"), out, max_bytes);
            }
        }
        serde_json::Value::Array(items) => {
            let _ = writeln!(out, "{path}: array[{}]", items.len());
            if let Some(first) = items.first() {
                json_outline(first, &format!("{path}[*]"), out, max_bytes);
            }
        }
        serde_json::Value::String(_) => {
            let _ = writeln!(out, "{path}: string");
        }
        serde_json::Value::Number(_) => {
            let _ = writeln!(out, "{path}: number");
        }
        serde_json::Value::Bool(_) => {
            let _ = writeln!(out, "{path}: boolean");
        }
        serde_json::Value::Null => {
            let _ = writeln!(out, "{path}: null");
        }
    }
}

/// The outline of one sample, at most `max_bytes` long (cut at a character boundary).
pub fn outline(sample: &PageSample, max_bytes: usize) -> String {
    let json = sample
        .content_type
        .as_deref()
        .is_some_and(|t| t.to_ascii_lowercase().contains("json"));
    let mut out = match (
        json,
        serde_json::from_str::<serde_json::Value>(&sample.body),
    ) {
        (true, Ok(value)) => {
            let mut out = String::new();
            json_outline(&value, "$", &mut out, max_bytes);
            out
        }
        _ => html_outline(&sample.body, max_bytes),
    };
    if out.len() > max_bytes {
        let mut cut = max_bytes;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(body: &str, content_type: &str) -> PageSample {
        PageSample {
            url_redacted: "https://a.example/list".into(),
            content_type: Some(content_type.into()),
            body: body.into(),
        }
    }

    #[test]
    fn html_outlines_keep_structure_and_links_but_no_text() {
        let html = r#"<html><head><title>ZEUS-CANARY-title</title><script>ZEUS-CANARY-script</script></head>
            <body><ul class="papers">
              <li class="item"><a class="pdf" href="/files/a.pdf?token=ZEUS-CANARY-q" title="ZEUS-CANARY-attr">ZEUS-CANARY-text A</a></li>
              <li class="item"><a class="pdf" href="/files/a.pdf?token=ZEUS-CANARY-q">ZEUS-CANARY-text B</a></li>
            </ul><p data-note="ZEUS-CANARY-data">ZEUS-CANARY-paragraph</p></body></html>"#;
        let outline = outline(&sample(html, "text/html"), 4096);
        assert!(!outline.contains("ZEUS-CANARY"), "{outline}");
        assert!(outline.contains("ul.papers"));
        assert!(
            outline.contains("a.pdf [href=/files/*.pdf title]"),
            "{outline}"
        );
        assert!(
            outline.contains("li.item x2"),
            "repeated items collapse: {outline}"
        );
        assert!(!outline.contains("a.pdf?"), "queries never appear");
        assert!(outline.contains("p [data-note]"));
    }

    #[test]
    fn json_outlines_are_typed_key_paths_without_values() {
        let body = r#"{"results":[{"name":"ZEUS-CANARY-1","download":"https://x/ZEUS-CANARY-2","size":3}],"next":null}"#;
        let outline = outline(&sample(body, "application/json"), 4096);
        assert!(!outline.contains("ZEUS-CANARY"));
        assert!(outline.contains("$.results[*].download: string"));
        assert!(outline.contains("$.results: array[1]"));
    }

    #[test]
    fn link_shapes_hide_file_names_but_keep_origins_and_directories() {
        assert_eq!(
            redact_link("https://a.example/files/2026/report.pdf?x=1"),
            "https://a.example/files/2026/*.pdf"
        );
        assert_eq!(redact_link("https://a.example"), "https://a.example");
        assert_eq!(redact_link("/list/"), "/list/");
        assert_eq!(redact_link("/item/42#top"), "/item/*");
    }

    #[test]
    fn outlines_respect_their_bound() {
        let body = format!(
            "<div>{}</div>",
            "<span class=\"c\"><b></b></span>".repeat(5000)
        );
        assert!(outline(&sample(&body, "text/html"), 256).len() <= 256);
    }
}
