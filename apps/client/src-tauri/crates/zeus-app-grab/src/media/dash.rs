//! Bounded static DASH: SegmentList, SegmentTemplate ($Number$/$Time$ and
//! finite SegmentTimeline), and direct single-file representations.

use std::collections::BTreeMap;

use quick_xml::Reader;
use quick_xml::events::Event;
use url::Url;
use zeus_app::failure::Result;

use super::manifest::{MAX_FORMATS, MAX_SEGMENTS, Segment, Track, range, resolve};

#[derive(Debug, Clone, Default)]
struct Node {
    name: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Node>,
    text: String,
}

impl Node {
    fn one(&self, name: &str) -> Result<Option<&Self>> {
        let mut matches = self.children.iter().filter(|n| n.name == name);
        let one = matches.next();
        if matches.next().is_some() {
            return Err(super::unsupported(
                "Alternative DASH addressing elements require an explicit selection and are not supported.",
            ));
        }
        Ok(one)
    }

    fn number(&self, name: &str, default: u64) -> Result<u64> {
        self.attrs.get(name).map_or(Ok(default), |s| {
            s.parse()
                .map_err(|_| super::invalid("A DASH integer attribute is invalid."))
        })
    }
}

fn xml(text: &str) -> Result<Node> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    reader.config_mut().expand_empty_elements = true;
    let mut stack = vec![Node::default()];
    let mut count = 0usize;
    loop {
        match reader
            .read_event()
            .map_err(|_| super::invalid("The DASH XML is invalid."))?
        {
            Event::Start(e) => {
                count += 1;
                if count > 50_000 || stack.len() > 32 {
                    return Err(super::unsupported(
                        "The DASH XML exceeds the element or nesting limit.",
                    ));
                }
                let mut node = Node {
                    name: e.local_name().as_ref().to_owned(),
                    ..Node::default()
                };
                for attr in e.attributes() {
                    let attr = attr.map_err(|_| super::invalid("A DASH attribute is invalid."))?;
                    let key = attr.key.as_ref().to_owned();
                    let value = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|_| super::invalid("A DASH attribute value is invalid."))?
                        .into_owned();
                    if node.attrs.insert(key, value).is_some() {
                        return Err(super::invalid("A DASH attribute is repeated."));
                    }
                }
                if node.name == "ContentProtection" {
                    return Err(super::unsupported("Protected DASH media is not supported."));
                }
                stack.push(node);
            }
            Event::End(_) => {
                if stack.len() < 2 {
                    return Err(super::invalid("The DASH XML has an unmatched closing tag."));
                }
                let node = stack
                    .pop()
                    .ok_or_else(|| super::invalid("The DASH XML is invalid."))?;
                stack
                    .last_mut()
                    .ok_or_else(|| super::invalid("The DASH XML is invalid."))?
                    .children
                    .push(node);
            }
            Event::Text(e) => {
                let raw = e.as_ref();
                let value = quick_xml::escape::unescape(raw)
                    .map_err(|_| super::invalid("DASH text has an invalid entity."))?;
                stack
                    .last_mut()
                    .ok_or_else(|| super::invalid("The DASH XML is invalid."))?
                    .text
                    .push_str(&value);
            }
            Event::CData(e) => {
                let value = e.as_ref();
                stack
                    .last_mut()
                    .ok_or_else(|| super::invalid("The DASH XML is invalid."))?
                    .text
                    .push_str(value);
            }
            Event::GeneralRef(e) => {
                let raw = e.as_ref();
                let encoded = format!("&{raw};");
                let value = quick_xml::escape::unescape(&encoded)
                    .map_err(|_| super::unsupported("DASH custom entities are not supported."))?;
                stack
                    .last_mut()
                    .ok_or_else(|| super::invalid("The DASH XML is invalid."))?
                    .text
                    .push_str(&value);
            }
            Event::DocType(_) => {
                return Err(super::unsupported("DASH document types are not supported."));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 || stack[0].children.len() != 1 || stack[0].children[0].name != "MPD" {
        return Err(super::invalid("The response is not one complete DASH MPD."));
    }
    Ok(stack.remove(0).children.remove(0))
}

fn duration(value: &str) -> Result<f64> {
    let value = value
        .strip_prefix("PT")
        .ok_or_else(|| super::unsupported("DASH durations must use hours, minutes and seconds."))?;
    let mut number = String::new();
    let mut seconds = 0.0;
    for c in value.chars() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
        } else {
            let amount: f64 = number
                .parse()
                .map_err(|_| super::invalid("A DASH duration is invalid."))?;
            number.clear();
            seconds += amount
                * match c {
                    'H' => 3600.0,
                    'M' => 60.0,
                    'S' => 1.0,
                    _ => {
                        return Err(super::unsupported(
                            "A DASH duration uses an unsupported unit.",
                        ));
                    }
                };
        }
    }
    if !number.is_empty() || !seconds.is_finite() || seconds <= 0.0 || seconds > 365.0 * 86400.0 {
        return Err(super::invalid(
            "A DASH duration is invalid or exceeds one year.",
        ));
    }
    Ok(seconds)
}

fn base_url(base: &Url, node: &Node) -> Result<Url> {
    match node.one("BaseURL")? {
        Some(url) => resolve(base, url.text.trim()),
        None => Ok(base.clone()),
    }
}

fn inherited(nodes: &[&Node], name: &str) -> Result<Option<Node>> {
    let mut result: Option<Node> = None;
    for node in nodes {
        if let Some(next) = node.one(name)? {
            let merged = result.get_or_insert_with(|| Node {
                name: name.into(),
                ..Node::default()
            });
            merged.attrs.extend(next.attrs.clone());
            let replaced = next
                .children
                .iter()
                .map(|child| child.name.as_str())
                .collect::<std::collections::HashSet<_>>();
            merged
                .children
                .retain(|prior| !replaced.contains(prior.name.as_str()));
            merged.children.extend(next.children.clone());
        }
    }
    Ok(result)
}

fn template(
    value: &str,
    representation: &str,
    bandwidth: u64,
    number: u64,
    time: u64,
) -> Result<String> {
    let mut result = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('$') {
        result.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        if let Some(tail) = rest.strip_prefix('$') {
            result.push('$');
            rest = tail;
            continue;
        }
        let (token, tail) = rest
            .split_once('$')
            .ok_or_else(|| super::invalid("A DASH template has an unclosed substitution."))?;
        let (name, format) = token
            .split_once('%')
            .map_or((token, None), |(n, f)| (n, Some(f)));
        let replacement = match name {
            "RepresentationID" if format.is_none() => representation.to_owned(),
            "Bandwidth" | "Number" | "Time" => {
                let value = match name {
                    "Bandwidth" => bandwidth,
                    "Number" => number,
                    _ => time,
                };
                let width = match format {
                    None => 0,
                    Some(format) => format
                        .strip_prefix('0')
                        .and_then(|s| s.strip_suffix('d'))
                        .and_then(|s| s.parse::<usize>().ok())
                        .filter(|width| *width <= 12)
                        .ok_or_else(|| {
                            super::unsupported("The DASH template number format is not supported.")
                        })?,
                };
                format!("{value:0width$}")
            }
            _ => {
                return Err(super::unsupported(
                    "The DASH template substitution is not supported.",
                ));
            }
        };
        result.push_str(&replacement);
        rest = tail;
        if result.len() > 16 * 1024 {
            return Err(super::invalid("A DASH resource address exceeds the limit."));
        }
    }
    result.push_str(rest);
    Ok(result)
}

fn from_template(
    base: &Url,
    node: &Node,
    repr: &Node,
    presentation_s: Option<f64>,
) -> Result<Vec<Segment>> {
    let representation = repr.attrs.get("id").map(String::as_str).unwrap_or("");
    let bandwidth = repr.number("bandwidth", 0)?;
    let scale = node.number("timescale", 1)?;
    if scale == 0 || scale > 1_000_000_000 {
        return Err(super::invalid("The DASH timescale is invalid."));
    }
    let start = node.number("startNumber", 1)?;
    let media = node
        .attrs
        .get("media")
        .ok_or_else(|| super::invalid("The DASH media template is missing."))?;
    let mut positions = Vec::new();
    if let Some(timeline) = node.one("SegmentTimeline")? {
        let mut time = 0;
        for point in timeline.children.iter().filter(|n| n.name == "S") {
            time = point.number("t", time)?;
            let step = point.number("d", 0)?;
            let repeats = point.number("r", 0).map_err(|_| {
                super::unsupported("Negative or unbounded DASH timeline repeats are not supported.")
            })?;
            if step == 0 || repeats >= MAX_SEGMENTS as u64 {
                return Err(super::invalid(
                    "A DASH timeline duration or repeat exceeds the limit.",
                ));
            }
            for _ in 0..=repeats {
                if positions.len() >= MAX_SEGMENTS - 1 {
                    return Err(super::unsupported(
                        "The DASH timeline exceeds the segment limit.",
                    ));
                }
                positions.push(time);
                time = time
                    .checked_add(step)
                    .ok_or_else(|| super::invalid("The DASH timeline overflows."))?;
            }
        }
    } else {
        let step = node.number("duration", 0)?;
        let seconds = presentation_s.ok_or_else(|| {
            super::unsupported("A finite DASH template needs a presentation duration or timeline.")
        })?;
        if step == 0 {
            return Err(super::invalid("The DASH template duration is zero."));
        }
        let count = (seconds * scale as f64 / step as f64).ceil();
        if count < 1.0 || count > (MAX_SEGMENTS - 1) as f64 {
            return Err(super::unsupported(
                "The DASH template exceeds the segment limit.",
            ));
        }
        for index in 0..count as u64 {
            positions.push(
                index
                    .checked_mul(step)
                    .ok_or_else(|| super::invalid("The DASH segment time overflows."))?,
            );
        }
    }
    let mut segments = Vec::new();
    if let Some(init) = node.attrs.get("initialization") {
        segments.push(Segment {
            url: resolve(base, &template(init, representation, bandwidth, start, 0)?)?,
            range: None,
        });
    }
    for (index, time) in positions.into_iter().enumerate() {
        let number = start
            .checked_add(index as u64)
            .ok_or_else(|| super::invalid("The DASH segment number overflows."))?;
        segments.push(Segment {
            url: resolve(
                base,
                &template(media, representation, bandwidth, number, time)?,
            )?,
            range: None,
        });
    }
    Ok(segments)
}

fn from_list(base: &Url, node: &Node) -> Result<Vec<Segment>> {
    let mut segments = Vec::new();
    if let Some(init) = node.one("Initialization")? {
        segments.push(Segment {
            url: init
                .attrs
                .get("sourceURL")
                .map_or(Ok(base.clone()), |url| resolve(base, url))?,
            range: init.attrs.get("range").map(|s| range(s)).transpose()?,
        });
    }
    for resource in node.children.iter().filter(|n| n.name == "SegmentURL") {
        let url = resource
            .attrs
            .get("media")
            .ok_or_else(|| super::invalid("A DASH segment has no media address."))?;
        segments.push(Segment {
            url: resolve(base, url)?,
            range: resource
                .attrs
                .get("mediaRange")
                .map(|s| range(s))
                .transpose()?,
        });
        if segments.len() > MAX_SEGMENTS {
            return Err(super::unsupported(
                "The DASH list exceeds the segment limit.",
            ));
        }
    }
    Ok(segments)
}

/// Parse the selected finite DASH presentation into separately reviewable tracks.
/// Multiple periods and protected/dynamic presentations are explicitly unavailable.
pub fn parse(base: &Url, text: &str) -> Result<Vec<Track>> {
    if text.len() > super::manifest::MAX_MANIFEST_BYTES {
        return Err(super::unsupported("The DASH manifest exceeds two MiB."));
    }
    let mpd = xml(text)?;
    if mpd
        .attrs
        .get("type")
        .map(String::as_str)
        .unwrap_or("static")
        != "static"
    {
        return Err(super::unsupported(
            "Live and dynamic DASH presentations are not supported.",
        ));
    }
    let period = mpd
        .one("Period")?
        .ok_or_else(|| super::invalid("The DASH presentation has no period."))?;
    let presentation_s = period
        .attrs
        .get("duration")
        .or_else(|| mpd.attrs.get("mediaPresentationDuration"))
        .map(|s| duration(s))
        .transpose()?;
    let base = base_url(&base_url(base, &mpd)?, period)?;
    let mut tracks = Vec::new();
    let mut address_bytes = 0usize;
    for adaptation in period.children.iter().filter(|n| n.name == "AdaptationSet") {
        let adaptation_base = base_url(&base, adaptation)?;
        for repr in adaptation
            .children
            .iter()
            .filter(|n| n.name == "Representation")
        {
            if tracks.len() >= MAX_FORMATS {
                return Err(super::unsupported(
                    "The DASH presentation exceeds the format limit.",
                ));
            }
            let source = base_url(&adaptation_base, repr)?;
            let chain = [&mpd, period, adaptation, repr];
            let list = inherited(&chain, "SegmentList")?;
            let templ = inherited(&chain, "SegmentTemplate")?;
            if list.is_some() && templ.is_some() {
                return Err(super::invalid(
                    "The DASH representation has conflicting addressing modes.",
                ));
            }
            let segments = match (list, templ) {
                (Some(list), None) => from_list(&source, &list)?,
                (None, Some(templ)) => from_template(&source, &templ, repr, presentation_s)?,
                (None, None) if source != base && !source.path().ends_with('/') => vec![Segment {
                    url: source,
                    range: None,
                }],
                _ => {
                    return Err(super::unsupported(
                        "This DASH representation needs an unsupported indexed addressing mode.",
                    ));
                }
            };
            if segments.is_empty() {
                return Err(super::invalid("The DASH representation has no segments."));
            }
            address_bytes = segments
                .iter()
                .try_fold(address_bytes, |total, segment| {
                    total.checked_add(segment.url.as_str().len())
                })
                .filter(|bytes| *bytes <= super::manifest::MAX_EXPANDED_URL_BYTES)
                .ok_or_else(|| {
                    super::unsupported("The expanded DASH resource addresses exceed sixteen MiB.")
                })?;
            let attribute = |key: &str| {
                repr.attrs
                    .get(key)
                    .or_else(|| adaptation.attrs.get(key))
                    .cloned()
            };
            let mime_type = attribute("mimeType").map(|mime| mime.to_ascii_lowercase());
            let webm = matches!(mime_type.as_deref(), Some("video/webm" | "audio/webm"));
            if mime_type.as_deref().is_some_and(|mime| {
                !matches!(
                    mime,
                    "video/mp4" | "audio/mp4" | "application/mp4" | "video/webm" | "audio/webm"
                )
            }) {
                return Err(super::unsupported(
                    "The DASH representation uses an unsupported media container.",
                ));
            }
            let audio = mime_type
                .as_deref()
                .is_some_and(|s| s.starts_with("audio/"))
                || attribute("contentType").as_deref() == Some("audio");
            let height = attribute("height").and_then(|s| s.parse().ok());
            let label = if audio {
                format!(
                    "DASH audio{}",
                    attribute("lang")
                        .map(|s| format!(" ({s})"))
                        .unwrap_or_default()
                )
            } else {
                height.map_or_else(
                    || "DASH video".into(),
                    |height| format!("DASH {height}p video"),
                )
            };
            tracks.push(Track {
                label, container: if webm { "webm" } else if audio { "m4a" } else { "mp4" }.into(), mime_type,
                bandwidth: attribute("bandwidth").and_then(|s| s.parse().ok()),
                width: attribute("width").and_then(|s| s.parse().ok()), height, segments,
                duration_ms: presentation_s.map(|s| (s * 1000.0).round() as u64),
                notices: vec!["This is one DASH representation. Separate audio/video tracks are saved separately; no automatic muxing or transcoding.".into()],
            });
        }
    }
    if tracks.is_empty() {
        return Err(super::invalid(
            "The DASH presentation has no selectable representation.",
        ));
    }
    Ok(tracks)
}
