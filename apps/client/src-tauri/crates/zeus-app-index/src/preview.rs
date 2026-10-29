//! The preview pane: a text head, an image, or a card of facts, for one file the person selected.
//!
//! Returns at most 64 KiB of text or 4 MiB of image. Optional text line counting is bounded at
//! 4 MiB; all reads reuse the opened file. SVG is source text only. Native routes refuse quarantined files before reading.

use std::io::{Read, Seek};
use std::path::Path;

use base64::Engine as _;
use serde_json::{Value, json};

use crate::{
    paths::{RootCaps, shown},
    read::{OpenedRead, changed},
};
use zeus_app::failure::Result;

/// Bytes of text shown.
pub const TEXT_LIMIT: usize = 64 * 1024;
/// Largest image shown.
pub const IMAGE_LIMIT: u64 = 4 * 1024 * 1024;
const COUNT_LINES_UP_TO: u64 = 4 * 1024 * 1024;

/// The picture formats the pane renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Image {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF.
    Gif,
    /// WebP.
    Webp,
}

impl Image {
    /// The MIME type.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

/// What the first bytes say the file is.
pub fn sniff_image(head: &[u8]) -> Option<Image> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(Image::Png)
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Image::Jpeg)
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some(Image::Gif)
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some(Image::Webp)
    } else {
        None
    }
}

/// Natural width and height from the image's own header.
pub fn dimensions(kind: Image, b: &[u8]) -> Option<(u32, u32)> {
    let be32 = |i: usize| {
        b.get(i..i + 4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    };
    let le16 = |i: usize| {
        b.get(i..i + 2)
            .map(|s| u32::from(u16::from_le_bytes([s[0], s[1]])))
    };
    match kind {
        Image::Png => Some((be32(16)?, be32(20)?)),
        Image::Gif => Some((le16(6)?, le16(8)?)),
        Image::Jpeg => {
            let mut i = 2;
            while i + 9 < b.len() {
                if b[i] != 0xFF {
                    i += 1;
                    continue;
                }
                let m = b[i + 1];
                if m == 0xFF {
                    i += 1;
                    continue;
                }
                if (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
                    let h = u32::from(u16::from_be_bytes([b[i + 5], b[i + 6]]));
                    let w = u32::from(u16::from_be_bytes([b[i + 7], b[i + 8]]));
                    return Some((w, h));
                }
                if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
                    i += 2;
                    continue;
                }
                let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
                i += 2 + len;
            }
            None
        }
        Image::Webp => match b.get(12..16)? {
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let (b0, b1, b2, b3) = (
                    u32::from(*b.get(21)?),
                    u32::from(*b.get(22)?),
                    u32::from(*b.get(23)?),
                    u32::from(*b.get(24)?),
                );
                Some((
                    1 + (b0 | ((b1 & 0x3f) << 8)),
                    1 + ((b1 >> 6) | (b2 << 2) | ((b3 & 0xf) << 10)),
                ))
            }
            b"VP8X" => {
                let le24 = |i: usize| {
                    b.get(i..i + 3)
                        .map(|s| u32::from(s[0]) | (u32::from(s[1]) << 8) | (u32::from(s[2]) << 16))
                };
                Some((1 + le24(24)?, 1 + le24(27)?))
            }
            _ => None,
        },
    }
}

/// The part of `head` that is text, if it is text: valid UTF-8 (a character cut at the end is fine) with no NUL and
/// no more than one control character in a hundred bytes (tab, line feed, form feed, carriage return and escape are
/// not counted). Data that happens to be valid UTF-8, such as a run of one low byte, is not text.
pub fn as_text(head: &[u8]) -> Option<&str> {
    if head.contains(&0) {
        return None;
    }
    let control = head
        .iter()
        .filter(|b| matches!(**b, 0x01..=0x08 | 0x0b | 0x0e..=0x1a | 0x1c..=0x1f | 0x7f))
        .count();
    if control * 100 > head.len() {
        return None;
    }
    match std::str::from_utf8(head) {
        Ok(s) => Some(s),
        Err(e) if head.len() - e.valid_up_to() <= 3 && e.error_len().is_none() => {
            std::str::from_utf8(&head[..e.valid_up_to()]).ok()
        }
        Err(_) => None,
    }
}

/// A MIME type from the extension, for the metadata card.
pub fn mime_of(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "txt" | "log" | "md" | "csv" | "ini" | "toml" | "yaml" | "yml" => "text/plain",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "7z" => "application/x-7z-compressed",
        "tar" => "application/x-tar",
        "iso" => "application/x-iso9660-image",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "epub" => "application/epub+zip",
        _ => "application/octet-stream",
    }
}

/// Preview an explicitly trusted local path, capturing its parent as authority. Native routes
/// instead use `build_pinned` with the settings service's existing root identities.
pub fn build(path: &Path, shown_path: &str) -> Value {
    let root = path.parent().unwrap_or(path);
    match RootCaps::capture(&[shown(root)]) {
        Ok(roots) => build_pinned(path, shown_path, &roots),
        Err(_) => refused(shown_path),
    }
}
fn refused(path: &str) -> Value {
    json!({"kind":"refused","reason":"changed_or_unavailable","path":path,"message":"That item changed or could not be read inside the enabled folders. Select it again."})
}
fn with(mut base: Value, extra: Value) -> Value {
    if let (Some(o), Some(e)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    base
}
/// Build a bounded preview using only retained directories and the same opened regular file.
/// The caller checks current quarantine facts before entering this function.
pub fn build_pinned(path: &Path, shown_path: &str, roots: &RootCaps) -> Value {
    build_scoped(path, shown_path, roots).unwrap_or_else(|_| refused(shown_path))
}
fn build_scoped(path: &Path, shown_path: &str, roots: &RootCaps) -> Result<Value> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Ok(dir) = roots.directory(path) {
        let meta = dir.metadata().map_err(crate::paths::cap_failure)?;
        return Ok(
            json!({"path":shown_path,"name":name,"kind":"folder","size":0,"modified_ms":meta.mtime_ms,"mime":mime_of(&name)}),
        );
    }
    let mut input = OpenedRead::open(roots, path)?;
    let base = json!({"path":shown_path,"name":name,"size":input.before.len,"modified_ms":input.before.mtime_ms,"mime":mime_of(&name)});
    let result = file_preview(&mut input, &name, base)?;
    input.check_unchanged()?;
    Ok(result)
}
fn bounded_bytes(input: &mut (impl Read + Seek), limit: u64) -> Result<Vec<u8>> {
    input.rewind()?;
    let mut bytes = Vec::new();
    input.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(changed());
    }
    Ok(bytes)
}
fn file_preview(input: &mut OpenedRead, name: &str, base: Value) -> Result<Value> {
    let mut head = Vec::new();
    (&mut input.file)
        .take(TEXT_LIMIT as u64)
        .read_to_end(&mut head)?;
    if let Some(kind) = sniff_image(&head) {
        if input.before.len > IMAGE_LIMIT {
            return Ok(with(
                base,
                json!({"kind":"meta","reason":"too_large","message":"This picture is larger than 4 MiB; Zeus shows its facts only."}),
            ));
        }
        let bytes = bounded_bytes(&mut input.file, IMAGE_LIMIT)?;
        let (w, h) = dimensions(kind, &bytes).unwrap_or((0, 0));
        let url = format!(
            "data:{};base64,{}",
            kind.mime(),
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        return Ok(with(
            base,
            json!({"kind":"image","mime":kind.mime(),"data_url":url,"width":w,"height":h}),
        ));
    }
    if let Some(text) = as_text(&head) {
        let truncated = input.before.len > head.len() as u64;
        let lines = if input.before.len <= COUNT_LINES_UP_TO {
            let bytes = bounded_bytes(&mut input.file, COUNT_LINES_UP_TO)?;
            Some(
                bytes.iter().filter(|c| **c == b'\n').count()
                    + usize::from(!bytes.is_empty() && bytes.last() != Some(&b'\n')),
            )
        } else {
            None
        };
        let svg = name.to_lowercase().ends_with(".svg");
        return Ok(with(
            base,
            json!({"kind":"text","text":text,"truncated":truncated,"lines":lines,"note":if svg {Value::from("Shown as source; Zeus never renders an SVG.")} else {Value::Null}}),
        ));
    }
    Ok(with(
        base,
        json!({"kind":"meta","reason":"binary","message":"No preview for this kind of file."}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        v.extend(w.to_be_bytes());
        v.extend(h.to_be_bytes());
        v.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        v
    }

    #[test]
    fn image_formats_and_sizes_are_read_from_their_headers() {
        let png = tiny_png(640, 480);
        assert_eq!(sniff_image(&png), Some(Image::Png));
        assert_eq!(dimensions(Image::Png, &png), Some((640, 480)));
        let mut gif = b"GIF89a".to_vec();
        gif.extend([0x20, 0x00, 0x10, 0x00]);
        assert_eq!(dimensions(Image::Gif, &gif), Some((32, 16)));
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 11, 8, 0, 100, 0, 200, 3, 1, 0x22, 0,
        ];
        assert_eq!(dimensions(Image::Jpeg, &jpeg), Some((200, 100)));
        assert_eq!(sniff_image(b"hello world"), None);
    }

    #[test]
    fn text_is_utf8_without_nul_and_tolerates_a_cut_character() {
        assert_eq!(as_text("héllo".as_bytes()), Some("héllo"));
        let cut = &"é".as_bytes()[..1];
        let mut v = b"ab".to_vec();
        v.extend(cut);
        assert_eq!(as_text(&v), Some("ab"));
        assert!(as_text(b"a\0b").is_none());
        assert!(as_text(&[0xff, 0xfe, 0xfd, 0xfc, 0xfb]).is_none());
        assert!(
            as_text(&[3u8; 300]).is_none(),
            "a run of one control byte is data"
        );
        let ansi = "line one\r\n\ttab\x0cpage\x1b[0m";
        assert_eq!(as_text(ansi.as_bytes()), Some(ansi));
    }

    #[test]
    fn previews_of_text_images_folders_and_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("notes.md");
        std::fs::write(&t, "line one\nline two\n").unwrap();
        let v = build(&t, "notes.md");
        assert_eq!(v["kind"], "text");
        assert_eq!(v["lines"], 2);
        assert_eq!(v["truncated"], false);
        let big = dir.path().join("big.txt");
        std::fs::write(&big, "x".repeat(TEXT_LIMIT * 2)).unwrap();
        let v = build(&big, "big.txt");
        assert_eq!(v["truncated"], true);
        assert_eq!(v["text"].as_str().unwrap().len(), TEXT_LIMIT);
        let p = dir.path().join("p.png");
        std::fs::write(&p, tiny_png(5, 7)).unwrap();
        let v = build(&p, "p.png");
        assert_eq!(
            (
                v["kind"].as_str(),
                v["width"].as_u64(),
                v["height"].as_u64()
            ),
            (Some("image"), Some(5), Some(7))
        );
        assert!(
            v["data_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        let b = dir.path().join("blob.bin");
        std::fs::write(&b, [0u8, 1, 2, 3]).unwrap();
        assert_eq!(build(&b, "blob.bin")["kind"], "meta");
        assert_eq!(build(dir.path(), "dir")["kind"], "folder");
        let svg = dir.path().join("x.svg");
        std::fs::write(&svg, "<svg><script>alert(1)</script></svg>").unwrap();
        let v = build(&svg, "x.svg");
        assert_eq!(v["kind"], "text");
        assert!(v["note"].as_str().unwrap().contains("never renders"));
    }
    #[test]
    fn image_growth_cannot_exceed_preview_read_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.png");
        std::fs::write(&path, tiny_png(2, 3)).unwrap();
        let roots = RootCaps::capture(&[shown(dir.path())]).unwrap();
        let mut input = OpenedRead::open(&roots, &path).unwrap();
        // Grow after opened metadata was captured, preserving the original image header.
        let mut grown = tiny_png(2, 3);
        grown.resize(IMAGE_LIMIT as usize + 100, 0);
        std::fs::write(&path, grown).unwrap();
        assert!(file_preview(&mut input, "image.png", json!({})).is_err());
        assert_eq!(input.file.stream_position().unwrap(), IMAGE_LIMIT + 1);
        let fresh = build_pinned(&path, "image.png", &roots);
        assert_eq!(fresh["kind"], "meta");
        assert_eq!(fresh["reason"], "too_large");
    }
    #[test]
    fn long_text_does_not_trigger_unbounded_line_counting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text.txt");
        std::fs::write(&path, vec![b'x'; COUNT_LINES_UP_TO as usize + 1]).unwrap();
        let roots = RootCaps::capture(&[shown(dir.path())]).unwrap();
        let out = build_pinned(&path, "text.txt", &roots);
        assert_eq!(out["kind"], "text");
        assert_eq!(out["text"].as_str().unwrap().len(), TEXT_LIMIT);
        assert!(out["lines"].is_null());
        assert_eq!(out["truncated"], true);
    }
}
