//! Newline-delimited JSON framing.
//!
//! One JSON-RPC message per line, UTF-8, terminated by `\n`. Messages are serialised compactly, so JSON's own
//! escaping guarantees there is no raw newline inside a message. A line longer than the connection's limit is a
//! protocol violation: the receiver answers `api.payload_too_large` if it can and **closes the connection**,
//! because the stream cannot be resynchronised cheaply and the sender may be hostile. Lines are bounded
//! (00 section 10): 1 MiB for requests, 16 MiB for responses, 5 MiB on native-host connections.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use zeus_types::limits::{MAX_API_REQUEST_BYTES, MAX_API_RESPONSE_BYTES};

use crate::error::{ApiError, codes};
use crate::jsonrpc::Message;

/// Line limit on native-host connections: a 4 MiB extension frame plus the JSON-RPC envelope (ADR 0002).
pub const MAX_HOST_LINE_BYTES: usize = 5 * 1024 * 1024;

/// Direction-specific line limits of one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineLimits {
    /// Longest line the server accepts from the client.
    pub max_request_bytes: usize,
    /// Longest line the client accepts from the server.
    pub max_response_bytes: usize,
}

impl LineLimits {
    /// Limits of an ordinary connection.
    pub const STANDARD: Self = Self {
        max_request_bytes: MAX_API_REQUEST_BYTES,
        max_response_bytes: MAX_API_RESPONSE_BYTES,
    };
    /// Limits of a `native_host` connection.
    pub const NATIVE_HOST: Self = Self {
        max_request_bytes: MAX_HOST_LINE_BYTES,
        max_response_bytes: MAX_API_RESPONSE_BYTES,
    };
}

impl Default for LineLimits {
    fn default() -> Self {
        Self::STANDARD
    }
}

/// Framing failures. All of them end the connection.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// A line exceeded the limit.
    #[error("line longer than {limit} bytes")]
    TooLong {
        /// The limit that was exceeded.
        limit: usize,
    },
    /// The stream ended in the middle of a line.
    #[error("stream ended inside a line")]
    Truncated,
    /// I/O failure.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

impl From<FrameError> for ApiError {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::TooLong { limit } => {
                ApiError::new(codes::PAYLOAD_TOO_LARGE, "message line exceeds the limit")
                    .with_detail("limit_bytes", limit as u64)
            }
            FrameError::Truncated | FrameError::Io(_) => {
                ApiError::new(codes::UNAVAILABLE, "connection closed")
            }
        }
    }
}

/// Serialise a message as one line including the trailing newline.
pub fn encode_line(message: &Message) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = message.to_vec()?;
    debug_assert!(
        !bytes.contains(&b'\n'),
        "compact JSON never contains a raw newline"
    );
    bytes.push(b'\n');
    Ok(bytes)
}

/// Incremental line splitter for callers that read raw chunks (tests, the native host). It never buffers more
/// than `max_line_bytes` plus one chunk.
#[derive(Debug)]
pub struct LineDecoder {
    max_line_bytes: usize,
    buffer: Vec<u8>,
    /// Bytes of `buffer` already scanned for a newline.
    scanned: usize,
}

impl LineDecoder {
    /// A decoder that rejects lines longer than `max_line_bytes` (excluding the newline).
    pub fn new(max_line_bytes: usize) -> Self {
        Self {
            max_line_bytes,
            buffer: Vec::new(),
            scanned: 0,
        }
    }

    /// Feed bytes read from the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next complete line (without the newline and without a trailing `\r`), `Ok(None)` when more bytes are
    /// needed, or an error when the pending line is too long. Empty lines are skipped.
    pub fn next_line(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        loop {
            let found = self.buffer[self.scanned..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| p + self.scanned);
            match found {
                Some(end) => {
                    let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
                    self.scanned = 0;
                    line.pop();
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    if line.len() > self.max_line_bytes {
                        return Err(FrameError::TooLong {
                            limit: self.max_line_bytes,
                        });
                    }
                    if line.is_empty() {
                        continue;
                    }
                    return Ok(Some(line));
                }
                None => {
                    if self.buffer.len() > self.max_line_bytes {
                        return Err(FrameError::TooLong {
                            limit: self.max_line_bytes,
                        });
                    }
                    self.scanned = self.buffer.len();
                    return Ok(None);
                }
            }
        }
    }

    /// Call at end of stream: an unfinished line is an error.
    pub fn finish(&self) -> Result<(), FrameError> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err(FrameError::Truncated)
        }
    }
}

/// Read one line from an async reader, enforcing `max_line_bytes` while reading (never allocating more than the
/// limit plus one buffer). `Ok(None)` is a clean end of stream.
pub async fn read_line<R>(
    reader: &mut R,
    max_line_bytes: usize,
) -> Result<Option<Vec<u8>>, FrameError>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.iter().all(u8::is_ascii_whitespace) {
                Ok(None)
            } else {
                Err(FrameError::Truncated)
            };
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(position) => {
                if line.len() + position > max_line_bytes + 1 {
                    return Err(FrameError::TooLong {
                        limit: max_line_bytes,
                    });
                }
                line.extend_from_slice(&available[..position]);
                reader.consume(position + 1);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.len() > max_line_bytes {
                    return Err(FrameError::TooLong {
                        limit: max_line_bytes,
                    });
                }
                if line.is_empty() {
                    continue;
                }
                return Ok(Some(line));
            }
            None => {
                let take = available.len();
                if line.len() + take > max_line_bytes + 1 {
                    return Err(FrameError::TooLong {
                        limit: max_line_bytes,
                    });
                }
                line.extend_from_slice(available);
                reader.consume(take);
            }
        }
    }
}

/// Write one message as a line and flush.
pub async fn write_message<W>(writer: &mut W, message: &Message) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let line = encode_line(message).map_err(|e| FrameError::Io(std::io::Error::other(e)))?;
    writer.write_all(&line).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::{Notification, Request};
    use proptest::prelude::*;
    use tokio::io::BufReader;

    #[test]
    fn encode_ends_with_one_newline_and_has_no_other() {
        let message = Message::Request(Request::new(
            1,
            "daemon.ping",
            Some(serde_json::json!({"text": "a\nb"})),
        ));
        let line = encode_line(&message).unwrap();
        assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
        assert_eq!(line.last(), Some(&b'\n'));
    }

    #[test]
    fn decoder_handles_partial_reads_crlf_and_blank_lines() {
        let mut decoder = LineDecoder::new(64);
        decoder.push(b"{\"a\":1}\r\n\n{\"b\"");
        assert_eq!(decoder.next_line().unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(decoder.next_line().unwrap(), None);
        decoder.push(b":2}\n");
        assert_eq!(decoder.next_line().unwrap().unwrap(), b"{\"b\":2}");
        assert_eq!(decoder.next_line().unwrap(), None);
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn decoder_rejects_long_lines_before_the_newline_arrives() {
        let mut decoder = LineDecoder::new(8);
        decoder.push(&[b'x'; 20]);
        assert!(matches!(
            decoder.next_line(),
            Err(FrameError::TooLong { limit: 8 })
        ));
    }

    #[test]
    fn decoder_flags_a_truncated_stream() {
        let mut decoder = LineDecoder::new(64);
        decoder.push(b"{\"a\"");
        assert_eq!(decoder.next_line().unwrap(), None);
        assert!(matches!(decoder.finish(), Err(FrameError::Truncated)));
    }

    proptest! {
        #[test]
        fn chunking_does_not_change_the_lines(lines in proptest::collection::vec("[a-z0-9 ]{1,30}", 1..10), cut in 1usize..20) {
            let stream: Vec<u8> = lines.iter().flat_map(|l| format!("{l}\n").into_bytes()).collect();
            let mut decoder = LineDecoder::new(64);
            let mut out = Vec::new();
            for chunk in stream.chunks(cut) {
                decoder.push(chunk);
                while let Some(line) = decoder.next_line().unwrap() {
                    out.push(String::from_utf8(line).unwrap());
                }
            }
            prop_assert_eq!(out, lines);
        }
    }

    #[tokio::test]
    async fn async_reader_round_trip_and_limits() {
        let bytes = b"{\"a\":1}\n{\"b\":2}\n".to_vec();
        let mut reader = BufReader::new(&bytes[..]);
        assert_eq!(
            read_line(&mut reader, 64).await.unwrap().unwrap(),
            b"{\"a\":1}"
        );
        assert_eq!(
            read_line(&mut reader, 64).await.unwrap().unwrap(),
            b"{\"b\":2}"
        );
        assert_eq!(read_line(&mut reader, 64).await.unwrap(), None);

        let long = [b'x'; 100];
        let mut reader = BufReader::with_capacity(16, &long[..]);
        assert!(matches!(
            read_line(&mut reader, 32).await,
            Err(FrameError::TooLong { limit: 32 })
        ));

        let mut reader = BufReader::new(&b"{\"a\":"[..]);
        assert!(matches!(
            read_line(&mut reader, 64).await,
            Err(FrameError::Truncated)
        ));
    }

    #[tokio::test]
    async fn write_message_appends_a_newline() {
        let mut out = Vec::new();
        write_message(
            &mut out,
            &Message::Notification(Notification::new("daemon.stopping", None)),
        )
        .await
        .unwrap();
        assert!(out.ends_with(b"\n"));
    }
}
