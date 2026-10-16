//! Baidu's documented precreate → locateupload → parts → create protocol.
//! MD5 is used only for the provider's legacy block manifest, never as approval integrity.

use async_trait::async_trait;
use md5::{Digest, Md5};
use reqwest::Method;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    CapabilityReason, CheckpointPhase, CheckpointSink, CloudError, CloudProvider, CloudResult,
    PrivateCheckpoint, ProviderCapabilities, ProviderKind, RemoteArtifact, SecretString,
    UploadRequest, UploadSource,
    http_client::{ProviderHttp, json as response_json, send, size, string},
    model::receipt,
    source::CheckedSource,
};

/// The ordinary-account chunk size also works within paid accounts' larger chunk limits.
pub const BAIDU_BLOCK_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024 * 1024;
const MAX_BLOCKS: usize = 8192;

/// Native Baidu Netdisk multipart uploads inside an authorized application directory.
/// The caller supplies a complete block manifest, computed during staged verification or
/// obtained with the source for a forward-only stream. This adapter never creates a spool file.
pub struct BaiduNetdisk {
    http: ProviderHttp,
    locate: Url,
    token: SecretString,
    root: String,
}

impl BaiduNetdisk {
    /// Configure existing `/apps/<appname>` storage. Account/application quotas are enforced
    /// by Baidu; the adapter's absolute ceiling is its documented 20 GiB maximum file size.
    pub fn new(token: SecretString, root_path: String) -> CloudResult<Self> {
        Self::validate_root(&root_path)?;
        Ok(Self {
            http: ProviderHttp::new("https://pan.baidu.com/", token.clone(), false)?,
            locate: Url::parse("https://d.pcs.baidu.com/")
                .map_err(|_| CloudError::InvalidRequest)?,
            token,
            root: root_path.trim_end_matches('/').to_owned(),
        })
    }

    /// Check application confinement without contacting the provider.
    pub fn validate_root(root: &str) -> CloudResult<()> {
        if root.len() > 512
            || !root.starts_with("/apps/")
            || root
                .chars()
                .any(|c| c.is_control() || matches!(c, '\\' | '?' | '#' | ':'))
            || root
                .trim_end_matches('/')
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || part == "." || part == "..")
            || root.trim_end_matches('/').split('/').count() < 3
        {
            return Err(CloudError::InvalidRequest);
        }
        Ok(())
    }

    /// Validate the mandatory manifest before any source bytes or provider mutation.
    pub fn validate_manifest(request: &UploadRequest) -> CloudResult<()> {
        request.validate()?;
        let hashes = request.block_md5.as_ref().ok_or(CloudError::Unsupported(
            CapabilityReason::KnownBlockHashesRequired,
        ))?;
        let count = request.size.div_ceil(BAIDU_BLOCK_BYTES).max(1);
        if request.size > MAX_FILE_BYTES
            || hashes.len() > MAX_BLOCKS
            || hashes.len() as u64 != count
            || hashes
                .iter()
                .any(|hash| hash.len() != 32 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(CloudError::InvalidRequest);
        }
        Ok(())
    }

    fn path(&self, request: &UploadRequest) -> String {
        format!("{}/{}", self.root, request.unique_name())
    }

    fn query(&self, method: &str) -> CloudResult<Url> {
        let mut url = self.http.url("/rest/2.0/xpan/file")?;
        url.query_pairs_mut()
            .append_pair("method", method)
            .append_pair("access_token", self.token.expose());
        Ok(url)
    }

    fn check_json(value: Value) -> CloudResult<Value> {
        let code = value.get("errno").or_else(|| value.get("error_code"));
        if let Some(code) = code {
            let code = code.as_i64().ok_or(CloudError::Protocol)?;
            if code != 0 {
                return Err(if matches!(code, -8 | 31061) {
                    CloudError::AlreadyExists
                } else {
                    CloudError::Provider(code)
                });
            }
        }
        Ok(value)
    }

    async fn form(
        &self,
        method: &str,
        fields: Vec<(&str, String)>,
        cancel: &CancellationToken,
    ) -> CloudResult<Value> {
        let response = send(
            self.http.client.post(self.query(method)?).form(&fields),
            cancel,
        )
        .await?;
        Self::check_json(response_json(response, cancel).await?)
    }

    async fn metadata(&self, path: &str, cancel: &CancellationToken) -> CloudResult<Option<Value>> {
        let mut url = self.query("meta")?;
        url.query_pairs_mut().append_pair("path", path);
        let value = response_json(send(self.http.client.get(url), cancel).await?, cancel).await?;
        if value.get("errno").and_then(Value::as_i64) == Some(-9) {
            return Ok(None);
        }
        let value = Self::check_json(value)?;
        let list = value
            .get("list")
            .and_then(Value::as_array)
            .ok_or(CloudError::Protocol)?;
        if list.len() != 1 {
            return Err(CloudError::Protocol);
        }
        let item = Self::check_json(list[0].clone())?;
        if item.get("path").and_then(Value::as_str) != Some(path)
            || item.get("isdir").and_then(Value::as_u64) != Some(0)
        {
            return Err(CloudError::Protocol);
        }
        Ok(Some(item))
    }

    fn upload_host(&self, raw: &str) -> CloudResult<Url> {
        let url = Url::parse(raw).map_err(|_| CloudError::UntrustedLocation)?;
        let allowed = url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url
                .host_str()
                .is_some_and(|h| h.ends_with(".pcs.baidu.com") || h.ends_with(".baidupcs.com"));
        #[cfg(test)]
        let allowed = allowed || url.origin() == self.http.api.origin();
        if !allowed
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(CloudError::UntrustedLocation);
        }
        Ok(url)
    }

    async fn locate_upload(
        &self,
        path: &str,
        upload_id: &str,
        cancel: &CancellationToken,
    ) -> CloudResult<Url> {
        let mut url = self
            .locate
            .join("/rest/2.0/pcs/file")
            .map_err(|_| CloudError::InvalidRequest)?;
        url.query_pairs_mut()
            .append_pair("method", "locateupload")
            .append_pair("appid", "250528")
            .append_pair("access_token", self.token.expose())
            .append_pair("path", path)
            .append_pair("uploadid", upload_id)
            .append_pair("upload_version", "2.0");
        let value = Self::check_json(
            response_json(send(self.http.client.get(url), cancel).await?, cancel).await?,
        )?;
        let servers = value
            .get("servers")
            .and_then(Value::as_array)
            .ok_or(CloudError::Protocol)?;
        if servers.len() > 32 {
            return Err(CloudError::Protocol);
        }
        for server in servers {
            if let Some(raw) = server.get("server").and_then(Value::as_str)
                && let Ok(url) = self.upload_host(raw)
            {
                return Ok(url);
            }
        }
        Err(CloudError::UntrustedLocation)
    }

    fn artifact(
        &self,
        cp: &PrivateCheckpoint,
        value: &Value,
        local: Option<String>,
    ) -> CloudResult<RemoteArtifact> {
        if value.get("path").and_then(Value::as_str) != Some(cp.object_path.as_str())
            || value.get("isdir").and_then(Value::as_u64).unwrap_or(0) != 0
        {
            return Err(CloudError::Protocol);
        }
        let id = size(value, "fs_id")?.to_string();
        if cp
            .object_id
            .as_ref()
            .is_some_and(|expected| expected != &id)
        {
            return Err(CloudError::CheckpointMismatch);
        }
        // Baidu's aggregate cloud MD5 can differ from the content MD5. Do not relabel it SHA-256.
        receipt(cp, id, size(value, "size")?, None, local)
    }
}

#[async_trait]
impl CloudProvider for BaiduNetdisk {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider: ProviderKind::BaiduNetdisk,
            staged_upload: true,
            stream_upload: true,
            remote_import: false,
            resumable_upload: true,
            remote_cancel: false,
            remote_sha256: false,
            remote_import_hard_byte_limit: false,
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
        Self::validate_manifest(request)?;
        self.capabilities().plan(
            request.mode,
            Some(request.size),
            request.expected_sha256.as_deref(),
            checkpoint.is_some(),
        )?;
        let path = self.path(request);
        let hashes = request
            .block_md5
            .as_ref()
            .ok_or(CloudError::InvalidRequest)?;
        let mut cp = if let Some(cp) = checkpoint {
            cp.validate_binding(ProviderKind::BaiduNetdisk, request, &path)?;
            if let Some(artifact) = self.reconcile(&cp, cancel).await? {
                return Ok(artifact);
            }
            if cp.provider_state.get("finalizing").and_then(Value::as_bool) == Some(true) {
                return Err(CloudError::ReconciliationRequired);
            }
            cp
        } else {
            if self.metadata(&path, cancel).await?.is_some() {
                return Err(CloudError::AlreadyExists);
            }
            let cp = PrivateCheckpoint {
                provider: ProviderKind::BaiduNetdisk,
                request: request.clone(),
                object_path: path.clone(),
                object_id: None,
                session_url: None,
                committed_bytes: 0,
                phase: CheckpointPhase::Prepared,
                provider_state: json!({}),
            };
            sink.save(&cp).await?;
            cp
        };
        let mut fields = vec![
            ("path", path.clone()),
            ("size", request.size.to_string()),
            ("isdir", "0".into()),
            ("autoinit", "1".into()),
            (
                "block_list",
                serde_json::to_string(hashes).map_err(|_| CloudError::InvalidRequest)?,
            ),
        ];
        if let Some(upload_id) = cp.provider_state.get("upload_id").and_then(Value::as_str) {
            fields.push(("uploadid", upload_id.to_owned()));
        }
        sink.save(&cp).await?;
        let prepared = self.form("precreate", fields, cancel).await?;
        if prepared.get("path").and_then(Value::as_str) != Some(path.as_str()) {
            return Err(CloudError::Protocol);
        }
        let upload_id = string(&prepared, "uploadid")?;
        if upload_id.len() > 512 || upload_id.chars().any(char::is_control) {
            return Err(CloudError::Protocol);
        }
        let missing = prepared
            .get("block_list")
            .and_then(Value::as_array)
            .ok_or(CloudError::Protocol)?;
        let mut needed = vec![false; hashes.len()];
        if missing.len() > hashes.len() {
            return Err(CloudError::Protocol);
        }
        for item in missing {
            let index = item
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .ok_or(CloudError::Protocol)?;
            let slot = needed.get_mut(index).ok_or(CloudError::Protocol)?;
            if *slot {
                return Err(CloudError::Protocol);
            }
            *slot = true;
        }
        cp.phase = CheckpointPhase::Uploading;
        cp.provider_state = json!({"upload_id": upload_id});
        sink.save(&cp).await?;
        let host = self.locate_upload(&path, &upload_id, cancel).await?;
        let mut reader = CheckedSource::new(
            source,
            request.size,
            request.expected_sha256.clone(),
            cancel.clone(),
        );
        for (index, expected) in hashes.iter().enumerate() {
            let chunk = reader.read_chunk(BAIDU_BLOCK_BYTES as usize).await?;
            if !hex::encode(Md5::digest(&chunk)).eq_ignore_ascii_case(expected) {
                return Err(CloudError::HashMismatch);
            }
            if needed[index] {
                sink.save(&cp).await?;
                let mut url = host
                    .join("/rest/2.0/pcs/superfile2")
                    .map_err(|_| CloudError::UntrustedLocation)?;
                url.query_pairs_mut()
                    .append_pair("method", "upload")
                    .append_pair("type", "tmpfile")
                    .append_pair("access_token", self.token.expose())
                    .append_pair("path", &path)
                    .append_pair("uploadid", &upload_id)
                    .append_pair("partseq", &index.to_string());
                let boundary = format!("zeus-{}-{index}", request.operation_id);
                let prefix = format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"chunk\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                );
                let suffix = format!("\r\n--{boundary}--\r\n");
                if chunk
                    .windows(boundary.len())
                    .any(|v| v == boundary.as_bytes())
                {
                    return Err(CloudError::InvalidRequest);
                }
                let mut body = Vec::with_capacity(prefix.len() + chunk.len() + suffix.len());
                body.extend_from_slice(prefix.as_bytes());
                body.extend_from_slice(&chunk);
                body.extend_from_slice(suffix.as_bytes());
                let value = Self::check_json(
                    response_json(
                        send(
                            self.http
                                .client
                                .request(Method::POST, url)
                                .header(
                                    http::header::CONTENT_TYPE,
                                    format!("multipart/form-data; boundary={boundary}"),
                                )
                                .body(body),
                            cancel,
                        )
                        .await?,
                        cancel,
                    )
                    .await?,
                )?;
                if !string(&value, "md5")?.eq_ignore_ascii_case(expected) {
                    return Err(CloudError::HashMismatch);
                }
            }
            cp.committed_bytes = ((index as u64 + 1) * BAIDU_BLOCK_BYTES).min(request.size);
            sink.save(&cp).await?;
        }
        let local_hash = reader.digest();
        cp.provider_state["local_sha256"] = json!(local_hash);
        cp.provider_state["finalizing"] = json!(true);
        sink.save(&cp).await?;
        let created = self
            .form(
                "create",
                vec![
                    ("path", path),
                    ("size", request.size.to_string()),
                    ("isdir", "0".into()),
                    ("rtype", "0".into()),
                    ("uploadid", upload_id),
                    (
                        "block_list",
                        serde_json::to_string(hashes).map_err(|_| CloudError::InvalidRequest)?,
                    ),
                ],
                cancel,
            )
            .await?;
        let artifact = self.artifact(&cp, &created, Some(local_hash))?;
        cp.object_id = Some(artifact.object_id.clone());
        cp.phase = CheckpointPhase::Complete;
        sink.save(&cp).await?;
        Ok(artifact)
    }

    async fn reconcile(
        &self,
        cp: &PrivateCheckpoint,
        cancel: &CancellationToken,
    ) -> CloudResult<Option<RemoteArtifact>> {
        Self::validate_manifest(&cp.request)?;
        cp.validate_binding(
            ProviderKind::BaiduNetdisk,
            &cp.request,
            &self.path(&cp.request),
        )?;
        let Some(value) = self.metadata(&cp.object_path, cancel).await? else {
            return Ok(None);
        };
        // A file appearing before this operation finalized is a collision, not proof of our upload.
        if cp.phase != CheckpointPhase::Complete
            && cp.provider_state.get("finalizing").and_then(Value::as_bool) != Some(true)
        {
            return Err(CloudError::AlreadyExists);
        }
        let local = cp
            .provider_state
            .get("local_sha256")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.artifact(cp, &value, local).map(Some)
    }

    async fn cleanup(&self, cp: &PrivateCheckpoint, cancel: &CancellationToken) -> CloudResult<()> {
        cp.validate_binding(
            ProviderKind::BaiduNetdisk,
            &cp.request,
            &self.path(&cp.request),
        )?;
        let Some(value) = self.metadata(&cp.object_path, cancel).await? else {
            return Ok(());
        };
        let expected_id = cp
            .object_id
            .as_ref()
            .ok_or(CloudError::ReconciliationRequired)?;
        if size(&value, "fs_id")?.to_string() != *expected_id {
            return Err(CloudError::CheckpointMismatch);
        }
        let mut url = self.query("filemanager")?;
        url.query_pairs_mut().append_pair("opera", "delete");
        let value = Self::check_json(
            response_json(
                send(
                    self.http.client.post(url).form(&[
                        ("async", "0".to_owned()),
                        (
                            "filelist",
                            serde_json::to_string(&[&cp.object_path])
                                .map_err(|_| CloudError::InvalidRequest)?,
                        ),
                    ]),
                    cancel,
                )
                .await?,
                cancel,
            )
            .await?,
        )?;
        if value.get("taskid").is_some() {
            return Err(CloudError::ReconciliationRequired);
        }
        let info = value
            .get("info")
            .and_then(Value::as_array)
            .ok_or(CloudError::Protocol)?;
        if info.len() != 1
            || info[0].get("path").and_then(Value::as_str) != Some(cp.object_path.as_str())
        {
            return Err(CloudError::Protocol);
        }
        Self::check_json(info[0].clone())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
