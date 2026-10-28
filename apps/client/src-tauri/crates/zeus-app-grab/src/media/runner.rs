//! Explicitly pinned local helper execution with bounded pipe output and
//! process-tree cancellation. This is not an OS filesystem/network sandbox.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use zeus_app::failure::{Failure, Result};
use zeus_fs::{DataFile, Filesystem, RealFs};

use super::http::{canceled, cancelled};
use super::transfer::Receipt;

/// Operator-owned helper configuration. No native route writes or widens it.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pin {
    pub version: u32,
    pub enabled: bool,
    pub executable: PathBuf,
    pub sha256: String,
}

fn error(code: &str, message: &str) -> Failure {
    Failure::new(code, message)
}

impl Pin {
    /// Read only the operator's explicit media-helper configuration, never PATH
    /// discovery or another application's settings.
    pub fn load(app_directory: &Path) -> Result<Self> {
        let path = app_directory.join("media-helper.json");
        zeus_api::transport::secure_private_file(&path).map_err(|_| {
            error(
                "media.helper_unconfigured",
                "The operator-owned helper configuration is missing or could not be protected.",
            )
        })?;
        let file = RealFs.open_read(&path).map_err(|_| {
            error(
                "media.helper_unconfigured",
                "Media muxing requires an explicitly configured, hash-pinned local FFmpeg helper.",
            )
        })?;
        let length = file.len().map_err(|_| {
            error(
                "media.helper_config",
                "The helper configuration could not be read.",
            )
        })?;
        if length > 16 * 1024 {
            return Err(error(
                "media.helper_config",
                "The helper configuration exceeds its size limit.",
            ));
        }
        let mut bytes = vec![0; length as usize];
        file.read_exact_at(0, &mut bytes).map_err(|_| {
            error(
                "media.helper_config",
                "The helper configuration could not be read.",
            )
        })?;
        let pin: Self = serde_json::from_slice(&bytes).map_err(|_| {
            error(
                "media.helper_config",
                "The helper configuration is invalid.",
            )
        })?;
        if pin.version != 1
            || !pin.enabled
            || !pin.executable.is_absolute()
            || pin.sha256.len() != 64
            || !pin.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(error(
                "media.helper_unconfigured",
                "Enable a version-one helper configuration with an absolute executable and its trusted SHA-256.",
            ));
        }
        #[cfg(windows)]
        if pin
            .executable
            .extension()
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("exe"))
            || pin.executable.to_string_lossy().starts_with("\\\\")
        {
            return Err(error(
                "media.helper_config",
                "The pinned Windows helper must be a local executable, not a script or network path.",
            ));
        }
        Ok(pin)
    }

    /// Verify the executable before each launch. Keep the open handle for the
    /// lifetime of the helper; on Windows its share mode denies replacement.
    fn verify(&self) -> Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_SHARE_READ only; FILE_FLAG_OPEN_REPARSE_POINT.
            options.share_mode(1).custom_flags(0x0020_0000);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        }
        let mut file = options.open(&self.executable).map_err(|_| {
            error(
                "media.helper_unavailable",
                "The pinned FFmpeg executable is unavailable.",
            )
        })?;
        let metadata = file.metadata().map_err(|_| {
            error(
                "media.helper_unavailable",
                "The pinned FFmpeg executable could not be inspected.",
            )
        })?;
        if !metadata.is_file() || metadata.len() > 512 * 1024 * 1024 {
            return Err(error(
                "media.helper_invalid",
                "The pinned helper is not a bounded regular executable.",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(error(
                    "media.helper_invalid",
                    "A reparse-point helper is not accepted.",
                ));
            }
        }
        let mut digest = Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let n = file.read(&mut buffer).map_err(|_| {
                error(
                    "media.helper_unavailable",
                    "The helper executable could not be hashed.",
                )
            })?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
        if hex::encode(digest.finalize()) != self.sha256.to_ascii_lowercase() {
            return Err(error(
                "media.helper_changed",
                "FFmpeg no longer matches its operator-approved SHA-256. Review the helper update before using it.",
            ));
        }
        Ok(file)
    }

    pub fn view(&self) -> Value {
        json!({"configured": true, "executable": self.executable, "sha256": self.sha256,
            "limits": "Pinned trusted helper; file-only FFmpeg arguments, output byte ceiling and wall-clock deadline. Process-tree controls are not an OS filesystem or network sandbox."})
    }
}

#[cfg(windows)]
fn tree(child: &tokio::process::Child) -> Result<win32job::Job> {
    let job = win32job::Job::create().map_err(|_| {
        error(
            "media.helper_supervision",
            "A helper process job could not be created.",
        )
    })?;
    let mut limits = job.query_extended_limit_info().map_err(|_| {
        error(
            "media.helper_supervision",
            "The helper job limits could not be read.",
        )
    })?;
    limits.limit_kill_on_job_close();
    job.set_extended_limit_info(&limits).map_err(|_| {
        error(
            "media.helper_supervision",
            "The helper job could not be configured.",
        )
    })?;
    job.assign_process(child.raw_handle().ok_or_else(|| {
        error(
            "media.helper_supervision",
            "The helper process has no handle.",
        )
    })? as isize)
        .map_err(|_| {
            error(
                "media.helper_supervision",
                "The helper process could not be supervised.",
            )
        })?;
    Ok(job)
}

#[cfg(unix)]
struct ProcessGroup(rustix::process::Pid);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process_group(self.0, rustix::process::Signal::KILL);
    }
}
#[cfg(unix)]
fn tree(child: &tokio::process::Child) -> Result<ProcessGroup> {
    let pid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .filter(|id| *id > 1)
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| {
            error(
                "media.helper_supervision",
                "The helper process has no valid identity.",
            )
        })?;
    Ok(ProcessGroup(pid))
}

#[cfg(not(any(unix, windows)))]
fn tree(_: &tokio::process::Child) -> Result<()> {
    Err(error(
        "media.helper_unsupported",
        "Helper process supervision is not available on this platform.",
    ))
}

fn environment(command: &mut tokio::process::Command, directory: &Path) {
    command.env_clear();
    for key in ["SystemRoot", "SYSTEMROOT", "WINDIR", "LANG", "LC_ALL"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("TEMP", directory)
        .env("TMP", directory)
        .env("TMPDIR", directory)
        .env("HOME", directory);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
}

/// Receive helper output through a bounded pipe. The helper never receives an
/// output filename: only Zeus writes, flushes, verifies and publishes it.
pub(crate) async fn run(
    pin: &Pin,
    args: &[OsString],
    directory: &Path,
    output: Arc<dyn DataFile>,
    ceiling: u64,
    timeout: Duration,
    cancel: Arc<AtomicBool>,
) -> Result<Receipt> {
    let verification = pin.clone();
    let started = std::time::Instant::now();
    let _executable = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || verification.verify()),
    )
    .await
    .map_err(|_| {
        error(
            "media.helper_timeout",
            "Helper verification reached its time limit.",
        )
    })?
    .map_err(|_| error("media.helper_unavailable", "Helper verification stopped."))??;
    let remaining = timeout.checked_sub(started.elapsed()).ok_or_else(|| {
        error(
            "media.helper_timeout",
            "Helper verification reached its time limit.",
        )
    })?;
    let mut command = tokio::process::Command::new(&pin.executable);
    command
        .args(args)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    environment(&mut command, directory);
    let mut child = command.spawn().map_err(|_| {
        error(
            "media.helper_unavailable",
            "The pinned FFmpeg helper could not start.",
        )
    })?;
    let process_tree = tree(&child)?;
    let mut stdout = child.stdout.take().ok_or_else(|| {
        error(
            "media.helper_failed",
            "The helper output pipe is unavailable.",
        )
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| {
        error(
            "media.helper_failed",
            "The helper diagnostic pipe is unavailable.",
        )
    })?;
    let work = async {
        let receive = async {
            let mut bytes = 0u64;
            let mut digest = Sha256::new();
            let mut buffer = vec![0; 64 * 1024];
            loop {
                let count = stdout
                    .read(&mut buffer)
                    .await
                    .map_err(|_| error("media.helper_failed", "The helper output pipe failed."))?;
                if count == 0 {
                    break;
                }
                if count as u64 > ceiling.saturating_sub(bytes) {
                    return Err(error(
                        "media.output_limit",
                        "The reviewed conversion output ceiling was reached.",
                    ));
                }
                digest.update(&buffer[..count]);
                let (writer, chunk, offset) = (output.clone(), buffer[..count].to_vec(), bytes);
                tokio::task::spawn_blocking(move || writer.write_all_at(offset, &chunk))
                    .await
                    .map_err(|_| error("media.filesystem", "The converter output writer stopped."))?
                    .map_err(|_| {
                        error(
                            "media.filesystem",
                            "The converter output could not be written.",
                        )
                    })?;
                bytes += count as u64;
            }
            if bytes == 0 {
                return Err(error(
                    "media.helper_empty",
                    "The media helper produced no output.",
                ));
            }
            Ok(Receipt {
                bytes,
                sha256: hex::encode(digest.finalize()),
                etag: None,
            })
        };
        let diagnostics = async {
            let mut count = 0usize;
            let mut buffer = [0; 4096];
            loop {
                let n = stderr.read(&mut buffer).await.map_err(|_| {
                    error("media.helper_failed", "The helper diagnostic pipe failed.")
                })?;
                if n == 0 {
                    return Ok(());
                }
                count += n;
                if count > 64 * 1024 {
                    return Err(error(
                        "media.helper_diagnostics_limit",
                        "The helper exceeded its bounded diagnostic output.",
                    ));
                }
            }
        };
        let (receipt, ()) = tokio::try_join!(receive, diagnostics)?;
        if !child
            .wait()
            .await
            .map_err(|_| {
                error(
                    "media.helper_failed",
                    "The helper process status is unavailable.",
                )
            })?
            .success()
        {
            return Err(error(
                "media.helper_failed",
                "FFmpeg could not copy these tracks into the selected container. No output was published.",
            ));
        }
        let writer = output.clone();
        tokio::task::spawn_blocking(move || writer.sync_all())
            .await
            .map_err(|_| error("media.filesystem", "The converter output flush stopped."))?
            .map_err(|_| {
                error(
                    "media.filesystem",
                    "The converter output could not be flushed.",
                )
            })?;
        Ok(receipt)
    };
    let result = tokio::select! {
        biased;
        _ = cancelled(cancel) => Err(canceled()),
        result = tokio::time::timeout(remaining, work) => match result {
            Ok(result) => result,
            Err(_) => Err(error("media.helper_timeout", "The media helper reached its reviewed time limit.")),
        },
    };
    drop(process_tree);
    if result.is_err() {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::Ordering;

    fn pin() -> Pin {
        let executable = std::env::current_exe().unwrap();
        let sha256 = hex::encode(Sha256::digest(std::fs::read(&executable).unwrap()));
        Pin {
            version: 1,
            enabled: true,
            executable,
            sha256,
        }
    }

    fn args() -> Vec<OsString> {
        [
            "--exact",
            "media::runner::tests::helper_child",
            "--ignored",
            "--nocapture",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    #[test]
    #[ignore = "subprocess fixture only"]
    fn helper_child() {
        let mode = std::fs::read_to_string("fixture-mode").unwrap();
        match mode.as_str() {
            "ok" => std::io::stdout().write_all(b"verified-fixture").unwrap(),
            "large" => std::io::stdout()
                .write_all(&vec![b'x'; 2 * 1024 * 1024])
                .unwrap(),
            "stderr" => {
                std::io::stderr().write_all(&vec![b'x'; 70 * 1024]).unwrap();
                std::io::stdout().write_all(b"done").unwrap();
            }
            "sleep" => std::thread::sleep(Duration::from_secs(10)),
            "late" => {
                std::fs::write("started", b"child running").unwrap();
                std::thread::sleep(Duration::from_secs(1));
                std::fs::write("late-marker", b"child survived").unwrap();
            }
            "descendant" => {
                let directory = std::env::current_dir().unwrap().join("child");
                std::fs::create_dir(&directory).unwrap();
                std::fs::write(directory.join("fixture-mode"), b"late").unwrap();
                let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(args())
                    .current_dir(directory)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                let _ = child.wait();
                std::thread::sleep(Duration::from_secs(10));
            }
            _ => panic!("unknown fixture mode"),
        }
    }

    #[tokio::test]
    async fn bounded_runner_checks_pins_caps_output_and_redacts_helper_failures() {
        for (mode, ceiling, expected) in [
            ("ok", 1024 * 1024, None),
            ("large", 1024, Some("media.output_limit")),
            (
                "stderr",
                1024 * 1024,
                Some("media.helper_diagnostics_limit"),
            ),
        ] {
            let directory = tempfile::TempDir::new().unwrap();
            std::fs::write(directory.path().join("fixture-mode"), mode).unwrap();
            let path = directory.path().join("output.part");
            let output: Arc<dyn DataFile> = Arc::from(RealFs.create_new(&path).unwrap());
            let result = run(
                &pin(),
                &args(),
                directory.path(),
                output,
                ceiling,
                Duration::from_secs(5),
                Arc::new(AtomicBool::new(false)),
            )
            .await;
            match expected {
                None => {
                    let receipt = result.unwrap();
                    let bytes = std::fs::read(&path).unwrap();
                    assert!(String::from_utf8_lossy(&bytes).contains("verified-fixture"));
                    assert_eq!(receipt.sha256, hex::encode(Sha256::digest(bytes)));
                }
                Some(code) => assert_eq!(result.unwrap_err().code, code),
            }
            assert!(std::fs::metadata(path).unwrap().len() <= ceiling);
        }
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("changed.part");
        let mut changed = pin();
        changed.sha256 = "0".repeat(64);
        let output: Arc<dyn DataFile> = Arc::from(RealFs.create_new(&path).unwrap());
        let refusal = run(
            &changed,
            &args(),
            directory.path(),
            output,
            1024,
            Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_err();
        assert_eq!(refusal.code, "media.helper_changed");
    }

    #[tokio::test]
    async fn cancellation_and_deadlines_kill_the_helper_process_tree() {
        for mode in ["sleep", "descendant"] {
            let directory = tempfile::TempDir::new().unwrap();
            std::fs::write(directory.path().join("fixture-mode"), mode).unwrap();
            let output: Arc<dyn DataFile> = Arc::from(
                RealFs
                    .create_new(&directory.path().join("output.part"))
                    .unwrap(),
            );
            let cancel = Arc::new(AtomicBool::new(false));
            let stop = cancel.clone();
            let child_started = directory.path().join("child/started");
            let marker = child_started.clone();
            let wait_for_descendant = mode == "descendant";
            tokio::spawn(async move {
                if wait_for_descendant {
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
                    while !marker.exists() && tokio::time::Instant::now() < deadline {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                stop.store(true, Ordering::Release);
            });
            let started = std::time::Instant::now();
            let refusal = run(
                &pin(),
                &args(),
                directory.path(),
                output,
                1024 * 1024,
                Duration::from_secs(5),
                cancel,
            )
            .await
            .unwrap_err();
            assert_eq!(refusal.code, "media.canceled");
            assert!(started.elapsed() < Duration::from_secs(5));
            if wait_for_descendant {
                assert!(
                    child_started.exists(),
                    "the descendant must start before cancellation is evaluated"
                );
            }
            tokio::time::sleep(Duration::from_millis(1200)).await;
            assert!(!directory.path().join("child/late-marker").exists());
        }
        let directory = tempfile::TempDir::new().unwrap();
        std::fs::write(directory.path().join("fixture-mode"), b"sleep").unwrap();
        let output: Arc<dyn DataFile> = Arc::from(
            RealFs
                .create_new(&directory.path().join("output.part"))
                .unwrap(),
        );
        let refusal = run(
            &pin(),
            &args(),
            directory.path(),
            output,
            1024,
            Duration::from_millis(300),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_err();
        assert_eq!(refusal.code, "media.helper_timeout");
    }

    #[test]
    fn helper_environment_contains_no_inherited_credentials_or_profile_locations() {
        let mut command = tokio::process::Command::new("unused-fixture");
        command
            .env("OGUN_VPS_PW", "ZEUS-CANARY-SHOULD-NOT-PASS")
            .env("OPENAI_API_KEY", "ZEUS-CANARY-SHOULD-NOT-PASS");
        environment(&mut command, Path::new("private-fixture"));
        assert!(
            !command
                .as_std()
                .get_envs()
                .any(|(key, _)| key == "OGUN_VPS_PW"
                    || key == "OPENAI_API_KEY"
                    || key == "USERPROFILE"
                    || key == "PATH")
        );
    }
}
