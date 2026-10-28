//! Finite, unencrypted HLS playlists (RFC 8216), including master variants,
//! initialization maps and byte ranges. Parsing never executes page code.

use std::collections::BTreeMap;

use url::Url;
use zeus_app::failure::Result;

use super::manifest::{MAX_FORMATS, MAX_SEGMENTS, MediaRange, Segment, Track, resolve};

/// One HLS master-playlist selection. URLs remain private.
#[derive(Debug, Clone)]
pub struct Variant {
    /// Private child-playlist address.
    pub url: Url,
    /// Selection facts; the segment list is populated after selection.
    pub facts: Track,
}

/// A master catalogue or one finite track.
#[derive(Debug, Clone)]
pub enum Playlist {
    /// Variant playlists and separate audio renditions.
    Master(Vec<Variant>),
    /// An ordered, finite track.
    Media(Track),
}

fn attributes(input: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut rest = input.trim();
    while !rest.is_empty() {
        let (name, value) = rest
            .split_once('=')
            .ok_or_else(|| super::invalid("An HLS attribute is invalid."))?;
        if name.is_empty() || name.len() > 64 {
            return Err(super::invalid("An HLS attribute is invalid."));
        }
        let (value, tail) = if let Some(quoted) = value.strip_prefix('"') {
            let end = quoted
                .find('"')
                .ok_or_else(|| super::invalid("An HLS quoted attribute is invalid."))?;
            let tail = &quoted[end + 1..];
            if !tail.is_empty() && !tail.starts_with(',') {
                return Err(super::invalid("An HLS attribute separator is invalid."));
            }
            (&quoted[..end], tail.strip_prefix(',').unwrap_or(tail))
        } else {
            value.split_once(',').unwrap_or((value, ""))
        };
        if out.insert(name.to_owned(), value.to_owned()).is_some() {
            return Err(super::invalid("An HLS attribute is repeated."));
        }
        rest = tail.trim();
    }
    Ok(out)
}

fn number<T: std::str::FromStr>(value: Option<&String>) -> Option<T> {
    value.and_then(|v| v.parse().ok())
}

fn interval(value: &str, next: Option<u64>) -> Result<MediaRange> {
    let (length, start) = value
        .split_once('@')
        .map_or((value, None), |(length, start)| (length, Some(start)));
    let length: u64 = length
        .parse()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| super::invalid("An HLS byte-range length is invalid."))?;
    let start = match start {
        Some(start) => start
            .parse()
            .map_err(|_| super::invalid("An HLS byte-range offset is invalid."))?,
        None => next.ok_or_else(|| {
            super::invalid(
                "An implicit HLS byte range needs the preceding range on the same resource.",
            )
        })?,
    };
    let end = start
        .checked_add(length)
        .ok_or_else(|| super::invalid("An HLS byte range overflows."))?;
    Ok(MediaRange { start, end })
}

fn initial_track() -> Track {
    Track {
        label: "HLS media".into(),
        container: "ts".into(),
        mime_type: Some("video/mp2t".into()),
        bandwidth: None,
        width: None,
        height: None,
        segments: Vec::new(),
        duration_ms: None,
        notices: Vec::new(),
    }
}

/// Parse an HLS playlist. Encrypted media, live/event recordings, variables,
/// discontinuities and low-latency partial segments return explicit errors.
pub fn parse(base: &Url, text: &str) -> Result<Playlist> {
    if text.len() > super::manifest::MAX_MANIFEST_BYTES {
        return Err(super::unsupported("The HLS manifest exceeds two MiB."));
    }
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next().map(str::trim) != Some("#EXTM3U") {
        return Err(super::invalid("The response is not an HLS playlist."));
    }
    let mut variants = Vec::new();
    let mut pending_variant = None;
    let mut track = initial_track();
    let mut pending_range: Option<String> = None;
    let mut prior_resource: Option<(Url, u64)> = None;
    let mut init: Option<Segment> = None;
    let mut ended = false;
    let mut duration = 0f64;
    let mut extinf = false;
    let mut independent_audio = false;
    let mut address_bytes = 0usize;
    for line in lines.map(str::trim).filter(|l| !l.is_empty()) {
        if line.len() > 16 * 1024 {
            return Err(super::invalid("An HLS playlist line exceeds the limit."));
        }
        if line.contains("{$") {
            return Err(super::unsupported(
                "HLS variable substitution is not supported.",
            ));
        }
        if let Some(raw) = line.strip_prefix("#EXT-X-KEY:") {
            if attributes(raw)?.get("METHOD").map(String::as_str) != Some("NONE") {
                return Err(super::unsupported(
                    "Encrypted or protected HLS media is not supported.",
                ));
            }
        } else if line.starts_with("#EXT-X-SESSION-KEY:") {
            return Err(super::unsupported(
                "Protected HLS sessions are not supported.",
            ));
        } else if line == "#EXT-X-DISCONTINUITY"
            || line.starts_with("#EXT-X-PART:")
            || line.starts_with("#EXT-X-PRELOAD-HINT:")
            || line == "#EXT-X-GAP"
            || line.starts_with("#EXT-X-I-FRAMES-ONLY")
        {
            return Err(super::unsupported(
                "This HLS track requires discontinuity, gap, I-frame or low-latency handling that Zeus does not yet support.",
            ));
        } else if let Some(raw) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            if pending_variant.is_some() {
                return Err(super::invalid("An HLS variant has no playlist address."));
            }
            let attrs = attributes(raw)?;
            let mut facts = initial_track();
            facts.bandwidth =
                number(attrs.get("AVERAGE-BANDWIDTH")).or_else(|| number(attrs.get("BANDWIDTH")));
            if let Some((width, height)) = attrs.get("RESOLUTION").and_then(|v| v.split_once('x')) {
                facts.width = width.parse().ok();
                facts.height = height.parse().ok();
            }
            facts.label = match facts.height {
                Some(height) => format!("HLS {height}p"),
                None => "HLS variant".into(),
            };
            if attrs.contains_key("AUDIO") {
                independent_audio = true;
                facts.notices.push("This variant may contain video only. Select its separate audio track separately; Zeus does not silently mux tracks.".into());
            }
            pending_variant = Some(facts);
        } else if let Some(raw) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attrs = attributes(raw)?;
            if attrs.get("TYPE").map(String::as_str) == Some("AUDIO")
                && let Some(uri) = attrs.get("URI")
            {
                let mut facts = initial_track();
                facts.label = format!(
                    "HLS audio: {}",
                    attrs
                        .get("NAME")
                        .map(String::as_str)
                        .unwrap_or("audio")
                        .chars()
                        .take(100)
                        .collect::<String>()
                );
                facts
                    .notices
                    .push("Separate audio track; no automatic muxing.".into());
                variants.push(Variant {
                    url: resolve(base, uri)?,
                    facts,
                });
            }
        } else if let Some(raw) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            if pending_range.replace(raw.to_owned()).is_some() {
                return Err(super::invalid(
                    "An HLS media resource has repeated byte ranges.",
                ));
            }
        } else if let Some(raw) = line.strip_prefix("#EXT-X-MAP:") {
            if !track.segments.is_empty() {
                return Err(super::unsupported(
                    "Changing HLS initialization maps requires a remux step and is not supported.",
                ));
            }
            let attrs = attributes(raw)?;
            let uri = attrs
                .get("URI")
                .ok_or_else(|| super::invalid("An HLS initialization map has no address."))?;
            let range = attrs
                .get("BYTERANGE")
                .map(|raw| interval(raw, None))
                .transpose()?;
            init = Some(Segment {
                url: resolve(base, uri)?,
                range,
            });
            track.container = "mp4".into();
            track.mime_type = Some("video/mp4".into());
        } else if let Some(raw) = line.strip_prefix("#EXTINF:") {
            let seconds: f64 = raw
                .split(',')
                .next()
                .unwrap_or("")
                .parse()
                .map_err(|_| super::invalid("An HLS segment duration is invalid."))?;
            if !seconds.is_finite() || seconds < 0.0 || seconds > 86400.0 || extinf {
                return Err(super::invalid(
                    "An HLS segment duration is invalid or repeated.",
                ));
            }
            duration += seconds;
            extinf = true;
        } else if line == "#EXT-X-ENDLIST" {
            ended = true;
        } else if !line.starts_with('#') {
            if let Some(facts) = pending_variant.take() {
                variants.push(Variant {
                    url: resolve(base, line)?,
                    facts,
                });
            } else {
                if !extinf || ended {
                    return Err(super::invalid(
                        "An HLS segment is missing a duration or appears after the end marker.",
                    ));
                }
                let url = resolve(base, line)?;
                address_bytes = address_bytes
                    .checked_add(url.as_str().len())
                    .filter(|bytes| *bytes <= super::manifest::MAX_EXPANDED_URL_BYTES)
                    .ok_or_else(|| {
                        super::unsupported(
                            "The expanded HLS resource addresses exceed sixteen MiB.",
                        )
                    })?;
                let next = prior_resource
                    .as_ref()
                    .filter(|(prior, _)| prior == &url)
                    .map(|(_, end)| *end);
                let range = pending_range
                    .take()
                    .map(|raw| interval(&raw, next))
                    .transpose()?;
                prior_resource = range.as_ref().map(|range| (url.clone(), range.end));
                track.segments.push(Segment { url, range });
                extinf = false;
            }
        }
        if variants.len() > MAX_FORMATS || track.segments.len() > MAX_SEGMENTS {
            return Err(super::unsupported(
                "The media playlist exceeds Zeus's bounded format or segment limit.",
            ));
        }
    }
    if pending_variant.is_some() || pending_range.is_some() || extinf {
        return Err(super::invalid(
            "The HLS playlist ends in an incomplete resource declaration.",
        ));
    }
    if !variants.is_empty() {
        if !track.segments.is_empty() || ended {
            return Err(super::invalid(
                "An HLS master cannot also contain media segments.",
            ));
        }
        if independent_audio {
            for variant in &mut variants {
                if variant.facts.notices.is_empty() {
                    variant.facts.notices.push("The master includes separate audio renditions. Review the selected track before saving.".into());
                }
            }
        }
        return Ok(Playlist::Master(variants));
    }
    if !ended {
        return Err(super::unsupported(
            "Only finite HLS playlists with EXT-X-ENDLIST can be saved. Live recording is not implemented.",
        ));
    }
    if track.segments.is_empty() {
        return Err(super::invalid("The HLS playlist has no media segments."));
    }
    if let Some(init) = init {
        track.segments.insert(0, init);
    }
    if track.segments.len() > MAX_SEGMENTS {
        return Err(super::unsupported(
            "The initialization map and segments exceed the media limit.",
        ));
    }
    track.duration_ms = Some((duration * 1000.0).round() as u64);
    Ok(Playlist::Media(track))
}
