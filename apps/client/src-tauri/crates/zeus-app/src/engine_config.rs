//! The settings this app owns in the engine's `config.toml`.
//!
//! zeusd reads its configuration at start. The app is the installer of its own engine, so on the person's click it
//! writes the file (from `app/engine.json`, the source of truth) and restarts the engine. `config.set` stays a
//! human-only API method that this app's credential does not hold.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::failure::{Failure, Result};
use crate::home::AppHome;

/// The engine settings the client's Settings pages edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineSettings {
    /// Folders downloads may be written to; the first is the default folder.
    pub write_roots: Vec<String>,
    /// Files at the same time, 1 to 64.
    pub concurrent_jobs: u32,
    /// Connections per file, 1 to 32.
    pub connections_per_job: u32,
    /// Connections per host, 1 to 64.
    pub connections_per_host: u32,
    /// Connect timeout in seconds, 1 to 120.
    pub connect_s: u32,
    /// Idle read timeout in seconds, 5 to 300.
    pub read_s: u32,
    /// Probe deadline in seconds, 5 to 300.
    pub probe_s: u32,
    /// Days to keep records of finished downloads, 1 to 365.
    pub tombstone_days: u32,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            write_roots: Vec::new(),
            concurrent_jobs: 4,
            connections_per_job: 8,
            connections_per_host: 8,
            connect_s: 15,
            read_s: 30,
            probe_s: 45,
            tombstone_days: 30,
        }
    }
}

#[derive(Serialize)]
struct Toml<'a> {
    write_roots: &'a [String],
    tombstone_days: u32,
    scheduler: Scheduler,
    timeouts: Timeouts,
}

#[derive(Serialize)]
struct Scheduler {
    concurrent_jobs: u32,
    connections_per_job: u32,
    connections_per_host: u32,
}

#[derive(Serialize)]
struct Timeouts {
    connect_s: u32,
    read_s: u32,
    probe_s: u32,
}

impl EngineSettings {
    /// Check the bounds zeusd enforces, so a bad value is refused here with a sentence instead of at engine start.
    pub fn validate(&self) -> Result<()> {
        let range = |name: &str, v: u32, lo: u32, hi: u32| {
            if (lo..=hi).contains(&v) {
                Ok(())
            } else {
                Err(Failure::invalid(format!(
                    "{name} must be between {lo} and {hi}."
                )))
            }
        };
        range("Files at the same time", self.concurrent_jobs, 1, 64)?;
        range("Connections per file", self.connections_per_job, 1, 32)?;
        range("Connections per host", self.connections_per_host, 1, 64)?;
        range("Connect timeout", self.connect_s, 1, 120)?;
        range("Read timeout", self.read_s, 5, 300)?;
        range("Probe deadline", self.probe_s, 5, 300)?;
        range("Record retention", self.tombstone_days, 1, 365)?;
        if self.write_roots.is_empty() || self.write_roots.len() > 16 {
            return Err(Failure::invalid("Keep between 1 and 16 download folders."));
        }
        for root in &self.write_roots {
            if !Path::new(root).is_absolute() {
                return Err(Failure::invalid(format!(
                    "{root} is not an absolute folder path."
                )));
            }
        }
        Ok(())
    }

    /// The `config.toml` text.
    pub fn to_toml(&self) -> Result<String> {
        let body = Toml {
            write_roots: &self.write_roots,
            tombstone_days: self.tombstone_days,
            scheduler: Scheduler {
                concurrent_jobs: self.concurrent_jobs,
                connections_per_job: self.connections_per_job,
                connections_per_host: self.connections_per_host,
            },
            timeouts: Timeouts {
                connect_s: self.connect_s,
                read_s: self.read_s,
                probe_s: self.probe_s,
            },
        };
        let text = toml::to_string(&body)
            .map_err(|_| Failure::new("app.config", "The engine settings could not be written."))?;
        Ok(format!(
            "# Written by the Zeus app from app/engine.json. Change settings in the app (Settings), not here.\n{text}"
        ))
    }
}

fn store_path(home: &AppHome) -> PathBuf {
    home.app_dir().join("engine.json")
}

/// Load the settings, creating the defaults (a download folder the app can reach) on first run.
pub fn load(home: &AppHome) -> Result<EngineSettings> {
    let mut settings: EngineSettings = match std::fs::read(store_path(home)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => EngineSettings::default(),
    };
    if settings.write_roots.is_empty() {
        settings.write_roots = vec![home.default_download_root().display().to_string()];
    }
    Ok(settings)
}

/// Save the settings and write the engine's `config.toml`. The engine must be restarted to apply them.
pub fn save(home: &AppHome, settings: &EngineSettings) -> Result<()> {
    settings.validate()?;
    for root in &settings.write_roots {
        std::fs::create_dir_all(root)?;
    }
    std::fs::create_dir_all(home.app_dir())?;
    write_atomic(&store_path(home), &serde_json::to_vec_pretty(settings)?)?;
    std::fs::create_dir_all(home.config_dir())?;
    write_atomic(
        &home.config_dir().join("config.toml"),
        settings.to_toml()?.as_bytes(),
    )?;
    Ok(())
}

/// Write `bytes` to `path` through a temporary file in the same folder, so a crash leaves the old or the new file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent folder"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> String {
        if cfg!(windows) {
            "C:\\dl".into()
        } else {
            "/dl".into()
        }
    }

    #[test]
    fn the_generated_file_has_the_shape_zeusd_reads() {
        let s = EngineSettings {
            write_roots: vec![root()],
            ..EngineSettings::default()
        };
        s.validate().unwrap();
        let text = s.to_toml().unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["scheduler"]["concurrent_jobs"].as_integer(), Some(4));
        assert_eq!(parsed["timeouts"]["probe_s"].as_integer(), Some(45));
        assert_eq!(parsed["tombstone_days"].as_integer(), Some(30));
        assert_eq!(parsed["write_roots"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn bounds_are_refused_with_a_sentence() {
        let mut s = EngineSettings {
            write_roots: vec![root()],
            concurrent_jobs: 0,
            ..EngineSettings::default()
        };
        assert!(
            s.validate()
                .unwrap_err()
                .message
                .contains("Files at the same time")
        );
        s.concurrent_jobs = 4;
        s.write_roots = vec!["relative".into()];
        assert!(s.validate().is_err());
    }
}
