//! RFC 8785 JSON Canonicalization Scheme for the subset Zeus hashes: objects, arrays, strings, booleans, null and
//! **integers** (00 section 7 forbids floats in hashed documents). The core repository has the reference
//! implementation in `zeus-verify`; this copy exists so plan hashing does not pull the verification crate.

use serde_json::Value;
use thiserror::Error;

/// Reasons a value cannot be canonicalised for hashing.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CanonicalError {
    /// A number that is not an integer within +/- (2^53 - 1).
    #[error("number is not an integer within +/-(2^53-1)")]
    Number,
    /// The value could not be serialised.
    #[error("serialisation failed: {0}")]
    Serialize(String),
}

const MAX_SAFE: i64 = 9_007_199_254_740_991;

/// Canonical JCS bytes of `value`.
pub fn jcs(value: &Value) -> Result<Vec<u8>, CanonicalError> {
    let mut out = Vec::new();
    write_value(value, &mut out)?;
    Ok(out)
}

/// Canonical JCS bytes of any serialisable value.
pub fn jcs_of<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    let value =
        serde_json::to_value(value).map_err(|e| CanonicalError::Serialize(e.to_string()))?;
    jcs(&value)
}

fn write_value(value: &Value, out: &mut Vec<u8>) -> Result<(), CanonicalError> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(n) => {
            let text = if let Some(i) = n.as_i64() {
                if i.abs() > MAX_SAFE {
                    return Err(CanonicalError::Number);
                }
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                if u > MAX_SAFE as u64 {
                    return Err(CanonicalError::Number);
                }
                u.to_string()
            } else {
                return Err(CanonicalError::Number);
            };
            out.extend_from_slice(text.as_bytes());
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_value(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            // JCS sorts member names by UTF-16 code units.
            entries.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            out.push(b'{');
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_string(key, out);
                out.push(b':');
                write_value(item, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn write_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for c in s.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{9}' => out.extend_from_slice(b"\\t"),
            '\u{a}' => out.extend_from_slice(b"\\n"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            '\u{d}' => out.extend_from_slice(b"\\r"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes())
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_keys_and_drops_whitespace() {
        let value = json!({"b": [1, 2, {"z": null, "a": true}], "a": "x"});
        assert_eq!(
            String::from_utf8(jcs(&value).unwrap()).unwrap(),
            r#"{"a":"x","b":[1,2,{"a":true,"z":null}]}"#
        );
    }

    #[test]
    fn sorts_by_utf16_code_units() {
        // RFC 8785 section 3.2.3: U+1F600 (surrogates D83D DE00) sorts before U+FB33.
        let value = json!({"\u{fb33}": 1, "\u{1f600}": 2});
        let text = String::from_utf8(jcs(&value).unwrap()).unwrap();
        assert!(text.find('\u{1f600}').unwrap() < text.find('\u{fb33}').unwrap());
    }

    #[test]
    fn escapes_controls_minimally() {
        let value = json!("a\u{1f}\n\"\\é/");
        assert_eq!(
            String::from_utf8(jcs(&value).unwrap()).unwrap(),
            "\"a\\u001f\\n\\\"\\\\é/\""
        );
    }

    #[test]
    fn rejects_floats_and_unsafe_integers() {
        assert_eq!(jcs(&json!(1.5)), Err(CanonicalError::Number));
        assert_eq!(
            jcs(&json!(9_007_199_254_740_992u64)),
            Err(CanonicalError::Number)
        );
        assert!(jcs(&json!(9_007_199_254_740_991u64)).is_ok());
    }
}
