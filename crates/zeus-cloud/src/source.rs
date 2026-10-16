use crate::{CloudError, CloudResult, UploadSource};
use bytes::Bytes;
use futures::stream;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

pub(crate) const IO_CHUNK: usize = 64 * 1024;

pub(crate) struct CheckedSource {
    source: UploadSource,
    remaining: u64,
    expected_hash: Option<String>,
    hash: Sha256,
    done: bool,
    cancel: CancellationToken,
}

impl CheckedSource {
    pub(crate) fn new(
        source: UploadSource,
        size: u64,
        hash: Option<String>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            source,
            remaining: size,
            expected_hash: hash,
            hash: Sha256::new(),
            done: false,
            cancel,
        }
    }

    pub(crate) async fn read_chunk(&mut self, cap: usize) -> CloudResult<Vec<u8>> {
        if self.done {
            return Ok(Vec::new());
        }
        let length = self.remaining.min(cap as u64) as usize;
        let mut result = vec![0; length];
        let mut used = 0;
        while used < length {
            let n = tokio::select! { biased;
                _ = self.cancel.cancelled() => return Err(CloudError::Cancelled),
                n = self.source.read(&mut result[used..]) => n.map_err(|_| CloudError::Source)?,
            };
            if n == 0 {
                return Err(CloudError::SizeMismatch);
            }
            used += n;
        }
        self.remaining -= length as u64;
        self.hash.update(&result);
        if self.remaining == 0 {
            let mut extra = [0u8; 1];
            let n = tokio::select! { biased;
                _ = self.cancel.cancelled() => return Err(CloudError::Cancelled),
                n = self.source.read(&mut extra) => n.map_err(|_| CloudError::Source)?,
            };
            if n != 0 {
                return Err(CloudError::SizeMismatch);
            }
            self.done = true;
            if self
                .expected_hash
                .as_ref()
                .is_some_and(|v| !v.eq_ignore_ascii_case(&self.digest()))
            {
                return Err(CloudError::HashMismatch);
            }
        }
        Ok(result)
    }

    pub(crate) fn digest(&self) -> String {
        hex::encode(self.hash.clone().finalize())
    }

    pub(crate) async fn skip(&mut self, mut bytes: u64) -> CloudResult<()> {
        if bytes > self.remaining {
            return Err(CloudError::CheckpointMismatch);
        }
        while bytes > 0 {
            let part = self.read_chunk(bytes.min(IO_CHUNK as u64) as usize).await?;
            bytes -= part.len() as u64;
        }
        Ok(())
    }
}

/// The final digest is published only after exact-size EOF and expected-hash validation.
pub(crate) type StreamOutcome = Arc<Mutex<Option<CloudResult<String>>>>;

pub(crate) fn body(source: CheckedSource) -> (reqwest::Body, StreamOutcome) {
    let result = Arc::new(Mutex::new(None));
    let copy = Arc::clone(&result);
    let chunks = stream::unfold(Some(source), move |state| {
        let result = Arc::clone(&copy);
        async move {
            let mut source = state?;
            match source.read_chunk(IO_CHUNK).await {
                Ok(bytes) => {
                    if source.done {
                        *result.lock().expect("cloud stream outcome") = Some(Ok(source.digest()));
                    }
                    if bytes.is_empty() {
                        None
                    } else {
                        Some((Ok::<_, std::io::Error>(Bytes::from(bytes)), Some(source)))
                    }
                }
                Err(error) => {
                    *result.lock().expect("cloud stream outcome") = Some(Err(error));
                    Some((Err(std::io::Error::other("cloud source failed")), None))
                }
            }
        }
    });
    (reqwest::Body::wrap_stream(chunks), result)
}

pub(crate) fn stream_result(outcome: &StreamOutcome) -> CloudResult<String> {
    outcome
        .lock()
        .map_err(|_| CloudError::Source)?
        .take()
        .ok_or(CloudError::Source)?
}
