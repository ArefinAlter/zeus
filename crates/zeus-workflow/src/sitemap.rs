//! Bounded, namespace-aware sitemap discovery. This parser never resolves external entities.
//! Protocol: <https://www.sitemaps.org/protocol.html>. The workflow deliberately uses lower caps.
use crate::{
    Diagnostic,
    runtime::{RuntimeError, RuntimeResult},
};
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
use std::collections::BTreeSet;
use std::io::Read;

/// Decode a bounded plain or gzip sitemap as strict UTF-8. XML validation remains in discovery.
/// All gzip members and checksums must finish; trailing junk and oversized expansion are refused.
pub fn decode(bytes: &[u8]) -> RuntimeResult<String> {
    const MAX: usize = 8 * 1024 * 1024;
    if bytes.len() > MAX {
        return Err(RuntimeError::Policy("workflow.page_too_large"));
    }
    let reader: Box<dyn Read + '_> = if bytes.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::MultiGzDecoder::new(bytes))
    } else {
        Box::new(bytes)
    };
    let mut output = Vec::new();
    reader
        .take((MAX + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|_| RuntimeError::Port("workflow.sitemap_encoding"))?;
    if output.len() > MAX {
        return Err(RuntimeError::Policy("workflow.page_too_large"));
    }
    String::from_utf8(output).map_err(|_| RuntimeError::Port("workflow.sitemap_encoding"))
}
const NS: &str = "http://www.sitemaps.org/schemas/sitemap/0.9";
const MAX_LOCATIONS: usize = 10_000;
/// Distinguishes page URLs from nested sitemap documents.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Pages,
    Index,
}
/// Raw URLs remain private and have no Debug implementation.
pub(crate) struct Sitemap {
    pub kind: Kind,
    pub locations: Vec<String>,
}
struct Element {
    name: String,
    core: bool,
    loc_count: u8,
}
fn invalid() -> RuntimeError {
    RuntimeError::Invalid(vec![Diagnostic::error(
        "workflow.sitemap_invalid",
        "/sources",
        "sitemap must be complete bounded UTF-8 XML with absolute HTTP(S) loc entries",
    )])
}
fn text(value: &str, stack: &[Element], loc: &mut Option<String>) -> RuntimeResult<()> {
    if let Some(loc) = loc {
        if stack.len() != 3 || stack.last().is_none_or(|e| !e.core || e.name != "loc") {
            return Err(invalid());
        }
        if loc.len().saturating_add(value.len()) > 8192 {
            return Err(invalid());
        }
        loc.push_str(value);
    } else if stack.is_empty() && !value.trim().is_empty() {
        return Err(invalid());
    }
    Ok(())
}
/// Parse one XML urlset/index; DTDs, unknown entities and off-origin links are refused before fetch.
pub(crate) fn parse(body: &str, base: &str) -> RuntimeResult<Sitemap> {
    if body.len() > 8 * 1024 * 1024 {
        return Err(RuntimeError::Policy("workflow.page_too_large"));
    }
    let base = url::Url::parse(base).map_err(|_| invalid())?;
    let mut reader = NsReader::from_str(body);
    reader.config_mut().expand_empty_elements = true;
    let mut stack: Vec<Element> = Vec::new();
    let (mut root_seen, mut closed, mut count) = (false, false, 0usize);
    let mut kind = None;
    let mut loc = None;
    let mut locations = Vec::new();
    let mut seen = BTreeSet::new();
    let mut rows = 0;
    let mut root_namespace: Option<String> = None;
    loop {
        let (namespace, event) = reader.read_resolved_event().map_err(|_| invalid())?;
        match event {
            Event::Start(e) => {
                count += 1;
                if count > 100_000 || stack.len() >= 32 || closed {
                    return Err(invalid());
                }
                let namespace = match namespace {
                    ResolveResult::Unbound => None,
                    ResolveResult::Bound(ns) => Some(ns.as_ref().to_owned()),
                    ResolveResult::Unknown(_) => return Err(invalid()),
                };
                let core = namespace.as_deref().is_none_or(|ns| ns == NS);
                let name = e.local_name().as_ref().to_owned();
                // Validate duplicate/malformed attributes too; no attribute supplies a fetch target.
                for attribute in e.attributes() {
                    attribute.map_err(|_| invalid())?;
                }
                if loc.is_some() {
                    return Err(invalid());
                }
                if stack.is_empty() {
                    if root_seen || !core {
                        return Err(invalid());
                    }
                    root_seen = true;
                    root_namespace = namespace.clone();
                    kind = Some(match name.as_str() {
                        "urlset" => Kind::Pages,
                        "sitemapindex" => Kind::Index,
                        _ => return Err(invalid()),
                    });
                } else if (core && namespace != root_namespace)
                    || (stack.len() == 1 && matches!(name.as_str(), "url" | "sitemap") && !core)
                {
                    return Err(invalid());
                } else if stack.len() == 1 && core {
                    let expected = if kind == Some(Kind::Pages) {
                        "url"
                    } else {
                        "sitemap"
                    };
                    if name != expected {
                        return Err(invalid());
                    }
                    rows += 1;
                    if rows > MAX_LOCATIONS {
                        return Err(RuntimeError::Policy("workflow.sitemap_limit"));
                    }
                } else if stack.len() == 2 && core && name == "loc" && stack[1].core {
                    if stack[1].loc_count != 0 {
                        return Err(invalid());
                    }
                    stack[1].loc_count = 1;
                    loc = Some(String::new());
                }
                stack.push(Element {
                    name,
                    core,
                    loc_count: 0,
                });
            }
            Event::End(_) => {
                let element = stack.pop().ok_or_else(invalid)?;
                if stack.len() == 2 && element.core && element.name == "loc" {
                    let value = loc.take().ok_or_else(invalid)?;
                    let value = value.trim();
                    if value.is_empty()
                        || value.chars().count() >= 2048
                        || value.chars().any(|c| c.is_control() || c.is_whitespace())
                    {
                        return Err(invalid());
                    }
                    let url = url::Url::parse(value).map_err(|_| invalid())?;
                    if !matches!(url.scheme(), "http" | "https")
                        || url.host_str() != base.host_str()
                        || url.port_or_known_default() != base.port_or_known_default()
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.fragment().is_some()
                        || base.scheme() == "https" && url.scheme() != "https"
                    {
                        return Err(RuntimeError::Policy("workflow.sitemap_scope"));
                    }
                    if seen.insert(value.to_owned()) {
                        locations.push(value.to_owned());
                    }
                }
                if stack.len() == 1
                    && element.core
                    && matches!(element.name.as_str(), "url" | "sitemap")
                    && element.loc_count != 1
                {
                    return Err(invalid());
                }
                if stack.is_empty() {
                    closed = true;
                }
            }
            Event::Text(e) => {
                let value = quick_xml::escape::unescape(e.as_ref()).map_err(|_| invalid())?;
                text(&value, &stack, &mut loc)?;
            }
            Event::CData(e) => text(e.as_ref(), &stack, &mut loc)?,
            Event::GeneralRef(e) => {
                let encoded = format!("&{};", e.as_ref());
                let value = quick_xml::escape::unescape(&encoded).map_err(|_| invalid())?;
                text(&value, &stack, &mut loc)?;
            }
            Event::DocType(_) => {
                return Err(RuntimeError::Policy("workflow.sitemap_dtd_forbidden"));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !root_seen || !closed || !stack.is_empty() || loc.is_some() {
        return Err(invalid());
    }
    Ok(Sitemap {
        kind: kind.ok_or_else(invalid)?,
        locations,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn gzip(input: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut writer = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        writer.write_all(input).unwrap();
        writer.finish().unwrap()
    }
    #[test]
    fn gzip_is_checked_in_full_and_plain_text_is_strict_utf8() {
        let xml = "<urlset><url><loc>https://example.org/a?sig=A%2Bb&amp;x=1</loc></url></urlset>";
        assert_eq!(decode(xml.as_bytes()).unwrap(), xml);
        let packed = gzip(xml.as_bytes());
        assert_eq!(decode(&packed).unwrap(), xml);
        assert_eq!(
            parse(&decode(&packed).unwrap(), "https://example.org/map.xml.gz")
                .unwrap()
                .locations,
            ["https://example.org/a?sig=A%2Bb&x=1"]
        );
        let mut corrupt = packed.clone();
        let end = corrupt.len();
        corrupt[end - 8] ^= 1;
        assert!(decode(&corrupt).is_err());
        assert!(decode(&packed[..packed.len() - 3]).is_err());
        assert!(decode(&[0xff, 0xfe, 0x61]).is_err());
        let mut junk = packed.clone();
        junk.extend_from_slice(b"unparsed-trailing-junk");
        assert!(decode(&junk).is_err());
        let mut members = packed;
        members.extend(gzip(b"<urlset/>"));
        let text = decode(&members).unwrap();
        assert!(text.ends_with("<urlset/>"));
        assert!(parse(&text, "https://example.org/map.xml.gz").is_err());
    }
    #[test]
    fn compressed_expansion_stops_at_the_document_budget() {
        let packed = gzip(&vec![b' '; 8 * 1024 * 1024 + 1]);
        assert!(matches!(
            decode(&packed),
            Err(RuntimeError::Policy("workflow.page_too_large"))
        ));
    }
    #[test]
    fn namespace_entities_and_signed_query_are_preserved() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><s:urlset xmlns:s="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:x="urn:extension"><s:url><s:loc>https://example.org/a?sig=A%2Bb&amp;x=1&#38;y=2</s:loc><x:loc>https://evil.example/ignored</x:loc></s:url><s:url><s:loc><![CDATA[https://example.org/a?sig=A%2Bb&x=1&y=2]]></s:loc></s:url></s:urlset>"#;
        let result = parse(xml, "https://example.org/map.xml").unwrap();
        assert!(result.kind == Kind::Pages);
        assert_eq!(
            result.locations,
            ["https://example.org/a?sig=A%2Bb&x=1&y=2"]
        );
    }
    #[test]
    fn index_is_distinct_and_plain_namespace_is_supported() {
        let map=parse("<sitemapindex><sitemap><loc>https://example.org/part.xml</loc></sitemap></sitemapindex>","https://example.org/map.xml").unwrap();
        assert!(map.kind == Kind::Index);
        assert_eq!(map.locations.len(), 1);
    }
    #[test]
    fn malformed_external_entities_namespaces_and_paths_are_rejected() {
        for xml in [
            "<!DOCTYPE urlset [<!ENTITY x SYSTEM 'file:///private'>]><urlset><url><loc>&x;</loc></url></urlset>",
            "<urlset><url><loc>&unknown;</loc></url></urlset>",
            "<urlset><url><loc>https://other.example/file</loc></url></urlset>",
            "<urlset><url><loc>https://user:secret@example.org/file</loc></url></urlset>",
            "<urlset><url><loc>http://example.org/file</loc></url></urlset>",
            "<urlset><url><loc>/relative</loc></url></urlset>",
            "<urlset><url><loc>https://example.org/a</loc><loc>https://example.org/b</loc></url></urlset>",
            "<urlset><url/></urlset>",
            "<urlset><url><loc><b>https://example.org/a</b></loc></url></urlset>",
            "<urlset><url><loc>https://example.org/a</url></loc></urlset>",
            "<urlset></urlset><urlset></urlset>",
            "<x:urlset xmlns:x='urn:wrong'/>",
            "<urlset><url><x:loc xmlns:x='urn:wrong'>https://example.org/a</x:loc></url></urlset>",
            "<urlset><url><loc>https://example.org/a</loc></url>",
        ] {
            assert!(
                parse(xml, "https://example.org/map.xml").is_err(),
                "accepted malformed input"
            );
        }
    }
    #[test]
    fn limits_fail_instead_of_truncating() {
        let xml = format!(
            "<urlset>{}</urlset>",
            "<url><loc>https://example.org/a</loc></url>".repeat(10_001)
        );
        assert!(matches!(
            parse(&xml, "https://example.org/map.xml"),
            Err(RuntimeError::Policy("workflow.sitemap_limit"))
        ));
    }
}
