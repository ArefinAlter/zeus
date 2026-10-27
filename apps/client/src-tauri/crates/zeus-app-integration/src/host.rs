//! Registering the native-messaging host with the person's browsers, from the app, with consent.
//!
//! What the browser starts is a small launcher script the app writes (`app/integration/host/`): it sets the `ZEUS_*`
//! variables that select THIS app's engine, then starts the bundled `zeus-native-host` beside the app, so a host that
//! the browser starts (with no environment of ours) finds the right daemon. On Windows the launcher is a `.cmd` file;
//! the host's browser-parent check already allows exactly that one hop through the system's `cmd.exe` (parent.rs).
//!
//! The registration is the one Zeus defines (spec 02 section 3.1), per user only: a registry value below HKCU on
//! Windows, a manifest in the user's own configuration folder on Linux. The host's allow-list names the extension ids
//! it will serve; it is written to the engine's private state folder, where the host looks when no file sits beside it.
//! The extension in this release is a development build with a fixed id, so its id goes under `dev_extensions` and the
//! person is told so before anything is written (spec 02 section 13.1).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use zeus_app::App;
use zeus_app::failure::{Failure, Result};
use zeus_native_host::allowlist;
use zeus_native_host::install::{
    self, Browser, InstallRequest, ManifestLocation, Os, Step, SystemRegistrar,
};

use crate::package;

/// One entry of the browser table.
struct Entry {
    browser: Browser,
    name: &'static str,
    page: &'static str,
}

const BROWSERS: [Entry; 5] = [
    Entry {
        browser: Browser::Chrome,
        name: "Google Chrome",
        page: "chrome://extensions",
    },
    Entry {
        browser: Browser::Edge,
        name: "Microsoft Edge",
        page: "edge://extensions",
    },
    Entry {
        browser: Browser::Brave,
        name: "Brave",
        page: "brave://extensions",
    },
    Entry {
        browser: Browser::Chromium,
        name: "Chromium",
        page: "chrome://extensions",
    },
    Entry {
        browser: Browser::Firefox,
        name: "Mozilla Firefox",
        page: "about:debugging#/runtime/this-firefox",
    },
];

/// The browsers named by a request (`["edge", ...]`); empty or absent means every one that is installed.
pub fn parse_browsers(body: &Value) -> Result<Vec<Browser>> {
    let Some(list) = body.get("browsers").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    list.iter()
        .map(|b| {
            b.as_str()
                .and_then(Browser::parse)
                .ok_or_else(|| Failure::invalid("Unknown browser."))
        })
        .collect()
}

fn os() -> Os {
    Os::current()
}

fn home_dir() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or_else(|| Failure::new("integration.home", "Cannot find your home folder."))
}

/// The files the app owns for the host.
pub struct Layout {
    /// Folder of the launcher.
    pub dir: PathBuf,
    /// The launcher the browser starts.
    pub launcher: PathBuf,
    /// The real host.
    pub host_exe: PathBuf,
    /// The host's log (its stderr, which the browser discards).
    pub log: PathBuf,
    /// The allow-list in the engine's private state folder.
    pub allow_list: PathBuf,
}

/// Where everything lives for `app`.
pub fn layout(app: &App) -> Layout {
    let dir = app.home.app_dir().join("integration").join("host");
    let name = if cfg!(windows) {
        "zeus-native-host.cmd"
    } else {
        "zeus-native-host.sh"
    };
    let exe = format!("zeus-native-host{}", std::env::consts::EXE_SUFFIX);
    Layout {
        launcher: dir.join(name),
        dir,
        host_exe: app.supervisor.bin_dir().join(exe),
        log: app.home.logs_dir().join("native-host.log"),
        allow_list: app.home.state_dir().join(allowlist::FILE_NAME),
    }
}

fn unsafe_for_cmd(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '%' | '!' | '"' | '\n' | '\r' | '\0'))
}

/// The text of the launcher for `os`.
pub fn launcher_text(
    os: Os,
    host_exe: &Path,
    env: &[(String, String)],
    log: &Path,
) -> Result<String> {
    let exe = host_exe.display().to_string();
    let log_path = log.display().to_string();
    if os == Os::Windows {
        let bad = |s: &str| {
            Failure::new(
                "integration.path",
                format!(
                    "A browser cannot start Zeus from {s}: the folder name has a character a launcher script cannot carry (% ! \")."
                ),
            )
        };
        for s in std::iter::once(&exe)
            .chain(std::iter::once(&log_path))
            .chain(env.iter().map(|(_, v)| v))
        {
            if unsafe_for_cmd(s) {
                return Err(bad(s));
            }
        }
        let mut text = String::from(
            "@echo off\r\nrem Written by the Zeus app. Starts the native-messaging host for this installation.\r\n",
        );
        for (k, v) in env {
            text.push_str(&format!("set \"{k}={v}\"\r\n"));
        }
        text.push_str(&format!("\"{exe}\" %* 2>>\"{log_path}\"\r\n"));
        Ok(text)
    } else {
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let mut text = String::from(
            "#!/bin/sh\n# Written by the Zeus app. Starts the native-messaging host for this installation.\n",
        );
        for (k, v) in env {
            text.push_str(&format!("export {k}={}\n", quote(v)));
        }
        text.push_str(&format!(
            "exec {} \"$@\" 2>>{}\n",
            quote(&exe),
            quote(&log_path)
        ));
        Ok(text)
    }
}

fn write_launcher(app: &App) -> Result<Layout> {
    let l = layout(app);
    if !l.host_exe.is_file() {
        return Err(Failure::new(
            "integration.host_missing",
            format!(
                "The Zeus browser host was not found at {}. Reinstall Zeus.",
                l.host_exe.display()
            ),
        ));
    }
    std::fs::create_dir_all(&l.dir)?;
    let text = launcher_text(os(), &l.host_exe, &app.home.zeus_env(), &l.log)?;
    std::fs::write(&l.launcher, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&l.launcher, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(l)
}

fn request(app: &App, browsers: Vec<Browser>, l: &Layout) -> Result<InstallRequest> {
    let chromium = package::chromium_dev_id();
    let firefox = package::firefox_id();
    let mut dev = Vec::new();
    if browsers.iter().any(|b| b.is_chromium_family()) {
        dev.extend(chromium);
    }
    if browsers.contains(&Browser::Firefox) {
        dev.extend(firefox);
    }
    Ok(InstallRequest {
        browsers,
        host_exe: l.launcher.clone(),
        extension_ids: Vec::new(),
        dev_extension_ids: dev,
        home: home_dir()?,
        state_dir: app.home.state_dir(),
        os: os(),
        allow_list_dir: Some(app.home.state_dir()),
    })
}

/// Browsers this computer has (any install we can see), in table order.
fn detected() -> Vec<(Browser, bool, Option<PathBuf>)> {
    let recognized = zeus_native_host::parent::recognized_installs();
    BROWSERS
        .iter()
        .map(|e| {
            let hit = recognized.iter().find(|r| r.name == e.browser.as_str());
            if let Some(r) = hit {
                return (e.browser, true, Some(r.image.clone()));
            }
            (e.browser, false, per_user_install(e.browser))
        })
        .collect()
}

#[cfg(windows)]
fn per_user_install(b: Browser) -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    let rel = match b {
        Browser::Chrome => r"Google\Chrome\Application\chrome.exe",
        Browser::Edge => r"Microsoft\Edge\Application\msedge.exe",
        Browser::Brave => r"BraveSoftware\Brave-Browser\Application\brave.exe",
        Browser::Chromium => r"Chromium\Application\chrome.exe",
        Browser::Firefox => r"Mozilla Firefox\firefox.exe",
    };
    let p = local.join(rel);
    p.is_file().then_some(p)
}

#[cfg(not(windows))]
fn per_user_install(b: Browser) -> Option<PathBuf> {
    let candidates: &[&str] = match b {
        Browser::Chrome => &["/usr/bin/google-chrome", "/usr/bin/google-chrome-stable"],
        Browser::Edge => &["/usr/bin/microsoft-edge", "/usr/bin/microsoft-edge-stable"],
        Browser::Brave => &["/usr/bin/brave-browser", "/usr/bin/brave"],
        Browser::Chromium => &[
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ],
        Browser::Firefox => &["/usr/bin/firefox", "/snap/bin/firefox"],
    };
    candidates.iter().map(PathBuf::from).find(|p| p.exists())
}

#[cfg(windows)]
fn registry_default(key: &str) -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(key)
        .ok()?
        .get_value::<String, _>("")
        .ok()
}

#[cfg(not(windows))]
fn registry_default(_key: &str) -> Option<String> {
    None
}

/// What one browser's registration looks like right now.
fn registration(app: &App, browser: Browser, l: &Layout) -> Value {
    let (manifest_file, key) = match install::manifest_location(
        browser,
        os(),
        &home_dir().unwrap_or_default(),
        &app.home.state_dir(),
    ) {
        ManifestLocation::File(p) => (p, None),
        ManifestLocation::Registry { key, manifest_file } => (manifest_file, Some(key)),
    };
    let registry_ok = key
        .as_deref()
        .map(|k| registry_default(k).is_some_and(|v| Path::new(&v) == manifest_file.as_path()));
    let manifest: Option<Value> = std::fs::read(&manifest_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let points_here = manifest
        .as_ref()
        .and_then(|m| m.get("path").and_then(Value::as_str))
        .is_some_and(|p| Path::new(p) == l.launcher.as_path());
    let pinned = manifest.as_ref().is_some_and(|m| {
        let ids = |k: &str| -> Vec<String> {
            m.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        if browser.is_chromium_family() {
            package::chromium_dev_id().is_some_and(|id| {
                ids("allowed_origins").contains(&format!("chrome-extension://{id}/"))
            })
        } else {
            package::firefox_id().is_some_and(|id| ids("allowed_extensions").contains(&id))
        }
    });
    json!({
        "manifest_file": manifest_file.display().to_string(),
        "registry_key": key.map(|k| format!("HKCU\\{k}")),
        "registered": manifest.is_some() && registry_ok.unwrap_or(true),
        "points_at_this_app": points_here,
        "extension_pinned": pinned,
    })
}

/// The full status the Connect page and Settings show.
pub async fn status(app: &std::sync::Arc<App>) -> Result<Value> {
    let l = layout(app);
    let found = detected();
    let allow = allowlist::AllowList::load(&l.allow_list).ok();
    let mut browsers = Vec::new();
    for e in &BROWSERS {
        let (_, recognized, image) = found
            .iter()
            .find(|(b, _, _)| *b == e.browser)
            .cloned()
            .unwrap_or((e.browser, false, None));
        let ext_id = if e.browser.is_chromium_family() {
            package::chromium_dev_id()
        } else {
            package::firefox_id()
        };
        let mut v = json!({
            "id": e.browser.as_str(),
            "name": e.name,
            "installed": image.is_some(),
            "image": image.map(|p| p.display().to_string()),
            "recognized_by_host": recognized,
            "extensions_page": e.page,
            "extension_id": ext_id,
            "family": if e.browser.is_chromium_family() { "chromium" } else { "firefox" },
        });
        if let (Some(obj), Some(r)) = (
            v.as_object_mut(),
            registration(app, e.browser, &l).as_object(),
        ) {
            obj.extend(r.clone());
        }
        browsers.push(v);
    }
    let connected = app
        .daemon
        .raw("daemon.status", json!({}))
        .await
        .ok()
        .and_then(|s| s.get("extension_connected").and_then(Value::as_bool));
    Ok(json!({
        "os": std::env::consts::OS,
        "browsers": browsers,
        "extension": {
            "packaged": package::packaged(),
            "meta": package::meta(),
            "chromium_folder": package::folder(&app.home, package::Family::Chromium).display().to_string(),
            "firefox_folder": package::folder(&app.home, package::Family::Firefox).display().to_string(),
            "unpacked": package::is_unpacked(&app.home, package::Family::Chromium) && package::is_unpacked(&app.home, package::Family::Firefox),
        },
        "host": {
            "exe": l.host_exe.display().to_string(),
            "exe_present": l.host_exe.is_file(),
            "launcher": l.launcher.display().to_string(),
            "launcher_present": l.launcher.is_file(),
            "log": l.log.display().to_string(),
            "allow_list": l.allow_list.display().to_string(),
            "allow_list_present": allow.is_some(),
            "dev_extensions": allow.as_ref().map(|a| a.dev_extensions.clone()).unwrap_or_default(),
            "production_extensions": allow.map(|a| a.extensions).unwrap_or_default(),
        },
        "extension_connected": connected,
        "notes": {
            "development_id": "This release carries the development build of the extension: its id comes from a fixed development key, and the host serves it only because it is listed as a development extension. The id of a store release is not known until the store assigns it.",
            "firefox": "Firefox release builds load an unsigned add-on only as a temporary add-on (about:debugging), which lasts until Firefox closes. A signed add-on needs a store listing, which does not exist yet.",
            "recognized": "Zeus decides what the browser may ask only for browsers it can verify as the parent of the connection: machine-wide installs (registered under HKLM on Windows, in system folders on Linux). A per-user install connects but cannot carry decisions.",
        },
    }))
}

fn describe(step: &Step) -> Value {
    match step {
        Step::WriteFile { path, .. } => {
            json!({"action": "write", "target": path.display().to_string()})
        }
        Step::SetRegistryDefault { key, value } => {
            json!({"action": "set", "target": format!("HKCU\\{key}"), "value": value})
        }
        Step::RemoveFile { path } => {
            json!({"action": "remove", "target": path.display().to_string()})
        }
        Step::DeleteRegistryKey { key } => {
            json!({"action": "delete", "target": format!("HKCU\\{key}")})
        }
    }
}

fn chosen(app: &std::sync::Arc<App>, body: &Value) -> Result<Vec<Browser>> {
    let mut browsers = parse_browsers(body)?;
    if browsers.is_empty() {
        browsers = detected()
            .into_iter()
            .filter(|(_, _, p)| p.is_some())
            .map(|(b, _, _)| b)
            .collect();
    }
    if browsers.is_empty() {
        return Err(Failure::new(
            "integration.no_browser",
            "No supported browser was found. Name one explicitly.",
        ));
    }
    let _ = app;
    Ok(browsers)
}

/// What an installation would write, without writing anything (the consent dialog shows this).
pub fn plan(app: &std::sync::Arc<App>, body: &Value) -> Result<Value> {
    let browsers = chosen(app, body)?;
    let l = layout(app);
    let req = request(app, browsers.clone(), &l)?;
    let steps = install::plan(&req).map_err(|e| Failure::invalid(e.to_string()))?;
    let mut list = vec![
        json!({"action": "write", "target": l.launcher.display().to_string(), "note": "the launcher the browser starts; it selects this app's engine"}),
        json!({"action": "unpack", "target": package::folder(&app.home, package::Family::Chromium).display().to_string(), "note": "the browser extension (Chromium family)"}),
        json!({"action": "unpack", "target": package::folder(&app.home, package::Family::Firefox).display().to_string(), "note": "the browser extension (Firefox)"}),
    ];
    list.extend(steps.iter().map(describe));
    Ok(json!({
        "browsers": browsers.iter().map(|b| b.as_str()).collect::<Vec<_>>(),
        "steps": list,
        "development_extension_ids": req.dev_extension_ids,
        "consent_required": ["development_extension_id"],
    }))
}

/// Unpack the extension, write the launcher and register the host. Needs `consent.development_extension_id`.
pub fn install(app: &std::sync::Arc<App>, body: &Value) -> Result<Value> {
    let consented = body
        .pointer("/consent/development_extension_id")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !consented {
        return Err(Failure::new(
            "integration.consent_required",
            "Confirm that Zeus may serve the development build of its extension (its id is listed as a development extension).",
        ));
    }
    let browsers = chosen(app, body)?;
    package::unpack(&app.home)?;
    let l = write_launcher(app)?;
    let req = request(app, browsers.clone(), &l)?;
    let steps = install::plan(&req).map_err(|e| Failure::invalid(e.to_string()))?;
    std::fs::create_dir_all(app.home.state_dir())?;
    install::apply(&steps, &mut SystemRegistrar)
        .map_err(|e| Failure::new("integration.install", e.to_string()))?;
    app.log.line(
        "integration.install",
        &format!(
            "registered the host for {}",
            browsers
                .iter()
                .map(|b| b.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    Ok(
        json!({"installed": browsers.iter().map(|b| b.as_str()).collect::<Vec<_>>(), "steps": steps.iter().map(describe).collect::<Vec<_>>()}),
    )
}

/// Remove the registrations of the named browsers (all of them when none are named); with every browser removed the
/// launcher and the allow-list go too.
pub fn uninstall(app: &std::sync::Arc<App>, body: &Value) -> Result<Value> {
    let named = parse_browsers(body)?;
    let all = named.is_empty();
    let browsers = if all {
        BROWSERS.iter().map(|e| e.browser).collect()
    } else {
        named
    };
    let l = layout(app);
    let mut req = request(app, browsers.clone(), &l)?;
    req.dev_extension_ids.clear();
    let mut steps = install::plan_uninstall(&req);
    if !all {
        // Keep the shared allow-list while other browsers stay registered.
        steps.retain(|s| !matches!(s, Step::RemoveFile { path } if path == &l.allow_list));
    } else {
        steps.push(Step::RemoveFile {
            path: l.launcher.clone(),
        });
    }
    install::apply(&steps, &mut SystemRegistrar)
        .map_err(|e| Failure::new("integration.uninstall", e.to_string()))?;
    Ok(
        json!({"removed": browsers.iter().map(|b| b.as_str()).collect::<Vec<_>>(), "steps": steps.iter().map(describe).collect::<Vec<_>>()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Vec<(String, String)> {
        vec![
            ("ZEUS_PROFILE".into(), "zeus-app".into()),
            (
                "ZEUS_STATE_DIR".into(),
                "C:\\Users\\Ada Lovelace\\state".into(),
            ),
        ]
    }

    #[test]
    fn the_windows_launcher_sets_the_environment_and_forwards_the_browsers_arguments() {
        let text = launcher_text(
            Os::Windows,
            Path::new("C:\\Program Files\\Zeus\\zeus-native-host.exe"),
            &env(),
            Path::new("C:\\logs\\native-host.log"),
        )
        .unwrap();
        assert!(text.starts_with("@echo off\r\n"));
        assert!(text.contains("set \"ZEUS_PROFILE=zeus-app\"\r\n"));
        assert!(text.contains("set \"ZEUS_STATE_DIR=C:\\Users\\Ada Lovelace\\state\"\r\n"));
        assert!(text.contains("\"C:\\Program Files\\Zeus\\zeus-native-host.exe\" %* 2>>\"C:\\logs\\native-host.log\"\r\n"));
    }

    #[test]
    fn characters_a_batch_file_cannot_carry_are_refused() {
        for bad in ["C:\\100%\\x", "C:\\a!b\\x", "C:\\a\"b\\x"] {
            let r = launcher_text(Os::Windows, Path::new(bad), &env(), Path::new("C:\\l.log"));
            assert_eq!(r.unwrap_err().code, "integration.path", "{bad}");
        }
    }

    #[test]
    fn the_unix_launcher_quotes_everything_and_execs_so_the_browser_stays_the_parent() {
        let env = vec![(
            "ZEUS_STATE_DIR".to_string(),
            "/home/a b/it's/state".to_string(),
        )];
        let text = launcher_text(
            Os::Linux,
            Path::new("/opt/zeus/zeus-native-host"),
            &env,
            Path::new("/home/a/log"),
        )
        .unwrap();
        assert!(text.starts_with("#!/bin/sh\n"));
        assert!(text.contains("export ZEUS_STATE_DIR='/home/a b/it'\\''s/state'\n"));
        assert!(text.contains("exec '/opt/zeus/zeus-native-host' \"$@\" 2>>'/home/a/log'\n"));
    }

    #[test]
    fn browsers_are_parsed_and_unknown_ones_refused() {
        assert_eq!(
            parse_browsers(&json!({"browsers": ["edge", "firefox"]})).unwrap(),
            [Browser::Edge, Browser::Firefox]
        );
        assert!(parse_browsers(&json!({"browsers": ["netscape"]})).is_err());
        assert!(parse_browsers(&json!({})).unwrap().is_empty());
    }
}
