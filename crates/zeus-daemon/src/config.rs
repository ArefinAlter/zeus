//! Daemon configuration (`config.toml`).
//!
//! Precedence is flags, then environment (`ZEUS_*`), then this file, then the defaults below (ARCHITECTURE section
//! 11). Unknown keys are an error (a typo in a security-relevant setting must not be silently ignored). Bounds are
//! the ones of 01 section 10.

use std::path::{Path, PathBuf};

use directories::UserDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Failure to load or validate the configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The file is not valid TOML or has unknown or mistyped keys.
    #[error("invalid configuration: {0}")]
    Parse(#[from] toml::de::Error),
    /// A value is out of range.
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Scheduler limits (01 section 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SchedulerConfig {
    /// Jobs transferring at once, 1 to 64.
    pub concurrent_jobs: u32,
    /// Connections per job, 1 to 32.
    pub connections_per_job: u32,
    /// Connections per host across jobs, 1 to 64.
    pub connections_per_host: u32,
    /// Segment size in MiB, 8 to 32.
    pub segment_size_mib: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            concurrent_jobs: 4,
            connections_per_job: 8,
            connections_per_host: 8,
            segment_size_mib: 16,
        }
    }
}

/// Timeouts (01 section 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TimeoutConfig {
    /// Connect timeout in seconds, 1 to 120.
    pub connect_s: u32,
    /// Idle read timeout in seconds, 5 to 300.
    pub read_s: u32,
    /// Probe deadline in seconds, 5 to 300.
    pub probe_s: u32,
    /// How long `awaiting_session` waits, in milliseconds, 1 minute to 24 hours.
    pub session_wait_ms: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect_s: 15,
            read_s: 30,
            probe_s: 45,
            session_wait_ms: 3_600_000,
        }
    }
}

/// Approval handling (01 section 5.3, 00 section 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ApprovalConfig {
    /// Approvals pending at once, 1 to 50.
    pub max_pending: u32,
    /// Decisions accepted per minute, 1 to 60.
    pub decisions_per_minute: u32,
    /// Default expiry in milliseconds, 1 minute to 30 days.
    pub expiry_ms: u64,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self {
            max_pending: 50,
            decisions_per_minute: 10,
            expiry_ms: 86_400_000,
        }
    }
}

/// The daemon configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct DaemonConfig {
    /// Directories downloads may be written to. Empty: the default root (`<Downloads>/Zeus`).
    pub write_roots: Vec<PathBuf>,
    /// Scheduler limits.
    pub scheduler: SchedulerConfig,
    /// Timeouts.
    pub timeouts: TimeoutConfig,
    /// Approvals.
    pub approvals: ApprovalConfig,
    /// Days to keep tombstones of finished jobs, 1 to 365.
    pub tombstone_days: Option<u32>,
    /// Log filter in `tracing` syntax, for example `info`.
    pub log_filter: Option<String>,
}

fn check(name: &str, value: u64, low: u64, high: u64, errors: &mut Vec<String>) {
    if !(low..=high).contains(&value) {
        errors.push(format!(
            "{name} must be between {low} and {high}, not {value}"
        ));
    }
}

impl DaemonConfig {
    /// Read `path`; a missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        Self::parse(&text)
    }

    /// Parse and validate TOML text.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Check every bound.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut errors = Vec::new();
        let s = &self.scheduler;
        check(
            "scheduler.concurrent_jobs",
            s.concurrent_jobs.into(),
            1,
            64,
            &mut errors,
        );
        check(
            "scheduler.connections_per_job",
            s.connections_per_job.into(),
            1,
            32,
            &mut errors,
        );
        check(
            "scheduler.connections_per_host",
            s.connections_per_host.into(),
            1,
            64,
            &mut errors,
        );
        check(
            "scheduler.segment_size_mib",
            s.segment_size_mib.into(),
            8,
            32,
            &mut errors,
        );
        let t = &self.timeouts;
        check(
            "timeouts.connect_s",
            t.connect_s.into(),
            1,
            120,
            &mut errors,
        );
        check("timeouts.read_s", t.read_s.into(), 5, 300, &mut errors);
        check("timeouts.probe_s", t.probe_s.into(), 5, 300, &mut errors);
        check(
            "timeouts.session_wait_ms",
            t.session_wait_ms,
            60_000,
            86_400_000,
            &mut errors,
        );
        let a = &self.approvals;
        check(
            "approvals.max_pending",
            a.max_pending.into(),
            1,
            50,
            &mut errors,
        );
        check(
            "approvals.decisions_per_minute",
            a.decisions_per_minute.into(),
            1,
            60,
            &mut errors,
        );
        check(
            "approvals.expiry_ms",
            a.expiry_ms,
            60_000,
            30 * 86_400_000,
            &mut errors,
        );
        if let Some(days) = self.tombstone_days {
            check("tombstone_days", days.into(), 1, 365, &mut errors);
        }
        for root in &self.write_roots {
            if !root.is_absolute() {
                errors.push(format!(
                    "write_roots entries must be absolute paths, not {}",
                    root.display()
                ));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(errors.join("; ")))
        }
    }

    /// The write roots in force: the configured ones, or `<Downloads>/Zeus` (home-relative fallback).
    pub fn effective_write_roots(&self) -> Vec<PathBuf> {
        if !self.write_roots.is_empty() {
            return self.write_roots.clone();
        }
        let downloads = UserDirs::new()
            .and_then(|d| d.download_dir().map(Path::to_path_buf))
            .or_else(|| UserDirs::new().map(|d| d.home_dir().join("Downloads")));
        downloads.map(|d| vec![d.join("Zeus")]).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_match_the_spec() {
        let config = DaemonConfig::default();
        config.validate().unwrap();
        assert_eq!(config.scheduler.concurrent_jobs, 4);
        assert_eq!(config.scheduler.segment_size_mib, 16);
        assert_eq!(config.timeouts.session_wait_ms, 3_600_000);
        assert_eq!(config.approvals.max_pending, 50);
    }

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            DaemonConfig::load(&dir.path().join("config.toml")).unwrap(),
            DaemonConfig::default()
        );
    }

    #[test]
    fn toml_overrides_and_typos_are_errors() {
        let ok = DaemonConfig::parse("[scheduler]\nconcurrent_jobs = 8\n").unwrap();
        assert_eq!(ok.scheduler.concurrent_jobs, 8);
        assert_eq!(ok.scheduler.connections_per_job, 8);
        assert!(matches!(
            DaemonConfig::parse("[scheduler]\nconcurent_jobs = 8\n"),
            Err(ConfigError::Parse(_))
        ));
        assert!(matches!(
            DaemonConfig::parse("surprise = true\n"),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn bounds_are_enforced_and_reported_together() {
        let error = DaemonConfig::parse("[scheduler]\nconcurrent_jobs = 0\nsegment_size_mib = 64\n[approvals]\nmax_pending = 51\n").unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("concurrent_jobs")
                && text.contains("segment_size_mib")
                && text.contains("max_pending")
        );
    }

    #[test]
    fn write_roots_must_be_absolute() {
        let root = if cfg!(windows) { "C:\\\\dl" } else { "/dl" };
        assert!(DaemonConfig::parse(&format!("write_roots = [\"{root}\"]\n")).is_ok());
        assert!(DaemonConfig::parse("write_roots = [\"relative/dir\"]\n").is_err());
    }

    #[test]
    fn the_default_root_ends_in_zeus() {
        for root in DaemonConfig::default().effective_write_roots() {
            assert!(root.ends_with("Zeus"));
        }
    }
}
