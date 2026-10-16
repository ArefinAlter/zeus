use crate::http_client::RESPONSE_CAP;
use crate::model::receipt;
use crate::source::{CheckedSource, IO_CHUNK};
use crate::*;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// Explicit human-configured executable and remote. Credentials remain in the rclone config.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RcloneConfig {
    /// Absolute path to the trusted rclone executable. No shell is involved.
    pub executable: PathBuf,
    /// Absolute path to the explicitly configured credential file.
    pub config_file: PathBuf,
    /// Named remote only; inline connection strings and drive-letter paths are rejected.
    pub remote: String,
    /// Existing remote folder/bucket prefix. Cannot contain traversal or URL syntax.
    pub root_path: String,
    /// SHA-256 of the configuration authorized by the human; checked before every launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_sha256: Option<String>,
    /// SHA-256 of the executable authorized by the human; checked before every launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_sha256: Option<String>,
}

impl RcloneConfig {
    /// Validate configuration before spawning a process.
    pub fn validate(&self) -> CloudResult<()> {
        if !self.executable.is_absolute()
            || !self.config_file.is_absolute()
            || self.remote.len() < 2
            || self.remote.len() > 96
            || !self
                .remote
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || self.root_path.len() > 1024
            || self.root_path.starts_with('/')
            || self.root_path.split('/').any(|p| p == ".." || p == ".")
            || self
                .root_path
                .chars()
                .any(|c| c.is_control() || matches!(c, '\\' | ':' | '?' | '#'))
        {
            return Err(CloudError::InvalidRequest);
        }
        if let Some(hash) = &self.config_sha256 {
            crate::model::validate_hash(hash)?;
        }
        if let Some(hash) = &self.executable_sha256 {
            crate::model::validate_hash(hash)?;
        }
        Ok(())
    }
}

/// rclone `rcat --size` adapter. Bytes are never replayed and provider-side URL fetching is not
/// emulated with `copyurl`. Mutations use a unique per-operation object name and refuse retries.
pub struct Rclone {
    config: RcloneConfig,
    sha256: bool,
}
impl std::fmt::Debug for Rclone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Rclone([REDACTED])")
    }
}

impl Rclone {
    /// Discover the configured remote's features using bounded output before advertising it.
    pub async fn connect(config: RcloneConfig, cancel: &CancellationToken) -> CloudResult<Self> {
        config.validate()?;
        let mut result = Self {
            config,
            sha256: false,
        };
        let remote = format!("{}:{}", result.config.remote, result.config.root_path);
        let output = result
            .execute(&["backend", "features", &remote], None, cancel)
            .await?;
        if output.status != 0 {
            return Err(CloudError::Unsupported(CapabilityReason::BackendNotReady));
        }
        let info: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|_| CloudError::Protocol)?;
        if !info.get("Features").is_some_and(|v| v.is_object())
            || info.pointer("/Features/IsLocal").and_then(|v| v.as_bool()) == Some(true)
            || info.get("Name").and_then(|v| v.as_str()) != Some(&result.config.remote)
        {
            return Err(CloudError::Unsupported(CapabilityReason::BackendNotReady));
        }
        result.sha256 =
            info.get("Hashes")
                .and_then(|v| v.as_array())
                .is_some_and(|hashes| {
                    hashes.iter().filter_map(|h| h.as_str()).any(|h| {
                        h.eq_ignore_ascii_case("sha256") || h.eq_ignore_ascii_case("sha-256")
                    })
                });
        Ok(result)
    }

    fn path(&self, request: &UploadRequest) -> String {
        let root = self.config.root_path.trim_end_matches('/');
        if root.is_empty() {
            format!("{}:{}", self.config.remote, request.unique_name())
        } else {
            format!("{}:{}/{}", self.config.remote, root, request.unique_name())
        }
    }

    fn check(&self, cp: &PrivateCheckpoint) -> CloudResult<()> {
        cp.request.validate()?;
        cp.validate_binding(ProviderKind::Rclone, &cp.request, &self.path(&cp.request))
    }

    async fn stat(
        &self,
        path: &str,
        cancel: &CancellationToken,
    ) -> CloudResult<Option<serde_json::Value>> {
        let mut args = vec!["lsjson", "--stat", "--no-modtime", "--no-mimetype"];
        if self.sha256 {
            args.extend(["--hash-type", "SHA-256"]);
        }
        args.push(path);
        let output = self.execute(&args, None, cancel).await?;
        // rclone documents exit 3=directory missing and 4=file missing.
        if output.status == 3 || output.status == 4 {
            return Ok(None);
        }
        if output.status != 0 {
            return Err(CloudError::Process);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|_| CloudError::Protocol)?;
        // Bucket providers represent a missing key as an empty directory for --stat.
        if value.get("IsDir").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(None);
        }
        if value.get("IsDir").and_then(|v| v.as_bool()) != Some(false) {
            return Err(CloudError::Protocol);
        }
        Ok(Some(value))
    }

    fn artifact(
        &self,
        cp: &PrivateCheckpoint,
        value: &serde_json::Value,
        local: Option<String>,
    ) -> CloudResult<RemoteArtifact> {
        if value.get("Name").and_then(|v| v.as_str()) != Some(cp.request.unique_name().as_str()) {
            return Err(CloudError::CheckpointMismatch);
        }
        let size = value
            .get("Size")
            .and_then(|v| v.as_u64())
            .ok_or(CloudError::Protocol)?;
        let hash = value
            .get("Hashes")
            .and_then(|v| v.as_object())
            .and_then(|hashes| {
                hashes.iter().find(|(key, _)| {
                    key.eq_ignore_ascii_case("sha-256") || key.eq_ignore_ascii_case("sha256")
                })
            })
            .and_then(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        // Do not expose the backend's arbitrary ID field: some backends use bearer URLs there.
        receipt(cp, cp.object_path.clone(), size, hash, local)
    }

    async fn execute(
        &self,
        args: &[&str],
        source: Option<CheckedSource>,
        cancel: &CancellationToken,
    ) -> CloudResult<ProcessOutput> {
        if cancel.is_cancelled() {
            return Err(CloudError::Cancelled);
        }
        if let Some(hash) = &self.config.config_sha256 {
            check_file_hash(&self.config.config_file, hash, 1024 * 1024, cancel).await?;
        }
        if let Some(hash) = &self.config.executable_sha256 {
            check_file_hash(&self.config.executable, hash, 512 * 1024 * 1024, cancel).await?;
        }
        let mut command = Command::new(&self.config.executable);
        command
            .args(args)
            .arg("--config")
            .arg(&self.config.config_file)
            .args([
                "--log-level",
                "ERROR",
                "--stats",
                "0",
                "--retries",
                "1",
                "--low-level-retries",
                "1",
                "--buffer-size",
                "64K",
                "--max-buffer-memory",
                "32M",
                "--transfers",
                "1",
                "--checkers",
                "1",
                "--multi-thread-streams",
                "0",
                "--streaming-upload-cutoff",
                "64K",
                "--timeout",
                "60s",
                "--contimeout",
                "20s",
            ])
            .stdin(if source.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        // Windows process startup and platform TLS need these non-credential OS variables.
        for name in [
            "SystemRoot",
            "WINDIR",
            "TEMP",
            "TMP",
            "PATH",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = command.spawn().map_err(|_| CloudError::Process)?;
        let stdout = child.stdout.take().ok_or(CloudError::Process)?;
        let stderr = child.stderr.take().ok_or(CloudError::Process)?;
        let stdin = child.stdin.take();
        let work = async {
            let feed = async {
                let Some(mut source) = source else {
                    return Ok(None);
                };
                let mut stdin = stdin.ok_or(CloudError::Process)?;
                loop {
                    let bytes = source.read_chunk(IO_CHUNK).await?;
                    if bytes.is_empty() {
                        break;
                    }
                    stdin
                        .write_all(&bytes)
                        .await
                        .map_err(|_| CloudError::Process)?;
                }
                stdin.shutdown().await.map_err(|_| CloudError::Process)?;
                Ok(Some(source.digest()))
            };
            let wait = async { child.wait().await.map_err(|_| CloudError::Process) };
            let (stdout, _stderr, digest, status) =
                tokio::try_join!(bounded_output(stdout), bounded_output(stderr), feed, wait)?;
            Ok(ProcessOutput {
                status: status.code().unwrap_or(-1),
                stdout,
                digest,
            })
        };
        let result = tokio::select! { biased;
            _ = cancel.cancelled() => Err(CloudError::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(24 * 60 * 60), work) => result.unwrap_or(Err(CloudError::Process)),
        };
        if result.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        result
    }
}

struct ProcessOutput {
    status: i32,
    stdout: Vec<u8>,
    digest: Option<String>,
}

async fn check_file_hash(
    path: &std::path::Path,
    expected: &str,
    cap: u64,
    cancel: &CancellationToken,
) -> CloudResult<()> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| CloudError::InvalidRequest)?;
    if !metadata.is_file() || metadata.len() > cap {
        return Err(CloudError::InvalidRequest);
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| CloudError::InvalidRequest)?;
    let mut hash = Sha256::new();
    let mut bytes = zeroize::Zeroizing::new(vec![0u8; IO_CHUNK]);
    let mut count = 0u64;
    loop {
        let n = tokio::select! { biased;
            _ = cancel.cancelled() => return Err(CloudError::Cancelled),
            n = file.read(&mut bytes) => n.map_err(|_| CloudError::InvalidRequest)?,
        };
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > cap {
            return Err(CloudError::InvalidRequest);
        }
        hash.update(&bytes[..n]);
    }
    if !hex::encode(hash.finalize()).eq_ignore_ascii_case(expected) {
        return Err(CloudError::CheckpointMismatch);
    }
    Ok(())
}

async fn bounded_output(mut pipe: impl AsyncRead + Unpin) -> CloudResult<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let count = pipe
            .read(&mut chunk)
            .await
            .map_err(|_| CloudError::Process)?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(count) > RESPONSE_CAP {
            return Err(CloudError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

#[async_trait]
impl CloudProvider for Rclone {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            provider: ProviderKind::Rclone,
            staged_upload: true,
            stream_upload: true,
            remote_import: false,
            resumable_upload: false,
            remote_cancel: false,
            remote_import_hard_byte_limit: false,
            remote_sha256: self.sha256,
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
        let mut cp = PrivateCheckpoint {
            provider: ProviderKind::Rclone,
            request: request.clone(),
            object_path: self.path(request),
            object_id: None,
            session_url: None,
            committed_bytes: 0,
            phase: CheckpointPhase::Prepared,
            provider_state: serde_json::Value::Null,
        };
        // Atomic no-clobber is not available for generic rcat. An unguessable per-operation path,
        // durable write-once admission and an existing-object check avoid overwrite on replay.
        if self.stat(&cp.object_path, cancel).await?.is_some() {
            return Err(CloudError::AlreadyExists);
        }
        sink.save(&cp).await?;
        cp.phase = CheckpointPhase::Uploading;
        sink.save(&cp).await?;
        let size = request.size.to_string();
        let source = CheckedSource::new(
            source,
            request.size,
            request.expected_sha256.clone(),
            cancel.clone(),
        );
        let output = self
            .execute(
                &["rcat", "--size", &size, &cp.object_path],
                Some(source),
                cancel,
            )
            .await?;
        if output.status != 0 {
            return Err(CloudError::Process);
        }
        let local = output.digest.ok_or(CloudError::Source)?;
        let value = self
            .stat(&cp.object_path, cancel)
            .await?
            .ok_or(CloudError::ReconciliationRequired)?;
        let result = self.artifact(&cp, &value, Some(local))?;
        cp.committed_bytes = request.size;
        cp.phase = CheckpointPhase::Complete;
        cp.provider_state = serde_json::json!({"local_sha256": result.local_sha256});
        sink.save(&cp).await?;
        Ok(result)
    }

    async fn reconcile(
        &self,
        cp: &PrivateCheckpoint,
        cancel: &CancellationToken,
    ) -> CloudResult<Option<RemoteArtifact>> {
        self.check(cp)?;
        self.stat(&cp.object_path, cancel)
            .await?
            .map(|value| self.artifact(cp, &value, None))
            .transpose()
    }

    async fn cleanup(&self, cp: &PrivateCheckpoint, cancel: &CancellationToken) -> CloudResult<()> {
        self.check(cp)?;
        if cp.phase == CheckpointPhase::Prepared {
            return Err(CloudError::ReconciliationRequired);
        }
        let Some(value) = self.stat(&cp.object_path, cancel).await? else {
            return Ok(());
        };
        let expected = cp
            .provider_state
            .get("local_sha256")
            .and_then(|v| v.as_str())
            .or(cp.request.expected_sha256.as_deref())
            .ok_or(CloudError::ReconciliationRequired)?;
        let observed = self
            .artifact(cp, &value, None)
            .map_err(|_| CloudError::ReconciliationRequired)?;
        if !observed
            .remote_sha256
            .as_ref()
            .is_some_and(|hash| hash.eq_ignore_ascii_case(expected))
        {
            return Err(CloudError::ReconciliationRequired);
        }
        let output = self
            .execute(&["deletefile", &cp.object_path], None, cancel)
            .await?;
        if output.status == 0 || output.status == 4 {
            Ok(())
        } else {
            Err(CloudError::Process)
        }
    }
}
