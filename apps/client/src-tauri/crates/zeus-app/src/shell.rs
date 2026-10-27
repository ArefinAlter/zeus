//! What the app does around its window: start at sign-in, start minimized, the notification-area icon, closing to it,
//! system notifications while the window is hidden, and a manual check for a newer release.
//!
//! The settings live in `app/shell.json`. Applying them needs the windowing toolkit, which this crate does not know: the
//! desktop shell registers [`ShellHooks`] and this module calls them. In the test harness there are no hooks, and the
//! routes still store the settings and say that nothing was applied.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine_config::write_atomic;
use crate::failure::{Failure, Result};
use crate::home::AppHome;

/// What the person chose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellSettings {
    /// Start Zeus when the person signs in to the computer.
    pub launch_at_login: bool,
    /// Start with the window hidden (in the notification area when it is shown, otherwise minimized).
    pub start_minimized: bool,
    /// Show the icon in the notification area.
    pub show_tray: bool,
    /// Closing the window hides it to the notification area and keeps the engine running.
    pub close_to_tray: bool,
    /// Check for a newer release about once a week (never by default).
    pub check_updates_weekly: bool,
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            launch_at_login: false,
            start_minimized: false,
            show_tray: true,
            close_to_tray: true,
            check_updates_weekly: false,
        }
    }
}

/// The windowing side of the shell, registered by the desktop app.
pub trait ShellHooks: Send + Sync {
    /// Apply the settings (autostart, tray icon). Returns what could not be applied, in a sentence.
    fn apply(&self, settings: &ShellSettings) -> std::result::Result<(), String>;
    /// Show a system notification.
    fn notify(&self, title: &str, body: &str);
    /// Set the tooltip of the notification-area icon.
    fn tooltip(&self, text: &str);
    /// Whether the platform offers a notification-area icon here.
    fn tray_available(&self) -> bool;
}

/// The shell state of the app.
pub struct Shell {
    path: PathBuf,
    hooks: std::sync::RwLock<Option<Arc<dyn ShellHooks>>>,
}

fn load(path: &PathBuf) -> ShellSettings {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

impl Shell {
    /// The shell state under `home`.
    pub fn new(home: &AppHome) -> Self {
        Self {
            path: home.app_dir().join("shell.json"),
            hooks: std::sync::RwLock::new(None),
        }
    }

    /// The stored settings.
    pub fn settings(&self) -> ShellSettings {
        load(&self.path)
    }

    /// Register the windowing side and apply the stored settings once.
    pub fn set_hooks(&self, hooks: Arc<dyn ShellHooks>) {
        let settings = self.settings();
        let _ = hooks.apply(&settings);
        if let Ok(mut h) = self.hooks.write() {
            *h = Some(hooks);
        }
    }

    fn hooks(&self) -> Option<Arc<dyn ShellHooks>> {
        self.hooks.read().ok().and_then(|h| h.clone())
    }

    /// The route answer for the settings page.
    pub fn view(&self) -> Value {
        let hooks = self.hooks();
        json!({
            "settings": self.settings(),
            "applied_by_shell": hooks.is_some(),
            "tray_available": hooks.as_ref().is_some_and(|h| h.tray_available()),
            "version": env!("CARGO_PKG_VERSION"),
        })
    }

    /// Store new settings and apply them.
    pub fn put(&self, body: &Value) -> Result<Value> {
        let settings: ShellSettings = serde_json::from_value(body.clone())?;
        write_atomic(&self.path, &serde_json::to_vec_pretty(&settings)?)?;
        let note = match self.hooks() {
            Some(h) => h.apply(&settings).err(),
            None => Some("This build has no window shell; the settings are stored.".into()),
        };
        let mut v = self.view();
        v["note"] = note.map_or(Value::Null, Value::from);
        Ok(v)
    }

    /// A system notification, with its text bounded and stripped of control characters.
    pub fn notify(&self, body: &Value) -> Result<Value> {
        let clean = |v: Option<&Value>, max: usize| -> String {
            v.and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control())
                .take(max)
                .collect()
        };
        let title = clean(body.get("title"), 80);
        let text = clean(body.get("body"), 200);
        if title.is_empty() {
            return Err(Failure::invalid("A notification needs a title."));
        }
        if let Some(h) = self.hooks() {
            h.notify(&title, &text);
        }
        Ok(json!({"ok": true}))
    }

    /// The tooltip of the notification-area icon (speed and counts).
    pub fn tooltip(&self, body: &Value) -> Value {
        let text: String = body
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("Zeus")
            .chars()
            .filter(|c| !c.is_control())
            .take(120)
            .collect();
        if let Some(h) = self.hooks() {
            h.tooltip(&text);
        }
        json!({"ok": true})
    }
}

/// Parse `0.0.1b`, `v0.0.1b`, `0.0.1-beta` into comparable parts (numbers, then a stage rank).
fn release_key(text: &str) -> Option<(Vec<u64>, u8)> {
    let t = text.trim().trim_start_matches('v');
    let (nums, stage) = match t.split_once('-') {
        Some((n, s)) => (n, s.to_owned()),
        None => {
            let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
            (&t[..split], t[split..].to_owned())
        }
    };
    let parts: Option<Vec<u64>> = nums.split('.').map(|p| p.parse().ok()).collect();
    let rank = match stage.as_str() {
        "" => 9,
        "a" | "alpha" => 1,
        "b" | "beta" => 2,
        "rc" => 3,
        _ => 0,
    };
    Some((parts?, rank))
}

/// Compare two release names.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (release_key(latest), release_key(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Ask GitHub for the latest release of Zeus (a person pressed the button, or the weekly check is on). The request
/// carries no identifier but the program name and version.
pub async fn check_update() -> Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(format!("Zeus/{} (update check)", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| Failure::new("app.update", "The update check could not start."))?;
    let response = client
        .get("https://api.github.com/repos/ArefinAlter/zeus/releases/latest")
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| {
            Failure::new(
                "app.update_offline",
                "Could not reach GitHub to look for a newer release.",
            )
        })?;
    if !response.status().is_success() {
        return Err(Failure::new(
            "app.update_failed",
            format!(
                "GitHub answered {} when asked for the latest release.",
                response.status().as_u16()
            ),
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| Failure::new("app.update_failed", "GitHub's answer was not understood."))?;
    let tag = body
        .get("tag_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let current = env!("CARGO_PKG_VERSION");
    Ok(json!({
        "current": current,
        "latest": tag,
        "newer": is_newer(&tag, current),
        "url": body.get("html_url").and_then(Value::as_str).filter(|u| u.starts_with("https://github.com/ArefinAlter/zeus/")),
        "name": body.get("name"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_names_compare_across_spellings() {
        assert!(!is_newer("v0.0.1b", "0.0.1-beta"));
        assert!(is_newer("v0.0.2a", "0.0.1-beta"));
        assert!(is_newer("v0.0.1", "0.0.1-beta"));
        assert!(!is_newer("v0.0.1a", "0.0.1-beta"));
        assert!(is_newer("v0.1.0", "0.0.9"));
        assert!(!is_newer("garbage", "0.0.1-beta"));
    }

    #[test]
    fn settings_round_trip_and_default_sensibly() {
        let dir = tempfile::tempdir().unwrap();
        let shell = Shell::new(&AppHome::at(dir.path().to_path_buf()));
        assert!(shell.settings().show_tray && !shell.settings().launch_at_login);
        let v = shell.put(&json!({"launch_at_login": true, "start_minimized": true, "show_tray": true, "close_to_tray": false, "check_updates_weekly": false})).unwrap();
        assert_eq!(v["settings"]["launch_at_login"], true);
        assert!(!shell.settings().close_to_tray);
        assert!(shell.notify(&json!({"title": ""})).is_err());
    }
}
