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

/// Keys that parse and validate but do not change behaviour yet. `config.set` refuses them and startup warns when
/// the file changes them, so nobody mistakes them for working controls. The engine currently issues ranged requests
/// of one commit unit (4 MiB) instead of planning 8 to 32 MiB segments.
pub const INEFFECTIVE_KEYS: &[&str] = &["scheduler.segment_size_mib"];

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

/// Router token-bucket rates of one credential class (05 section 13.2). Burst is twice the rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RateConfig {
    /// Mutating calls per second (`jobs.enqueue` costs one per job), 1 to 1,000.
    pub mutating_per_s: u32,
    /// Other calls per second, except `jobs.wait` and the handshake, 1 to 10,000.
    pub read_per_s: u32,
    /// `jobs.probe`, `jobs.verify` and `audit.verify` per second, 1 to 1,000.
    pub probe_per_s: u32,
    /// `sessions.request` and session-bearing jobs per minute, 1 to 600.
    pub session_request_per_min: u32,
}

impl Default for RateConfig {
    fn default() -> Self {
        Self {
            mutating_per_s: 50,
            read_per_s: 200,
            probe_per_s: 10,
            session_request_per_min: 10,
        }
    }
}

impl RateConfig {
    /// The router rates.
    pub fn limits(&self) -> zeus_api::rate::RateLimits {
        use zeus_api::rate::Rate;
        zeus_api::rate::RateLimits {
            mutating: Rate::per_second(self.mutating_per_s),
            read: Rate::per_second(self.read_per_s),
            probe: Rate::per_second(self.probe_per_s),
            session_request: Rate::per_minute(self.session_request_per_min),
        }
    }
}

/// `api.rate_limits`: agent credentials share one setting; each service credential, by name, has its own.
/// Human-capable credentials keep the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct RateLimitConfig {
    /// Every agent credential.
    pub agent: RateConfig,
    /// Service credentials by name (at most 64 entries).
    pub service: std::collections::BTreeMap<String, RateConfig>,
}

impl RateLimitConfig {
    /// The router's per-caller rates: agents share `agent`, a service uses its entry (by credential name) or the
    /// defaults, human-capable callers use the defaults.
    pub fn per_caller(
        &self,
        credentials: std::sync::Arc<crate::provision::FileCredentialStore>,
    ) -> std::sync::Arc<zeus_api::rate::LimitsFor> {
        use zeus_api::{ClientKind, rate::RateLimits};
        let agent = self.agent.limits();
        let services: std::collections::BTreeMap<String, RateLimits> = self
            .service
            .iter()
            .map(|(name, rates)| (name.clone(), rates.limits()))
            .collect();
        std::sync::Arc::new(
            move |identity: &zeus_api::ClientIdentity| match identity.kind {
                ClientKind::Agent => agent,
                ClientKind::Service if !services.is_empty() => credentials
                    .record(&identity.credential_id)
                    .and_then(|record| services.get(&record.name).copied())
                    .unwrap_or_default(),
                _ => RateLimits::default(),
            },
        )
    }
}

/// API settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ApiConfig {
    /// Router rate limits.
    pub rate_limits: RateLimitConfig,
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
    /// Log filter in `tracing` syntax, for example `info`. `ZEUS_LOG` overrides it; read at startup.
    pub log_filter: Option<String>,
    /// API settings (rate limits).
    pub api: ApiConfig,
}

impl DaemonConfig {
    /// The log filter in force: `ZEUS_LOG` when set and valid, then `log_filter`, then `info`.
    pub fn effective_log_filter(&self, env: Option<&str>) -> String {
        env.filter(|v| tracing_subscriber::EnvFilter::try_new(v).is_ok())
            .or(self.log_filter.as_deref())
            .unwrap_or("info")
            .to_owned()
    }
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
        let limits = &self.api.rate_limits;
        if limits.service.len() > 64 {
            errors.push("api.rate_limits.service has more than 64 entries".into());
        }
        for (prefix, rates) in std::iter::once(("api.rate_limits.agent".to_owned(), &limits.agent))
            .chain(
                limits
                    .service
                    .iter()
                    .map(|(name, rates)| (format!("api.rate_limits.service.{name}"), rates)),
            )
        {
            check(
                &format!("{prefix}.mutating_per_s"),
                rates.mutating_per_s.into(),
                1,
                1_000,
                &mut errors,
            );
            check(
                &format!("{prefix}.read_per_s"),
                rates.read_per_s.into(),
                1,
                10_000,
                &mut errors,
            );
            check(
                &format!("{prefix}.probe_per_s"),
                rates.probe_per_s.into(),
                1,
                1_000,
                &mut errors,
            );
            check(
                &format!("{prefix}.session_request_per_min"),
                rates.session_request_per_min.into(),
                1,
                600,
                &mut errors,
            );
        }
        for name in limits.service.keys() {
            if !crate::provision::valid_name(name)
                || crate::provision::RESERVED_NAMES.contains(&name.as_str())
            {
                errors.push(format!(
                    "api.rate_limits.service keys are service credential names, not {name:?}"
                ));
            }
        }
        if let Some(filter) = &self.log_filter
            && (filter.len() > 512 || tracing_subscriber::EnvFilter::try_new(filter).is_err())
        {
            errors.push("log_filter is not a valid tracing filter".into());
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
    fn the_log_filter_is_validated_and_the_environment_wins() {
        let config = DaemonConfig::parse("log_filter = \"zeus_daemon=debug,warn\"\n").unwrap();
        assert_eq!(config.effective_log_filter(None), "zeus_daemon=debug,warn");
        assert_eq!(config.effective_log_filter(Some("trace")), "trace");
        assert_eq!(
            config.effective_log_filter(Some("[not a filter")),
            "zeus_daemon=debug,warn"
        );
        assert_eq!(DaemonConfig::default().effective_log_filter(None), "info");
        assert!(DaemonConfig::parse("log_filter = \"[unclosed\"\n").is_err());
    }

    #[test]
    fn rate_limits_are_configured_per_credential_class() {
        let config = DaemonConfig::parse(
            "[api.rate_limits.agent]
mutating_per_s = 5
[api.rate_limits.service.ogun]
read_per_s = 7
",
        )
        .unwrap();
        assert_eq!(config.api.rate_limits.agent.limits().mutating.tokens, 5);
        assert_eq!(config.api.rate_limits.agent.read_per_s, 200);
        let ogun = config.api.rate_limits.service["ogun"].limits();
        assert_eq!((ogun.read.tokens, ogun.mutating.tokens), (7, 50));
        assert_eq!(ogun.session_request.per, std::time::Duration::from_secs(60));
        for bad in [
            "[api.rate_limits.agent]
probe_per_s = 0
",
            "[api.rate_limits.service.Ogun]
read_per_s = 7
",
            "[api.rate_limits.service.cli]
read_per_s = 7
",
            "[api.rate_limits.agent]
burst = 7
",
        ] {
            assert!(DaemonConfig::parse(bad).is_err(), "{bad}");
        }
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
