//! Open a downloaded file or show it in its folder, from the person's click.
//!
//! Zeus never opens or runs a file it quarantined or one that can run code: those can only be shown in their folder.
//! Paths must lie inside a download folder (or the app's own folders for logs).

use std::path::Path;

use crate::engine_config::EngineSettings;
use crate::failure::{Failure, Result};
use crate::home::AppHome;
use crate::review::inside;

/// Extensions that run code or install software; they are shown in the folder, never opened.
const RUNNABLE: &[&str] = &[
    "exe", "msi", "bat", "cmd", "com", "scr", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf",
    "wsh", "jar", "lnk", "reg", "dll", "appx", "msix", "msp", "cpl", "hta", "sh", "run",
    "appimage", "deb", "rpm", "desktop", "pkg", "dmg", "app", "apk", "bin",
];

/// True when opening a file named `name` could run code.
pub fn runnable(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RUNNABLE.contains(&e.to_ascii_lowercase().as_str()))
}

/// What the person asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open with the default program (folders open in the file manager).
    Open,
    /// Show the file selected in its folder.
    Reveal,
}

/// Check the path and launch the file manager or the default program.
pub fn open(home: &AppHome, settings: &EngineSettings, path: &str, action: Action) -> Result<()> {
    let p = Path::new(path);
    if !p.is_absolute() || path.contains("..") {
        return Err(Failure::invalid("That is not a path Zeus can open."));
    }
    let allowed = settings.write_roots.iter().any(|r| inside(r, path))
        || inside(&home.logs_dir().display().to_string(), path)
        || inside(&home.app_dir().display().to_string(), path);
    if !allowed {
        return Err(Failure::new(
            "app.path_outside",
            "Zeus opens only files inside its download folders.",
        ));
    }
    if !p.exists() {
        return Err(Failure::new(
            "app.not_found",
            "That file or folder no longer exists.",
        ));
    }
    if action == Action::Open && p.is_file() && runnable(path) {
        return Err(Failure::new(
            "app.runnable",
            "Zeus does not open programs and scripts. Use Show in folder, and decide there.",
        ));
    }
    launch(p, action)
}

#[cfg(windows)]
fn launch(p: &Path, action: Action) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("explorer.exe");
    match action {
        Action::Reveal if p.is_file() => {
            cmd.raw_arg(format!("/select,\"{}\"", p.display()));
        }
        _ => {
            cmd.raw_arg(format!("\"{}\"", p.display()));
        }
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|_| Failure::new("app.open", "The file manager could not be started."))
}

#[cfg(not(windows))]
fn launch(p: &Path, action: Action) -> Result<()> {
    let target = match action {
        Action::Reveal if p.is_file() => p.parent().unwrap_or(p),
        _ => p,
    };
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|_| Failure::new("app.open", "No file manager could be started."))
}

/// Put the computer to sleep (a queue's "when finished" choice).
pub fn sleep() -> Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.args(["powrprof.dll,SetSuspendState", "0,1,0"]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new(if cfg!(target_os = "macos") {
            "pmset"
        } else {
            "systemctl"
        });
        c.arg(if cfg!(target_os = "macos") {
            "sleepnow"
        } else {
            "suspend"
        });
        c
    };
    cmd.spawn().map(|_| ()).map_err(|_| {
        Failure::new(
            "app.sleep",
            "The computer could not be put to sleep from here.",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_and_scripts_are_never_opened() {
        assert!(runnable("setup.EXE"));
        assert!(runnable("run.sh"));
        assert!(runnable("tool.AppImage"));
        assert!(!runnable("report.pdf"));
        assert!(!runnable("archive.zip"));
    }
}
