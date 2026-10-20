use crate::http_client::{ProviderHttp, json, safe_id, send, size, string};
use crate::model::receipt;
use crate::source::CheckedSource;
use crate::*;
use async_trait::async_trait;
use reqwest::Method;
use tokio_util::sync::CancellationToken;

const CHUNK: usize = 1024 * 1024;
const FIELDS: &str = "id,size,sha256Checksum,appProperties";

/// Native Google Drive v3 resumable upload. The pre-generated file ID is saved before creation,
/// providing duplicate-safe reconciliation even if session initiation acknowledgement is lost.
pub struct GoogleDrive {
    http: ProviderHttp,
    parent_id: String,
}

impl std::fmt::Debug for GoogleDrive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GoogleDrive([REDACTED])")
    }
}

impl GoogleDrive {
    /// Construct using an explicitly configured OAuth access token and existing folder ID.
    pub fn new(token: SecretString, parent_id: String) -> CloudResult<Self> {
        safe_id(&parent_id).map_err(|_| CloudError::InvalidRequest)?;
        Ok(Self {
            http: ProviderHttp::new("https://www.googleapis.com/", token, false)?,
            parent_id,
        })
    }

    pub(crate) fn validate_parent(parent: &str) -> CloudResult<()> {
        safe_id(parent).map_err(|_| CloudError::InvalidRequest)
    }

    fn path(&self, request: &UploadRequest) -> String {
        format!("{}/{}", self.parent_id, request.unique_name())
    }

    fn check(&self, cp: &PrivateCheckpoint) -> CloudResult<()> {
        cp.request.validate()?;
        cp.validate_binding(
            ProviderKind::GoogleDrive,
            &cp.request,
            &self.path(&cp.request),
        )?;
        safe_id(
            cp.object_id
                .as_deref()
                .ok_or(CloudError::CheckpointMismatch)?,
        )?;
        Ok(())
    }

    async fn metadata(
        &self,
        cp: &PrivateCheckpoint,
        cancel: &CancellationToken,
    ) -> CloudResult<Option<serde_json::Value>> {
        self.check(cp)?;
        let id = cp
            .object_id
            .as_deref()
            .ok_or(CloudError::CheckpointMismatch)?;
        let mut url = self.http.url(&format!("drive/v3/files/{id}"))?;
        url.query_pairs_mut()
            .append_pair("fields", FIELDS)
            .append_pair("supportsAllDrives", "true");
        let response = send(self.http.authorized(Method::GET, url)?, cancel).await?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        let value = json(response, cancel).await?;
        if string(&value, "id")? != id
            || value
                .pointer("/appProperties/zeus_operation")
                .and_then(|v| v.as_str())
                != Some(&cp.request.operation_id)
        {
            return Err(CloudError::CheckpointMismatch);
        }
        Ok(Some(value))
    }

    fn artifact(
        &self,
        cp: &PrivateCheckpoint,
        value: &serde_json::Value,
        local: Option<String>,
    ) -> CloudResult<RemoteArtifact> {
        let id = string(value, "id")?;
        if Some(&id) != cp.object_id.as_ref() {
            return Err(CloudError::CheckpointMismatch);
        }
        let remote = value
            .get("sha256Checksum")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        receipt(cp, id, size(value, "size")?, remote, local)
    }

    async fn allocate(
        &self,
        request: &UploadRequest,
        sink: &dyn CheckpointSink,
        cancel: &CancellationToken,
    ) -> CloudResult<PrivateCheckpoint> {
        let mut url = self.http.url("drive/v3/files/generateIds")?;
        url.query_pairs_mut()
            .append_pair("count", "1")
            .append_pair("space", "drive")
            .append_pair("type", "files");
        let value = json(
            send(self.http.authorized(Method::GET, url)?, cancel).await?,
            cancel,
        )
        .await?;
        let id = value
            .get("ids")
            .and_then(|v| v.as_array())
            .filter(|v| v.len() == 1)
            .and_then(|v| v[0].as_str())
            .ok_or(CloudError::Protocol)?
            .to_owned();
        safe_id(&id)?;
        let cp = PrivateCheckpoint {
            provider: ProviderKind::GoogleDrive,
            request: request.clone(),
            object_path: self.path(request),
            object_id: Some(id),
            session_url: None,
            committed_bytes: 0,
            phase: CheckpointPhase::Prepared,
            provider_state: serde_json::Value::Null,
        };
        sink.save(&cp).await?;
        Ok(cp)
    }

    async fn initiate(
        &self,
        cp: &mut PrivateCheckpoint,
        sink: &dyn CheckpointSink,
        cancel: &CancellationToken,
    ) -> CloudResult<Option<RemoteArtifact>> {
        let mut url = self.http.url("upload/drive/v3/files")?;
        url.query_pairs_mut()
            .append_pair("uploadType", "resumable")
            .append_pair("fields", FIELDS)
            .append_pair("supportsAllDrives", "true");
        let metadata = serde_json::json!({ "id": cp.object_id, "name": cp.request.unique_name(), "parents": [self.parent_id],
            "appProperties": { "zeus_operation": cp.request.operation_id } });
        sink.save(cp).await?;
        let response = send(
            self.http
                .authorized(Method::POST, url)?
                .json(&metadata)
                .header("X-Upload-Content-Type", "application/octet-stream")
                .header("X-Upload-Content-Length", cp.request.size),
            cancel,
        )
        .await?;
        if response.status().as_u16() == 409 {
            return self
                .reconcile(cp, cancel)
                .await?
                .map(Some)
                .ok_or(CloudError::ReconciliationRequired);
        }
        if !response.status().is_success() {
            return Err(CloudError::Http(response.status().as_u16()));
        }
        let location = response
            .headers()
            .get(http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(CloudError::Protocol)?;
        let location = self
            .http
            .same_origin_location(location, "/upload/drive/v3/files")?;
        cp.session_url = Some(location.to_string());
        cp.phase = CheckpointPhase::Uploading;
        sink.save(cp).await?;
        Ok(None)
    }

    async fn status(
        &self,
        cp: &mut PrivateCheckpoint,
        cancel: &CancellationToken,
    ) -> CloudResult<Option<RemoteArtifact>> {
        let url = self.http.same_origin_location(
            cp.session_url
                .as_deref()
                .ok_or(CloudError::CheckpointMismatch)?,
            "/upload/drive/v3/files",
        )?;
        let response = send(
            self.http
                .authorized(Method::PUT, url)?
                .header(http::header::CONTENT_LENGTH, 0)
                .header(
                    http::header::CONTENT_RANGE,
                    format!("bytes */{}", cp.request.size),
                ),
            cancel,
        )
        .await?;
        if response.status().as_u16() == 308 {
            cp.committed_bytes = confirmed_offset(response.headers(), cp.request.size)?;
            return Ok(None);
        }
        if response.status().is_success() {
            return Ok(Some(self.artifact(
                cp,
                &json(response, cancel).await?,
                None,
            )?));
        }
        if response.status().as_u16() == 404 {
            return Err(CloudError::ReconciliationRequired);
        }
        Err(CloudError::Http(response.status().as_u16()))
    }
}

#[async_trait]
impl CloudProvider for GoogleDrive {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider: ProviderKind::GoogleDrive,
            staged_upload: true,
            stream_upload: true,
            remote_import: false,
            resumable_upload: true,
            remote_cancel: false,
            remote_import_hard_byte_limit: false,
            remote_sha256: true,
        }
    }

    async fn upload(
        &self,
        request: &UploadRequest,
        source: UploadSource,
        checkpoint: Option<PrivateCheckpoint>,
        sink: &dyn CheckpointSink,
        cancel: &CancellationToken,
    ) -> CloudResult<RemoteArtifact> {
        request.validate()?;
        self.capabilities().plan(
            request.mode,
            Some(request.size),
            request.expected_sha256.as_deref(),
            checkpoint.is_some(),
        )?;
        if request.mode == DeliveryMode::RemoteImport {
            return Err(CloudError::Unsupported(
                CapabilityReason::RemoteImportUnavailable,
            ));
        }
        let resumed = checkpoint.is_some();
        let mut cp = match checkpoint {
            Some(cp) => {
                cp.validate_binding(ProviderKind::GoogleDrive, request, &self.path(request))?;
                self.check(&cp)?;
                cp
            }
            None => self.allocate(request, sink, cancel).await?,
        };
        if cp.phase == CheckpointPhase::Complete {
            return self
                .reconcile(&cp, cancel)
                .await?
                .ok_or(CloudError::ReconciliationRequired);
        }
        if cp.session_url.is_none() {
            if let Some(result) = self.initiate(&mut cp, sink, cancel).await? {
                return Ok(result);
            }
        } else if resumed {
            if let Some(result) = self.status(&mut cp, cancel).await? {
                return Ok(result);
            }
            sink.save(&cp).await?;
        }
        let mut reader = CheckedSource::new(
            source,
            request.size,
            request.expected_sha256.clone(),
            cancel.clone(),
        );
        reader.skip(cp.committed_bytes).await?;
        loop {
            let chunk = reader.read_chunk(CHUNK).await?;
            let start = cp.committed_bytes;
            let sent_end = start
                .checked_add(chunk.len() as u64)
                .ok_or(CloudError::SizeMismatch)?;
            let range = if chunk.is_empty() {
                format!("bytes */{}", request.size)
            } else {
                format!("bytes {start}-{}/{}", sent_end - 1, request.size)
            };
            let url = self.http.same_origin_location(
                cp.session_url
                    .as_deref()
                    .ok_or(CloudError::CheckpointMismatch)?,
                "/upload/drive/v3/files",
            )?;
            sink.save(&cp).await?;
            let response = send(
                self.http
                    .authorized(Method::PUT, url)?
                    .header(http::header::CONTENT_RANGE, range)
                    .header(http::header::CONTENT_TYPE, "application/octet-stream")
                    .header(http::header::CONTENT_LENGTH, chunk.len())
                    .body(chunk),
                cancel,
            )
            .await?;
            if response.status().as_u16() == 308 {
                let confirmed = confirmed_offset(response.headers(), request.size)?;
                // Forward-only streams cannot replay partially acknowledged chunks. The durable
                // offset is saved and a staged caller can reopen the source for an explicit resume.
                if confirmed < start || confirmed > sent_end {
                    return Err(CloudError::Protocol);
                }
                cp.committed_bytes = confirmed;
                sink.save(&cp).await?;
                if confirmed != sent_end || confirmed == request.size {
                    return Err(CloudError::ReconciliationRequired);
                }
                continue;
            }
            if !response.status().is_success() {
                return Err(CloudError::Http(response.status().as_u16()));
            }
            if sent_end != request.size {
                return Err(CloudError::Protocol);
            }
            let result =
                self.artifact(&cp, &json(response, cancel).await?, Some(reader.digest()))?;
            cp.committed_bytes = request.size;
            cp.phase = CheckpointPhase::Complete;
            sink.save(&cp).await?;
            return Ok(result);
        }
    }

    async fn reconcile(
        &self,
        cp: &PrivateCheckpoint,
        cancel: &CancellationToken,
    ) -> CloudResult<Option<RemoteArtifact>> {
        self.metadata(cp, cancel)
            .await?
            .map(|v| self.artifact(cp, &v, None))
            .transpose()
    }

    async fn cleanup(&self, cp: &PrivateCheckpoint, cancel: &CancellationToken) -> CloudResult<()> {
        if self.metadata(cp, cancel).await?.is_none() {
            return Ok(());
        }
        let id = cp
            .object_id
            .as_deref()
            .ok_or(CloudError::CheckpointMismatch)?;
        let mut url = self.http.url(&format!("drive/v3/files/{id}"))?;
        url.query_pairs_mut()
            .append_pair("supportsAllDrives", "true");
        let response = send(self.http.authorized(Method::DELETE, url)?, cancel).await?;
        if response.status().is_success() || response.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(CloudError::Http(response.status().as_u16()))
        }
    }
}

fn confirmed_offset(headers: &http::HeaderMap, total: u64) -> CloudResult<u64> {
    let Some(value) = headers.get(http::header::RANGE) else {
        return Ok(0);
    };
    let value = value.to_str().map_err(|_| CloudError::Protocol)?;
    let end = value
        .strip_prefix("bytes=0-")
        .ok_or(CloudError::Protocol)?
        .parse::<u64>()
        .map_err(|_| CloudError::Protocol)?;
    let offset = end
        .checked_add(1)
        .filter(|v| *v <= total)
        .ok_or(CloudError::Protocol)?;
    Ok(offset)
}

#[cfg(test)]
impl GoogleDrive {
    pub(crate) fn test_endpoint(endpoint: &str) -> Self {
        Self {
            http: ProviderHttp::new(
                endpoint,
                SecretString::new("ZEUS-CANARY-token".into()),
                false,
            )
            .expect("test endpoint"),
            parent_id: "root".into(),
        }
    }
}
