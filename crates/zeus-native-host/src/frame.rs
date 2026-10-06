//! Native-messaging framing (spec 02 section 3.3).
//!
//! ```text
//! +----------------------+---------------------------------+
//! | length: uint32       | payload: UTF-8 JSON object      |
//! | native byte order    | exactly `length` bytes          |
//! +----------------------+---------------------------------+
//! ```
//!
//! Limits: 4,194,304 bytes extension to host, 1,000,000 bytes host to extension. A zero length or an announced
//! length above the limit cannot be skipped safely (the stream would desynchronise), so the caller closes the
//! connection. The length is checked **before** any allocation.

use std::io::{Read, Write};

use serde_json::Value;
use zeus_types::limits::{MAX_FRAME_TO_DAEMON_BYTES, MAX_FRAME_TO_EXTENSION_BYTES, MAX_JSON_DEPTH};

use crate::error::HostError;

/// Largest frame the extension may send.
pub const MAX_FROM_EXTENSION: usize = MAX_FRAME_TO_DAEMON_BYTES;
/// Largest frame the host may send to the extension.
pub const MAX_TO_EXTENSION: usize = MAX_FRAME_TO_EXTENSION_BYTES;

/// Read one frame's payload bytes. `Ok(None)` is a clean end of stream at a frame boundary.
pub fn read_frame<R: Read>(reader: &mut R, limit: usize) -> Result<Option<Vec<u8>>, HostError> {
    let mut prefix = [0u8; 4];
    let mut filled = 0;
    while filled < prefix.len() {
        let n = reader.read(&mut prefix[filled..])?;
        if n == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(HostError::Truncated)
            };
        }
        filled += n;
    }
    let length = u64::from(u32::from_ne_bytes(prefix));
    if length == 0 {
        return Err(HostError::ZeroLength);
    }
    if length > limit as u64 {
        return Err(HostError::FrameTooLarge { length, limit });
    }
    let mut payload = vec![0u8; length as usize];
    reader
        .read_exact(&mut payload)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => HostError::Truncated,
            _ => HostError::Io(e),
        })?;
    Ok(Some(payload))
}

/// Parse a payload as a JSON object within the nesting limit of 00 section 10.
pub fn parse_payload(payload: &[u8]) -> Result<Value, HostError> {
    if depth_exceeds(payload, MAX_JSON_DEPTH) {
        return Err(HostError::InvalidJson);
    }
    let value: Value = serde_json::from_slice(payload).map_err(|_| HostError::InvalidJson)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(HostError::InvalidJson)
    }
}

/// Cheap bracket-depth pre-check so hostile nesting never reaches the parser's recursion.
fn depth_exceeds(payload: &[u8], max: usize) -> bool {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for &byte in payload {
        if in_string {
            match (escaped, byte) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > max {
                    return true;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// Write one frame. Refuses a payload above `limit` (the caller must split or page, spec 02 section 3.3 rule 1).
pub fn write_frame<W: Write>(
    writer: &mut W,
    payload: &[u8],
    limit: usize,
) -> Result<(), HostError> {
    if payload.is_empty() {
        return Err(HostError::ZeroLength);
    }
    if payload.len() > limit {
        return Err(HostError::FrameTooLarge {
            length: payload.len() as u64,
            limit,
        });
    }
    let length = u32::try_from(payload.len()).map_err(|_| HostError::FrameTooLarge {
        length: payload.len() as u64,
        limit,
    })?;
    writer.write_all(&length.to_ne_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn framed(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_frame(&mut out, payload, MAX_TO_EXTENSION).unwrap();
        out
    }

    #[test]
    fn the_spec_example_has_the_documented_prefix() {
        // 02 section 3.3: the 41-byte ping is preceded by 29 00 00 00 on a little-endian machine.
        let payload = br#"{"v":1,"id":"e1","type":"ping","body":{}}"#;
        assert_eq!(payload.len(), 41);
        let bytes = framed(payload);
        assert_eq!(&bytes[..4], 41u32.to_ne_bytes());
        if cfg!(target_endian = "little") {
            assert_eq!(&bytes[..4], [0x29, 0, 0, 0]);
        }
    }

    #[test]
    fn round_trip_and_clean_eof() {
        let mut stream =
            std::io::Cursor::new([framed(b"{\"a\":1}"), framed(b"{\"b\":2}")].concat());
        assert_eq!(
            read_frame(&mut stream, 1024).unwrap().unwrap(),
            b"{\"a\":1}"
        );
        assert_eq!(
            read_frame(&mut stream, 1024).unwrap().unwrap(),
            b"{\"b\":2}"
        );
        assert!(read_frame(&mut stream, 1024).unwrap().is_none());
    }

    #[test]
    fn zero_and_oversized_lengths_are_rejected_before_allocating() {
        let mut zero = std::io::Cursor::new(0u32.to_ne_bytes().to_vec());
        assert!(matches!(
            read_frame(&mut zero, 1024),
            Err(HostError::ZeroLength)
        ));
        let mut huge = std::io::Cursor::new(u32::MAX.to_ne_bytes().to_vec());
        assert!(matches!(
            read_frame(&mut huge, MAX_FROM_EXTENSION),
            Err(HostError::FrameTooLarge { .. })
        ));
        let mut just_over =
            std::io::Cursor::new(((MAX_FROM_EXTENSION + 1) as u32).to_ne_bytes().to_vec());
        assert!(matches!(
            read_frame(&mut just_over, MAX_FROM_EXTENSION),
            Err(HostError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn truncation_is_detected_in_prefix_and_payload() {
        let mut short_prefix = std::io::Cursor::new(vec![1, 0]);
        assert!(matches!(
            read_frame(&mut short_prefix, 1024),
            Err(HostError::Truncated)
        ));
        let mut short_payload =
            std::io::Cursor::new([10u32.to_ne_bytes().to_vec(), b"{\"a\"".to_vec()].concat());
        assert!(matches!(
            read_frame(&mut short_payload, 1024),
            Err(HostError::Truncated)
        ));
    }

    #[test]
    fn writes_over_the_limit_are_refused() {
        let mut out = Vec::new();
        let big = vec![b'x'; MAX_TO_EXTENSION + 1];
        assert!(matches!(
            write_frame(&mut out, &big, MAX_TO_EXTENSION),
            Err(HostError::FrameTooLarge { .. })
        ));
        assert!(out.is_empty());
        assert!(matches!(
            write_frame(&mut out, b"", MAX_TO_EXTENSION),
            Err(HostError::ZeroLength)
        ));
    }

    #[test]
    fn payloads_must_be_json_objects_within_the_depth_limit() {
        assert!(parse_payload(b"{\"a\":[1,2]}").is_ok());
        assert!(parse_payload(b"[1]").is_err());
        assert!(parse_payload(b"\"x\"").is_err());
        assert!(parse_payload(&[0xff, 0xfe]).is_err());
        let deep = format!("{}1{}", "[".repeat(40), "]".repeat(40));
        assert!(parse_payload(format!("{{\"a\":{deep}}}").as_bytes()).is_err());
        let string_with_brackets = format!("{{\"a\":\"{}\"}}", "[".repeat(100));
        assert!(parse_payload(string_with_brackets.as_bytes()).is_ok());
    }

    proptest! {
        #[test]
        fn any_object_round_trips(key in "[a-z]{1,8}", value in "[ -~]{0,64}") {
            let payload = serde_json::to_vec(&serde_json::json!({ key.clone(): value.clone() })).unwrap();
            let mut stream = std::io::Cursor::new(framed(&payload));
            let read = read_frame(&mut stream, 4096).unwrap().unwrap();
            let parsed = parse_payload(&read).unwrap();
            prop_assert_eq!(parsed[&key].as_str().unwrap(), value.as_str());
        }

        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            let mut stream = std::io::Cursor::new(bytes);
            let _ = read_frame(&mut stream, 64);
        }
    }
}
