//! A small incremental Server-Sent Events splitter.
//!
//! Bytes go in as they arrive; whole events come out. An event is the lines up to a blank line: `event:` names it,
//! `data:` lines (joined with newlines) carry the payload, `:` lines are comments and `id:` and `retry:` are ignored.
//! Both `\n\n` and `\r\n\r\n` end an event, and a multi-byte character may be split across two chunks.

/// One event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The `event:` name, if any.
    pub name: Option<String>,
    /// The joined `data:` lines.
    pub data: String,
}

/// The splitter.
#[derive(Debug, Default)]
pub struct Splitter {
    buf: Vec<u8>,
}

impl Splitter {
    /// A new splitter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes buffered but not yet a whole event.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// Feed bytes; returns every event completed by them.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some((end, skip)) = find_boundary(&self.buf) {
            let block: Vec<u8> = self.buf.drain(..end + skip).take(end).collect();
            if let Some(event) = parse_block(&block) {
                out.push(event);
            }
        }
        out
    }

    /// The last event when the stream ended without a final blank line.
    pub fn finish(&mut self) -> Option<Event> {
        let block = std::mem::take(&mut self.buf);
        parse_block(&block)
    }
}

/// The index of the first event boundary and its length.
fn find_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\n' {
            if buf.get(i + 1) == Some(&b'\n') {
                return Some((i, 2));
            }
            if buf.get(i + 1) == Some(&b'\r') && buf.get(i + 2) == Some(&b'\n') {
                return Some((i, 3));
            }
        }
        i += 1;
    }
    None
}

fn parse_block(block: &[u8]) -> Option<Event> {
    let text = String::from_utf8_lossy(block);
    let mut name = None;
    let mut data: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => name = Some(value.to_owned()),
            "data" => data.push(value),
            _ => {}
        }
    }
    if name.is_none() && data.is_empty() {
        return None;
    }
    Some(Event {
        name,
        data: data.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_across_chunks_and_line_endings() {
        let mut s = Splitter::new();
        assert!(s.feed(b"event: a\ndata: {\"x\"").is_empty());
        let got = s.feed(b":1}\n\ndata: two\r\n\r\n: comment\n\ndata: l1\ndata: l2\n\n");
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[0],
            Event {
                name: Some("a".into()),
                data: "{\"x\":1}".into()
            }
        );
        assert_eq!(got[1].data, "two");
        assert_eq!(got[2].data, "l1\nl2");
    }

    #[test]
    fn a_multibyte_character_split_between_chunks_survives() {
        let mut s = Splitter::new();
        let full = "data: caf\u{e9}\n\n".as_bytes().to_vec();
        let (a, b) = full.split_at(10);
        assert!(s.feed(a).is_empty());
        let got = s.feed(b);
        assert_eq!(got[0].data, "caf\u{e9}");
    }

    #[test]
    fn the_last_event_without_a_blank_line_is_recovered() {
        let mut s = Splitter::new();
        assert!(s.feed(b"data: [DONE]").is_empty());
        assert_eq!(s.finish().unwrap().data, "[DONE]");
        assert!(s.finish().is_none());
    }
}
