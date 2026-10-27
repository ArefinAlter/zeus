//! A tolerant link extractor for the site grabber: it reads tags and attributes, never runs anything.
//!
//! It finds `href` of `<a>` and `<area>`, `src` of media and embed tags, `src` of frames, and the `<base href>`.
//! Comments, scripts and styles are skipped. Entities in attribute values are decoded for the few that appear in URLs.

/// Where a link was found, which decides how the crawler treats it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// An anchor or image map: a page to read or a file to take.
    Anchor,
    /// A frame: a page to read.
    Frame,
    /// A media or image source: a file to take, never a page to read.
    Media,
}

/// One raw link as written in the page (not yet resolved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLink {
    /// The attribute value, entities decoded.
    pub href: String,
    /// The kind of tag it came from.
    pub source: Source,
}

fn decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&#38;", "&")
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&#x2F;", "/")
        .trim()
        .to_owned()
}

/// Every link of `html`, in document order, and the `<base href>` when there is one.
pub fn extract(html: &str) -> (Vec<RawLink>, Option<String>) {
    let bytes = html.as_bytes();
    let mut links = Vec::new();
    let mut base = None;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if html[i..].starts_with("<!--") {
            i = html[i..].find("-->").map_or(bytes.len(), |e| i + e + 3);
            continue;
        }
        // The tag name.
        let mut j = i + 1;
        let closing = bytes.get(j) == Some(&b'/');
        if closing {
            j += 1;
        }
        let name_start = j;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-') {
            j += 1;
        }
        let name = html[name_start..j].to_ascii_lowercase();
        if name.is_empty() {
            i += 1;
            continue;
        }
        // The attributes up to the closing `>`, honouring quotes.
        let mut attrs: Vec<(String, String)> = Vec::new();
        loop {
            while j < bytes.len() && (bytes[j].is_ascii_whitespace() || bytes[j] == b'/') {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] == b'>' {
                j += 1;
                break;
            }
            let key_start = j;
            while j < bytes.len()
                && !bytes[j].is_ascii_whitespace()
                && !matches!(bytes[j], b'=' | b'>' | b'/')
            {
                j += 1;
            }
            let key = html[key_start..j].to_ascii_lowercase();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let mut value = String::new();
            if bytes.get(j) == Some(&b'=') {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                match bytes.get(j) {
                    Some(&q) if q == b'"' || q == b'\'' => {
                        let start = j + 1;
                        let end = html[start..]
                            .find(q as char)
                            .map_or(bytes.len(), |e| start + e);
                        value = html[start..end].to_owned();
                        j = (end + 1).min(bytes.len());
                    }
                    _ => {
                        let start = j;
                        while j < bytes.len() && !bytes[j].is_ascii_whitespace() && bytes[j] != b'>'
                        {
                            j += 1;
                        }
                        value = html[start..j].to_owned();
                    }
                }
            }
            if key.is_empty() {
                j += 1;
                continue;
            }
            attrs.push((key, value));
        }
        i = j;
        if closing {
            continue;
        }
        let attr = |k: &str| {
            attrs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| decode(v))
        };
        match name.as_str() {
            "a" | "area" => {
                if let Some(h) = attr("href") {
                    links.push(RawLink {
                        href: h,
                        source: Source::Anchor,
                    });
                }
            }
            "iframe" | "frame" => {
                if let Some(h) = attr("src") {
                    links.push(RawLink {
                        href: h,
                        source: Source::Frame,
                    });
                }
            }
            "img" | "source" | "video" | "audio" | "embed" | "track" => {
                if let Some(h) = attr("src") {
                    links.push(RawLink {
                        href: h,
                        source: Source::Media,
                    });
                }
            }
            "base" => {
                if base.is_none() {
                    base = attr("href");
                }
            }
            "script" | "style" => {
                // Skip to the matching close so markup inside is not read as tags.
                let close = format!("</{name}");
                i = html[i..]
                    .to_ascii_lowercase()
                    .find(&close)
                    .map_or(bytes.len(), |e| i + e);
            }
            _ => {}
        }
    }
    (links, base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_anchors_media_and_base_and_skips_comments_and_scripts() {
        let html = r#"<!doctype html><base href="/site/">
            <a href="guide/">Guide</a> <A HREF='files/a&amp;b.pdf'>x</A>
            <!-- <a href="hidden.zip"> -->
            <script>var s = "<a href='js.zip'>";</script>
            <img src=pics/p.png alt=x> <iframe src="/frame/"></iframe>
            <a name="top">no href</a> <a href="mailto:a@b.c">mail</a>"#;
        let (links, base) = extract(html);
        assert_eq!(base.as_deref(), Some("/site/"));
        let hrefs: Vec<&str> = links.iter().map(|l| l.href.as_str()).collect();
        assert_eq!(
            hrefs,
            [
                "guide/",
                "files/a&b.pdf",
                "pics/p.png",
                "/frame/",
                "mailto:a@b.c"
            ]
        );
        assert_eq!(links[2].source, Source::Media);
        assert_eq!(links[3].source, Source::Frame);
    }

    #[test]
    fn broken_markup_does_not_loop_or_panic() {
        for html in [
            "<a href=\"x",
            "<",
            "<a",
            "<a href=>",
            "<<<>>>",
            "<a href='x' href='y'>",
            "<!-- never closed <a href=z>",
        ] {
            let _ = extract(html);
        }
        assert_eq!(extract("<a href='x' href='y'>").0[0].href, "x");
    }
}
