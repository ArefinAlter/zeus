//! Provisions the app's own Zeus installation and keeps its `zeusd` running.
//!
//! * First launch: `zeusd init` creates the IPC secret, the client credentials (including this app's `app.token`)
//!   and the pins of the sibling programs, all under the app's own state folder.
//! * `zeusd run` is started as a child process with the app's `ZEUS_*` environment, its output going to
//!   `logs/zeusd.log`. A daemon that is already running for this installation (left behind by a crashed app) is
//!   adopted instead of started twice.
//! * A daemon that dies is restarted with a growing delay; after five failures in two minutes the engine reports
//!   `failed` with the reason and waits for the person to press Restart.
//! * On exit the app asks the daemon to stop through the installed `zeus daemon stop` and only then ends the process.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify, watch};

use crate::daemon::Daemon;
use crate::engine_config;
use crate::failure::{Failure, Result};
use crate::home::AppHome;

/// Where the engine is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Provisioning or starting.
    Starting,
    /// Answering requests.
    Ready,
    /// Gone; coming back.
    Restarting,
    /// Could not start; see the message.
    Failed,
    /// Stopped on purpose.
    Stopped,
}

/// What the page shows about the engine.
#[derive(Debug, Clone, Serialize)]
pub struct EngineStatus {
    /// Life phase.
    pub phase: Phase,
    /// A sentence for a person.
    pub message: String,
    /// Times the engine was restarted since the app started.
    pub restarts: u32,
    /// zeusd's version once known.
    pub version: Option<String>,
    /// Process id of the daemon this app started, if it started it.
    pub pid: Option<u32>,
    /// True when a daemon that was already running was taken over.
    pub adopted: bool,
}

impl EngineStatus {
    fn new(phase: Phase, message: &str) -> Self {
        Self {
            phase,
            message: message.into(),
            restarts: 0,
            version: None,
            pid: None,
            adopted: false,
        }
    }
}

/// The names of the programs the app ships beside its own executable.
pub fn program(bin_dir: &Path, name: &str) -> PathBuf {
    bin_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// The folder holding `zeusd`, `zeus` and `zeus-native-host`: `ZEUS_BIN_DIR`, else the executable's folder.
pub fn bin_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ZEUS_BIN_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .ok_or_else(|| {
            Failure::new(
                "engine.not_installed",
                "Cannot find the folder Zeus is installed in.",
            )
        })
}

/// An AppImage mounts itself at a new path on every launch, so the programs inside it have no stable address: the
/// engine's trusted-program pins, the browser's native-messaging manifest and an AI assistant's configuration would all
/// name a path that is gone next time. For such a launch the programs are copied once to `<home>/bin` and used from
/// there. Every other install keeps its programs where the installer put them.
pub fn stage_if_ephemeral(home: &AppHome, dir: PathBuf) -> Result<PathBuf> {
    let text = dir.display().to_string();
    if std::env::var_os("APPIMAGE").is_none() && !text.contains("/.mount_") {
        return Ok(dir);
    }
    let target = home.root.join("bin");
    std::fs::create_dir_all(&target)?;
    for name in ["zeusd", "zeus", "zeus-native-host"] {
        let from = program(&dir, name);
        let to = program(&target, name);
        if !from.exists() {
            continue;
        }
        let same = std::fs::metadata(&to).is_ok_and(|t| std::fs::metadata(&from).is_ok_and(|f| f.len() == t.len()))
            && std::fs::read(&from).ok().zip(std::fs::read(&to).ok()).is_some_and(|(a, b)| a == b);
        if !same {
            let tmp = target.join(format!(".{name}.tmp"));
            std::fs::copy(&from, &tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            }
            std::fs::rename(&tmp, &to)?;
        }
    }
    Ok(target)
}

/// Supervises one `zeusd`.
pub struct Supervisor {
    home: AppHome,
    bin_dir: PathBuf,
    daemon: Arc<Daemon>,
    status: watch::Sender<EngineStatus>,
    child: Mutex<Option<Child>>,
    stopping: AtomicBool,
    restart_requested: AtomicBool,
    wake: Notify,
}

fn tool(bin_dir: &Path, name: &str, home: &AppHome) -> Command {
    let mut cmd = Command::new(program(bin_dir, name));
    cmd.envs(home.zeus_env());
    cmd.env_remove("ZEUS_LOG");
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

impl Supervisor {
    /// A supervisor for the installation under `home`, with its programs in `bin_dir`.
    pub fn new(home: AppHome, bin_dir: PathBuf, daemon: Arc<Daemon>) -> Arc<Self> {
        let (status, _) = watch::channel(EngineStatus::new(
            Phase::Starting,
            "Starting the Zeus engine",
        ));
        Arc::new(Self {
            home,
            bin_dir,
            daemon,
            status,
            child: Mutex::new(None),
            stopping: AtomicBool::new(false),
            restart_requested: AtomicBool::new(false),
            wake: Notify::new(),
        })
    }

    /// The current status.
    pub fn status(&self) -> EngineStatus {
        self.status.borrow().clone()
    }

    /// A receiver for status changes.
    pub fn watch(&self) -> watch::Receiver<EngineStatus> {
        self.status.subscribe()
    }

    /// The folder of the programs.
    pub fn bin_dir(&self) -> &Path {
        &self.bin_dir
    }

    fn set(&self, phase: Phase, message: &str) {
        self.status.send_modify(|s| {
            s.phase = phase;
            s.message = message.into();
        });
    }

    /// Start the background task that brings the engine up and keeps it up.
    pub fn start(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move { this.run().await });
    }

    async fn run(self: Arc<Self>) {
        let mut failures: Vec<Instant> = Vec::new();
        loop {
            if self.stopping.load(Ordering::SeqCst) {
                self.set(Phase::Stopped, "The Zeus engine is stopped");
                return;
            }
            match self.bring_up().await {
                Ok(()) => {
                    self.monitor().await;
                    if self.stopping.load(Ordering::SeqCst) {
                        continue;
                    }
                    if self.restart_requested.swap(false, Ordering::SeqCst) {
                        self.graceful_stop().await;
                    }
                    self.status.send_modify(|s| s.restarts += 1);
                    self.set(
                        Phase::Restarting,
                        "The Zeus engine stopped; starting it again",
                    );
                }
                Err(failure) => {
                    tracing::warn!(code = %failure.code, "the engine did not start: {}", failure.message);
                    failures.retain(|t| t.elapsed() < Duration::from_secs(120));
                    failures.push(Instant::now());
                    self.set(Phase::Failed, &failure.message);
                    if failures.len() >= 5 {
                        // Wait for the person (Restart) instead of looping forever.
                        self.wake.notified().await;
                        failures.clear();
                        continue;
                    }
                    let delay = Duration::from_secs(1 << failures.len().min(4));
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = self.wake.notified() => {}
                    }
                }
            }
        }
    }

    /// Provision if needed and make sure a daemon answers.
    async fn bring_up(&self) -> Result<()> {
        self.restart_requested.store(false, Ordering::SeqCst);
        self.home.create()?;
        let settings = engine_config::load(&self.home)?;
        if !self.home.config_dir().join("config.toml").exists() {
            engine_config::save(&self.home, &settings)?;
        } else {
            for root in &settings.write_roots {
                std::fs::create_dir_all(root)?;
            }
        }
        if self.daemon.alive().await {
            self.adopt().await;
            return Ok(());
        }
        self.daemon.reset().await;
        self.init().await?;
        self.spawn().await?;
        self.wait_ready().await
    }

    async fn adopt(&self) {
        let version = self.version().await;
        self.status.send_modify(|s| {
            s.adopted = true;
            s.version = version;
            s.pid = None;
        });
        self.set(Phase::Ready, "The Zeus engine is running");
    }

    async fn version(&self) -> Option<String> {
        self.daemon
            .raw("daemon.status", serde_json::json!({}))
            .await
            .ok()
            .and_then(|v| {
                v.pointer("/daemon/version")
                    .and_then(|x| x.as_str().map(str::to_owned))
            })
    }

    /// `zeusd init`: idempotent; creates the secret, the credentials and the program pins.
    async fn init(&self) -> Result<()> {
        let needed = !self
            .home
            .state_dir()
            .join("credentials")
            .join("app.token")
            .exists()
            || !self.home.state_dir().join("trusted-programs.json").exists();
        if !needed {
            return Ok(());
        }
        let out = tokio::time::timeout(
            Duration::from_secs(60),
            tool(&self.bin_dir, "zeusd", &self.home)
                .arg("init")
                .output(),
        )
        .await
        .map_err(|_| Failure::new("engine.init", "Setting up the Zeus engine took too long."))?
        .map_err(|e| {
            Failure::new(
                "engine.not_installed",
                format!(
                    "The Zeus engine program could not be started ({}). Reinstall Zeus.",
                    e.kind()
                ),
            )
        })?;
        if !out.status.success() {
            let text = String::from_utf8_lossy(&out.stderr);
            let tail: String = text
                .lines()
                .last()
                .unwrap_or("")
                .chars()
                .take(300)
                .collect();
            return Err(Failure::new(
                "engine.init",
                format!("Setting up the Zeus engine failed: {tail}"),
            ));
        }
        Ok(())
    }

    async fn spawn(&self) -> Result<()> {
        let log_path = self.home.logs_dir().join("zeusd.log");
        if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
            let _ = std::fs::rename(&log_path, self.home.logs_dir().join("zeusd.log.1"));
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let err = log.try_clone()?;
        let mut cmd = tool(&self.bin_dir, "zeusd", &self.home);
        cmd.arg("run")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err));
        cmd.kill_on_drop(false);
        let child = cmd.spawn().map_err(|e| {
            Failure::new(
                "engine.not_installed",
                format!(
                    "The Zeus engine program could not be started ({}). Reinstall Zeus.",
                    e.kind()
                ),
            )
        })?;
        let pid = child.id();
        *self.child.lock().await = Some(child);
        self.status.send_modify(|s| {
            s.pid = pid;
            s.adopted = false;
        });
        Ok(())
    }

    async fn wait_ready(&self) -> Result<()> {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(120) {
            if let Some(child) = self.child.lock().await.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                return Err(Failure::new(
                    "engine.exited",
                    format!(
                        "The Zeus engine stopped right after starting ({status}). See {}.",
                        self.home.logs_dir().join("zeusd.log").display()
                    ),
                ));
            }
            if self.daemon.alive().await {
                let version = self.version().await;
                self.status.send_modify(|s| s.version = version);
                self.set(Phase::Ready, "The Zeus engine is running");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Err(Failure::new(
            "engine.timeout",
            "The Zeus engine did not answer within two minutes.",
        ))
    }

    /// Returns when the daemon is gone or a restart is requested.
    async fn monitor(&self) {
        let mut misses = 0;
        loop {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(1500)) => {}
                () = self.wake.notified() => return,
            }
            if self.stopping.load(Ordering::SeqCst) {
                return;
            }
            let exited = match self.child.lock().await.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(Some(_))),
                None => false,
            };
            if exited {
                *self.child.lock().await = None;
                self.daemon.reset().await;
                return;
            }
            if self.daemon.alive().await {
                misses = 0;
            } else {
                misses += 1;
                if misses >= 3 {
                    self.daemon.reset().await;
                    self.kill().await;
                    return;
                }
            }
        }
    }

    async fn kill(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
        }
    }

    /// Ask the daemon to stop through the installed CLI, then make sure the process is gone.
    async fn graceful_stop(&self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(12),
            tool(&self.bin_dir, "zeus", &self.home)
                .args(["daemon", "stop"])
                .output(),
        )
        .await;
        let started = Instant::now();
        loop {
            let gone = match self.child.lock().await.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(Some(_))),
                None => !self.daemon.alive().await,
            };
            if gone || started.elapsed() > Duration::from_secs(12) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.kill().await;
        self.daemon.reset().await;
    }

    /// Restart the engine (after its settings changed, or from the Restart button).
    pub fn restart(&self) {
        self.set(Phase::Restarting, "Restarting the Zeus engine");
        self.restart_requested.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Stop the engine for good (the app is closing).
    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.wake.notify_waiters();
        self.graceful_stop().await;
        self.set(Phase::Stopped, "The Zeus engine is stopped");
    }
}
