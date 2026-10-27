//! Browser-parent evidence for spec 07 §7.4. Missing or ambiguous OS evidence is false.
//!
//! Executable names, argv and environment variables never attest a browser. Windows compares the
//! OS image path to App Paths registration and permits only the OS System32 cmd.exe as a launcher.
//! Linux accepts known installed browser binaries only when their entire canonical path is owned
//! by root and is not group/world writable. macOS and sandboxed browser packages remain unverified.

use std::path::{Path, PathBuf};
use zeus_types::BrowserName;

#[derive(Clone, Debug)]
struct Process {
    pid: u32,
    parent: u32,
    started: u64,
    image: PathBuf,
}

#[derive(Clone, Debug)]
struct Registration {
    name: &'static str,
    firefox: bool,
    image: PathBuf,
}

/// A browser installation the host accepts as a launcher on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecognizedInstall {
    /// `chrome`, `edge`, `brave`, `chromium` or `firefox` (the spelling of the install commands).
    pub name: &'static str,
    /// True for Firefox, whose manifests use add-on ids.
    pub firefox: bool,
    /// The browser executable.
    pub image: PathBuf,
}

/// The browser installs this host would accept as the parent of a connection, so a status page can say which
/// browsers can carry Zeus decisions before anyone clicks. Per-user and portable installs are not listed: they fail
/// closed (see the module documentation).
pub fn recognized_installs() -> Vec<RecognizedInstall> {
    #[cfg(any(target_os = "linux", windows))]
    {
        platform::registered()
            .into_iter()
            .map(|r| RecognizedInstall {
                name: r.name,
                firefox: r.firefox,
                image: r.image,
            })
            .collect()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Vec::new()
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn child_of(child: &Process, parent: &Process) -> bool {
    child.parent == parent.pid
        && child.pid != parent.pid
        && parent.pid != 0
        && parent.started <= child.started
}

/// The name of the registered install that started the host (`edge`, `chrome`, ...), if the process chain proves it.
fn chain_match(
    browser: BrowserName,
    host: &Process,
    parent: &Process,
    grandparent: Option<&Process>,
    registered: &[Registration],
    command: Option<&Path>,
) -> Option<&'static str> {
    if !child_of(host, parent) {
        return None;
    }
    let candidate = if command.is_some_and(|cmd| same_path(cmd, &parent.image)) {
        let grandparent = grandparent?;
        if !child_of(parent, grandparent) || grandparent.pid == host.pid {
            return None;
        }
        grandparent
    } else {
        parent
    };
    let firefox = browser == BrowserName::Firefox;
    registered
        .iter()
        .find(|r| r.firefox == firefox && same_path(&r.image, &candidate.image))
        .map(|r| r.name)
}

#[cfg(test)]
fn verify_chain(
    browser: BrowserName,
    host: &Process,
    parent: &Process,
    grandparent: Option<&Process>,
    registered: &[Registration],
    command: Option<&Path>,
) -> bool {
    chain_match(browser, host, parent, grandparent, registered, command).is_some()
}

fn browser_named(name: &str) -> Option<BrowserName> {
    match name {
        "chrome" => Some(BrowserName::Chrome),
        "edge" => Some(BrowserName::Edge),
        "brave" => Some(BrowserName::Brave),
        "chromium" => Some(BrowserName::Chromium),
        "firefox" => Some(BrowserName::Firefox),
        _ => None,
    }
}

/// Check the actual parent chain and name the browser it proves: the registered install that started this host. The
/// argv of a Chromium-family launch cannot tell Chrome from Edge or Brave, so the verified install, never argv, decides
/// which browser the daemon is told it is talking to. `None` when the parent cannot be verified.
pub fn verified_browser(browser: BrowserName) -> Option<BrowserName> {
    #[cfg(any(target_os = "linux", windows))]
    {
        platform::verify(browser).and_then(browser_named)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = browser;
        None
    }
}

/// Check the actual parent chain. This function never elevates, writes registration or logs paths.
pub fn verify(browser: BrowserName) -> bool {
    verified_browser(browser).is_some()
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn identity(pid: u32) -> Option<Process> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // comm may itself contain spaces or ')'. The last ')' terminates it; state is field 3.
        let rest: Vec<_> = text.rsplit_once(')')?.1.split_whitespace().collect();
        Some(Process {
            pid,
            parent: rest.get(1)?.parse().ok()?,
            started: rest.get(19)?.parse().ok()?,
            image: std::fs::canonicalize(format!("/proc/{pid}/exe")).ok()?,
        })
    }

    fn trusted_install(path: &str) -> Option<PathBuf> {
        let path = std::fs::canonicalize(path).ok()?;
        if !path.is_file() {
            return None;
        }
        for part in path.ancestors() {
            let metadata = std::fs::metadata(part).ok()?;
            if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
                return None;
            }
        }
        Some(path)
    }

    pub(super) fn registered() -> Vec<Registration> {
        [
            ("chrome", false, "/opt/google/chrome/chrome"),
            ("chrome", false, "/opt/google/chrome-beta/chrome"),
            ("chromium", false, "/usr/lib/chromium/chromium"),
            (
                "chromium",
                false,
                "/usr/lib/chromium-browser/chromium-browser",
            ),
            ("edge", false, "/opt/microsoft/msedge/msedge"),
            ("brave", false, "/opt/brave.com/brave/brave"),
            ("firefox", true, "/usr/lib/firefox/firefox"),
            ("firefox", true, "/usr/lib/firefox/firefox-bin"),
            ("firefox", true, "/usr/lib/firefox-esr/firefox-esr"),
        ]
        .into_iter()
        .filter_map(|(name, firefox, path)| {
            trusted_install(path).map(|image| Registration {
                name,
                firefox,
                image,
            })
        })
        .collect()
    }

    pub(super) fn verify(browser: BrowserName) -> Option<&'static str> {
        let host = identity(std::process::id())?;
        let parent = identity(host.parent)?;
        let registered = registered();
        // Reparenting while collecting evidence invalidates this attestation.
        let fresh = identity(host.pid)?;
        if fresh.parent != host.parent || fresh.started != host.started {
            return None;
        }
        chain_match(browser, &host, &parent, None, &registered, None)
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use super::*;
    use std::collections::HashMap;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use winreg::{
        RegKey,
        enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY},
    };

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: Handle owns one successful kernel handle; it is closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn parents() -> Option<HashMap<u32, u32>> {
        // SAFETY: no pointers; snapshot is owned below on success.
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw == INVALID_HANDLE_VALUE || raw.is_null() {
            return None;
        }
        let snapshot = Handle(raw);
        let mut item = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut output = HashMap::new();
        // SAFETY: live snapshot and correctly sized, writable entry.
        let mut found = unsafe { Process32FirstW(snapshot.0, &mut item) };
        while found != 0 {
            output.insert(item.th32ProcessID, item.th32ParentProcessID);
            // SAFETY: same live snapshot and initialized entry.
            found = unsafe { Process32NextW(snapshot.0, &mut item) };
        }
        Some(output)
    }

    fn identity(pid: u32, parents: &HashMap<u32, u32>) -> Option<(Process, Handle)> {
        // SAFETY: OpenProcess takes scalar arguments and grants only query access.
        let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw.is_null() {
            return None;
        }
        let handle = Handle(raw);
        let mut text = vec![0u16; 32_768];
        let mut length = text.len() as u32;
        // SAFETY: valid live process handle; buffer and length describe writable memory.
        if unsafe { QueryFullProcessImageNameW(handle.0, 0, text.as_mut_ptr(), &mut length) } == 0 {
            return None;
        }
        text.truncate(length as usize);
        let image =
            std::fs::canonicalize(PathBuf::from(std::ffi::OsString::from_wide(&text))).ok()?;
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: all four output structs are initialized and writable for this call.
        if unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
        {
            return None;
        }
        if exited.dwHighDateTime != 0 || exited.dwLowDateTime != 0 {
            return None;
        }
        Some((
            Process {
                pid,
                parent: *parents.get(&pid)?,
                started: (u64::from(created.dwHighDateTime) << 32)
                    | u64::from(created.dwLowDateTime),
                image,
            },
            handle,
        ))
    }

    fn command_interpreter() -> Option<PathBuf> {
        let mut text = vec![0u16; 32_768];
        // SAFETY: writable UTF-16 buffer of the declared capacity; no environment lookup.
        let length = unsafe { GetSystemDirectoryW(text.as_mut_ptr(), text.len() as u32) } as usize;
        if length == 0 || length >= text.len() {
            return None;
        }
        text.truncate(length);
        std::fs::canonicalize(PathBuf::from(std::ffi::OsString::from_wide(&text)).join("cmd.exe"))
            .ok()
    }

    pub(super) fn registered() -> Vec<Registration> {
        registrations()
    }

    fn registrations() -> Vec<Registration> {
        let mut output = Vec::new();
        // HKCU App Paths can be rewritten by any process running as this user. It cannot
        // attest a browser parent for human decisions. Per-user browser installs fail closed.
        for hive in [HKEY_LOCAL_MACHINE] {
            for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
                for (short, name, firefox) in [
                    ("chrome", "chrome.exe", false),
                    ("edge", "msedge.exe", false),
                    ("brave", "brave.exe", false),
                    ("chromium", "chromium.exe", false),
                    ("firefox", "firefox.exe", true),
                ] {
                    let key =
                        format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{name}");
                    let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(key, KEY_READ | view)
                    else {
                        continue;
                    };
                    let Ok(value) = key.get_value::<String, _>("") else {
                        continue;
                    };
                    let path = PathBuf::from(value.trim().trim_matches('"'));
                    if !path.is_absolute() || !path.is_file() {
                        continue;
                    }
                    if let Ok(image) = std::fs::canonicalize(path) {
                        output.push(Registration {
                            name: short,
                            firefox,
                            image,
                        });
                    }
                }
            }
        }
        output
    }

    pub(super) fn verify(browser: BrowserName) -> Option<&'static str> {
        let parents = parents()?;
        // Keep handles alive throughout verification so recycled PIDs cannot change identities.
        let (host, _host_handle) = identity(std::process::id(), &parents)?;
        let (parent, _parent_handle) = identity(host.parent, &parents)?;
        let command = command_interpreter();
        let grandparent = if command
            .as_ref()
            .is_some_and(|c| same_path(c, &parent.image))
        {
            Some(identity(parent.parent, &parents)?)
        } else {
            None
        };
        chain_match(
            browser,
            &host,
            &parent,
            grandparent.as_ref().map(|(p, _)| p),
            &registrations(),
            command.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn process(pid: u32, parent: u32, started: u64, image: &str) -> Process {
        Process {
            pid,
            parent,
            started,
            image: image.into(),
        }
    }
    fn registered() -> Vec<Registration> {
        vec![
            Registration {
                name: "chrome",
                firefox: false,
                image: "/installed/chrome.exe".into(),
            },
            Registration {
                name: "firefox",
                firefox: true,
                image: "/installed/firefox.exe".into(),
            },
        ]
    }
    #[test]
    fn direct_registered_browser_is_required_and_family_must_match() {
        let host = process(3, 2, 30, "/host");
        let parent = process(2, 1, 20, "/installed/chrome.exe");
        assert!(verify_chain(
            BrowserName::Chrome,
            &host,
            &parent,
            None,
            &registered(),
            None
        ));
        assert!(!verify_chain(
            BrowserName::Firefox,
            &host,
            &parent,
            None,
            &registered(),
            None
        ));
        assert!(!verify_chain(
            BrowserName::Chrome,
            &host,
            &process(2, 1, 20, "/tmp/chrome.exe"),
            None,
            &registered(),
            None
        ));
        assert!(!verify_chain(
            BrowserName::Chrome,
            &host,
            &parent,
            None,
            &[],
            None
        ));
    }
    #[test]
    fn the_verified_install_names_the_browser_not_argv() {
        let host = process(3, 2, 30, "/host");
        let edge = process(2, 1, 20, "/installed/msedge.exe");
        let registered = vec![
            Registration {
                name: "chrome",
                firefox: false,
                image: "/installed/chrome.exe".into(),
            },
            Registration {
                name: "edge",
                firefox: false,
                image: "/installed/msedge.exe".into(),
            },
        ];
        // A Chromium-family launch is reported as `chrome` by argv; the install says edge.
        assert_eq!(
            chain_match(BrowserName::Chrome, &host, &edge, None, &registered, None),
            Some("edge")
        );
        assert_eq!(browser_named("edge"), Some(BrowserName::Edge));
        assert_eq!(browser_named("netscape"), None);
        assert_eq!(
            chain_match(
                BrowserName::Chrome,
                &host,
                &process(2, 1, 20, "/tmp/msedge.exe"),
                None,
                &registered,
                None
            ),
            None
        );
    }

    #[test]
    fn only_os_command_interpreter_permits_one_extra_parent() {
        let host = process(4, 3, 40, "/host");
        let command = process(3, 2, 30, "/system/cmd.exe");
        let browser = process(2, 1, 20, "/installed/chrome.exe");
        let path = Some(Path::new("/system/cmd.exe"));
        assert!(verify_chain(
            BrowserName::Chrome,
            &host,
            &command,
            Some(&browser),
            &registered(),
            path
        ));
        assert!(!verify_chain(
            BrowserName::Chrome,
            &host,
            &command,
            None,
            &registered(),
            path
        ));
        assert!(!verify_chain(
            BrowserName::Chrome,
            &host,
            &process(3, 2, 30, "/tmp/cmd.exe"),
            Some(&browser),
            &registered(),
            path
        ));
        assert!(!verify_chain(
            BrowserName::Chrome,
            &host,
            &command,
            Some(&process(2, 1, 20, "/system/cmd.exe")),
            &registered(),
            path
        ));
    }
    #[test]
    fn recycled_or_unrelated_pids_fail_closed() {
        let host = process(3, 2, 30, "/host");
        assert!(!verify_chain(
            BrowserName::Chrome,
            &host,
            &process(2, 1, 40, "/installed/chrome.exe"),
            None,
            &registered(),
            None
        ));
        assert!(!verify_chain(
            BrowserName::Chrome,
            &host,
            &process(9, 1, 20, "/installed/chrome.exe"),
            None,
            &registered(),
            None
        ));
        assert!(!verify_chain(
            BrowserName::Chrome,
            &host,
            &host,
            None,
            &registered(),
            None
        ));
    }
    #[test]
    fn recognized_installs_name_only_known_browsers() {
        for install in recognized_installs() {
            assert!(["chrome", "edge", "brave", "chromium", "firefox"].contains(&install.name));
            assert_eq!(install.firefox, install.name == "firefox");
            assert!(install.image.is_absolute());
        }
    }

    #[test]
    fn test_runner_is_not_a_verified_browser_launch() {
        assert!(!verify(BrowserName::Chrome));
        assert!(!verify(BrowserName::Firefox));
    }
}
