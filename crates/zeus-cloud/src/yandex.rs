use crate::http_client::{ProviderHttp, json, send, size, string};
use crate::model::receipt;
use crate::source::{CheckedSource, body, stream_result};
use crate::*;
use async_trait::async_trait;
use reqwest::Method;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Yandex Disk REST upload and asynchronous provider-side URL import.
/// Import cancellation and hard remote byte limits are deliberately not advertised.
pub struct YandexDisk {
    http: ProviderHttp,
    root_path: String,
}
impl std::fmt::Debug for YandexDisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("YandexDisk([REDACTED])")
    }
}

impl YandexDisk {
    /// Construct with an explicit OAuth token and existing `disk:/` or `app:/` folder.
    pub fn new(token: SecretString, root_path: String) -> CloudResult<Self> {
        Self::validate_root(&root_path)?;
        Ok(Self {
            http: ProviderHttp::new("https://cloud-api.yandex.net/", token, true)?,
            root_path,
        })
    }

    pub(crate) fn validate_root(root: &str) -> CloudResult<()> {
        let path = root
            .strip_prefix("disk:/")
            .or_else(|| root.strip_prefix("app:/"))
            .ok_or(CloudError::InvalidRequest)?;
        if root.len() > 1024
            || path.split('/').any(|p| p == ".." || p == ".")
            || path
                .chars()
                .any(|c| c.is_control() || matches!(c, '\\' | ':' | '?' | '#'))
        {
            return Err(CloudError::InvalidRequest);
        }
        Ok(())
    }

    fn path(&self, request: &UploadRequest) -> String {
        format!(
            "{}/{}",
            self.root_path.trim_end_matches('/'),
            request.unique_name()
        )
    }

    fn checkpoint(&self, request: &UploadRequest) -> PrivateCheckpoint {
        PrivateCheckpoint {
            provider: ProviderKind::YandexDisk,
            request: request.clone(),
            object_path: self.path(request),
            object_id: None,
            session_url: None,
            committed_bytes: 0,
            phase: CheckpointPhase::Prepared,
            provider_state: serde_json::Value::Null,
        }
    }

    fn check(&self, cp: &PrivateCheckpoint) -> CloudResult<()> {
        cp.request.validate()?;
        cp.validate_binding(
            ProviderKind::YandexDisk,
            &cp.request,
            &self.path(&cp.request),
        )
    }

    fn operation_url(&self, cp: &PrivateCheckpoint) -> CloudResult<Url> {
        self.check(cp)?;
        if cp.request.mode != DeliveryMode::RemoteImport {
            return Err(CloudError::CheckpointMismatch);
        }
        self.operation_location(
            cp.session_url
                .as_deref()
                .ok_or(CloudError::ReconciliationRequired)?,
        )
    }

    fn operation_location(&self, raw: &str) -> CloudResult<Url> {
        let url = self
            .http
            .same_origin_location(raw, "/v1/disk/operations/")?;
        let operation = url
            .path()
            .strip_prefix("/v1/disk/operations/")
            .ok_or(CloudError::UntrustedLocation)?;
        if operation.is_empty()
            || operation.len() > 256
            || !operation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || url.query().is_some()
        {
            return Err(CloudError::UntrustedLocation);
        }
        Ok(url)
    }

    async fn metadata(
        &self,
        cp: &PrivateCheckpoint,
        local: Option<String>,
        cancel: &CancellationToken,
    ) -> CloudResult<Option<RemoteArtifact>> {
        self.check(cp)?;
        let mut url = self.http.url("v1/disk/resources")?;
        url.query_pairs_mut()
            .append_pair("path", &cp.object_path)
            .append_pair("fields", "path,type,size,sha256");
        let response = send(self.http.authorized(Method::GET, url)?, cancel).await?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        let value = json(response, cancel).await?;
        if string(&value, "path")? != cp.object_path
            || value.get("type").and_then(|v| v.as_str()) != Some("file")
        {
            return Err(CloudError::CheckpointMismatch);
        }
        let hash = value
            .get("sha256")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        receipt(
            cp,
            cp.object_path.clone(),
            size(&value, "size")?,
            hash,
            local,
        )
        .map(Some)
    }
}

#[async_trait]
impl CloudProvider for YandexDisk {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider: ProviderKind::YandexDisk,
            staged_upload: true,
            stream_upload: true,
            remote_import: true,
            resumable_upload: false,
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
            return Err(CloudError::InvalidRequest);
        }
        let mut cp = self.checkpoint(request);
        sink.save(&cp).await?;
        let mut url = self.http.url("v1/disk/resources/upload")?;
        url.query_pairs_mut()
            .append_pair("path", &cp.object_path)
            .append_pair("overwrite", "false");
        let response = send(self.http.authorized(Method::GET, url)?, cancel).await?;
        if response.status().as_u16() == 409 {
            return Err(CloudError::AlreadyExists);
        }
        let value = json(response, cancel).await?;
        if value.get("method").and_then(|v| v.as_str()) != Some("PUT")
            || value.get("templated").and_then(|v| v.as_bool()) == Some(true)
        {
            return Err(CloudError::Protocol);
        }
        let upload = self.http.yandex_upload_location(&string(&value, "href")?)?;
        cp.session_url = Some(upload.to_string());
        cp.phase = CheckpointPhase::Uploading;
        sink.save(&cp).await?;
        let mut reader = CheckedSource::new(
            source,
            request.size,
            request.expected_sha256.clone(),
            cancel.clone(),
        );
        let empty_hash = if request.size == 0 {
            reader.read_chunk(1).await?;
            Some(reader.digest())
        } else {
            None
        };
        let (payload, outcome) = body(reader);
        // The signed upload URL is sufficient. OAuth credentials never leave cloud-api's origin.
        let response = send(
            self.http
                .client
                .put(upload)
                .header(http::header::CONTENT_LENGTH, request.size)
                .header(http::header::CONTENT_TYPE, "application/octet-stream")
                .body(payload),
            cancel,
        )
        .await;
        let response = match response {
            Ok(response) => response,
            Err(CloudError::Cancelled) => return Err(CloudError::Cancelled),
            Err(error) => {
                // Preserve a typed source/hash failure without leaking reqwest's body error text.
                if let Ok(mut result) = outcome.lock()
                    && let Some(Err(source_error)) = result.take()
                {
                    return Err(source_error);
                }
                return Err(error);
            }
        };
        if !response.status().is_success() {
            return Err(CloudError::Http(response.status().as_u16()));
        }
        let local = match empty_hash {
            Some(hash) => hash,
            None => stream_result(&outcome)?,
        };
        let result = self
            .metadata(&cp, Some(local), cancel)
            .await?
            .ok_or(CloudError::ReconciliationRequired)?;
        cp.phase = CheckpointPhase::Complete;
        cp.committed_bytes = request.size;
        cp.provider_state = serde_json::json!({"local_sha256": result.local_sha256});
        sink.save(&cp).await?;
        Ok(result)
    }

    async fn import_url(
        &self,
        request: &ImportRequest,
        sink: &dyn CheckpointSink,
        cancel: &CancellationToken,
    ) -> CloudResult<PrivateCheckpoint> {
        request.upload.validate()?;
        if request.upload.mode != DeliveryMode::RemoteImport {
            return Err(CloudError::InvalidRequest);
        }
        let source =
            Url::parse(request.source_url.expose()).map_err(|_| CloudError::InvalidRequest)?;
        if source.scheme() != "https"
            || source.host_str().is_none()
            || !source.username().is_empty()
            || source.password().is_some()
            || source.fragment().is_some()
            || source.as_str().len() > 8192
        {
            return Err(CloudError::InvalidRequest);
        }
        // Source address/DNS authorization and signed-URL disclosure consent belong to the daemon.
        let mut cp = self.checkpoint(&request.upload);
        sink.save(&cp).await?;
        let mut url = self.http.url("v1/disk/resources/upload")?;
        url.query_pairs_mut()
            .append_pair("path", &cp.object_path)
            .append_pair("url", source.as_str())
            .append_pair("overwrite", "false")
            .append_pair("disable_redirects", "true");
        let response = send(self.http.authorized(Method::POST, url)?, cancel).await?;
        if response.status().as_u16() == 409 {
            return Err(CloudError::AlreadyExists);
        }
        let value = json(response, cancel).await?;
        if value.get("method").and_then(|v| v.as_str()) != Some("GET")
            || value.get("templated").and_then(|v| v.as_bool()) == Some(true)
        {
            return Err(CloudError::Protocol);
        }
        cp.session_url = Some(string(&value, "href")?);
        self.operation_url(&cp)?;
        cp.phase = CheckpointPhase::ImportPending;
        sink.save(&cp).await?;
        Ok(cp)
    }

    async fn poll_import(
        &self,
        cp: &PrivateCheckpoint,
        cancel: &CancellationToken,
    ) -> CloudResult<ImportStatus> {
        let url = self.operation_url(cp)?;
        let value = json(
            send(self.http.authorized(Method::GET, url)?, cancel).await?,
            cancel,
        )
        .await?;
        match value.get("status").and_then(|v| v.as_str()) {
            Some("in-progress") => Ok(ImportStatus::Pending),
            Some("failed") => Ok(ImportStatus::Failed),
            Some("success") => Ok(ImportStatus::Complete {
                artifact: self
                    .metadata(cp, None, cancel)
                    .await?
                    .ok_or(CloudError::ReconciliationRequired)?,
            }),
            _ => Err(CloudError::Protocol),
        }
    }

    async fn reconcile(
        &self,
        cp: &PrivateCheckpoint,
        cancel: &CancellationToken,
    ) -> CloudResult<Option<RemoteArtifact>> {
        self.metadata(cp, None, cancel).await
    }

    async fn cleanup(&self, cp: &PrivateCheckpoint, cancel: &CancellationToken) -> CloudResult<()> {
        self.check(cp)?;
        if cp.phase == CheckpointPhase::Prepared {
            return Err(CloudError::ReconciliationRequired);
        }
        // Deleting the target while an import still runs does not stop it recreating the target.
        if cp.request.mode == DeliveryMode::RemoteImport {
            let value = json(
                send(
                    self.http.authorized(Method::GET, self.operation_url(cp)?)?,
                    cancel,
                )
                .await?,
                cancel,
            )
            .await?;
            match value.get("status").and_then(|v| v.as_str()) {
                Some("in-progress") => {
                    return Err(CloudError::Unsupported(
                        CapabilityReason::RemoteCancellationUnavailable,
                    ));
                }
                Some("success" | "failed") => {}
                _ => return Err(CloudError::Protocol),
            }
        }
        // A unique destination alone does not prove that a separate writer has not replaced it.
        let expected = cp
            .provider_state
            .get("local_sha256")
            .and_then(|value| value.as_str())
            .or(cp.request.expected_sha256.as_deref())
            .ok_or(CloudError::ReconciliationRequired)?;
        let Some(current) = self
            .metadata(cp, None, cancel)
            .await
            .map_err(|error| match error {
                CloudError::HashMismatch
                | CloudError::SizeMismatch
                | CloudError::Unsupported(CapabilityReason::RemoteHashUnavailable) => {
                    CloudError::ReconciliationRequired
                }
                error => error,
            })?
        else {
            return Ok(());
        };
        if !current
            .remote_sha256
            .as_ref()
            .is_some_and(|hash| hash.eq_ignore_ascii_case(expected))
        {
            return Err(CloudError::ReconciliationRequired);
        }
        let mut url = self.http.url("v1/disk/resources")?;
        url.query_pairs_mut()
            .append_pair("path", &cp.object_path)
            .append_pair("permanently", "true");
        let response = send(self.http.authorized(Method::DELETE, url)?, cancel).await?;
        match response.status().as_u16() {
            204 | 404 => Ok(()),
            // Asynchronous delete is not reported complete without polling its operation.
            202 => {
                let value = json(response, cancel).await?;
                let raw = string(&value, "href")?;
                let url = self.operation_location(&raw)?;
                let value = json(
                    send(self.http.authorized(Method::GET, url)?, cancel).await?,
                    cancel,
                )
                .await?;
                if value.get("status").and_then(|v| v.as_str()) == Some("success") {
                    Ok(())
                } else {
                    Err(CloudError::ReconciliationRequired)
                }
            }
            status => Err(CloudError::Http(status)),
        }
    }
}

#[cfg(test)]
impl YandexDisk {
    pub(crate) fn test_endpoint(endpoint: &str) -> Self {
        Self {
            http: ProviderHttp::new(
                endpoint,
                SecretString::new("ZEUS-CANARY-token".into()),
                true,
            )
            .expect("test endpoint"),
            root_path: "disk:/Zeus".into(),
        }
    }
}
