//! Open the installed human CLI with this desktop's state paths.
//! The app never sends a decision, keystroke, credential or confirmation to the terminal.
use crate::{
    failure::{Failure, Result},
    home::AppHome,
};
use std::path::Path;

fn arguments(action: &str, id: &str) -> Result<Vec<String>> {
    let valid_id = !id.is_empty()
        && !id.starts_with('-')
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    let args = match action {
        "configure" if id.is_empty() => vec!["cloud", "accounts", "configure"],
        "oauth-login" if id.is_empty() => vec!["cloud", "auth", "login"],
        "oauth-status" if valid_id => vec!["cloud", "auth", "status", id],
        "oauth-refresh" if valid_id => vec!["cloud", "auth", "refresh", id],
        "oauth-disconnect" if valid_id => vec!["cloud", "auth", "disconnect", id],
        "oauth-disconnect-remote" if valid_id => {
            vec!["cloud", "auth", "disconnect", id, "--revoke-remote"]
        }
        "revoke" if valid_id => vec!["cloud", "accounts", "revoke", id],
        "approve" if valid_id => vec!["approvals", "approve", id],
        "review" if id.is_empty() => vec!["tui"],
        _ => {
            return Err(Failure::invalid(
                "Choose account setup, cloud sign-in/lifecycle or approval review.",
            ));
        }
    };
    Ok(args.into_iter().map(str::to_owned).collect())
}

#[cfg(windows)]
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Launch a separate, visible terminal. Approval/account changes still require physical human review.
pub fn open(home: &AppHome, bin_dir: &Path, action: &str, id: &str) -> Result<()> {
    let args = arguments(action, id)?;
    let program = crate::supervisor::program(bin_dir, "zeus");
    if !program.is_file() {
        return Err(Failure::new(
            "engine.not_installed",
            "The bundled Zeus CLI is missing. Reinstall Zeus.",
        ));
    }
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let powershell =
            std::path::PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let line = std::iter::once(program.to_string_lossy().into_owned())
            .chain(args)
            .map(|s| ps_quote(&s))
            .collect::<Vec<_>>()
            .join(" ");
        let mut cmd = std::process::Command::new(powershell);
        cmd.args([
            "-NoLogo",
            "-NoProfile",
            "-NoExit",
            "-Command",
            &format!("& {line}"),
        ]);
        cmd.creation_flags(0x0000_0010); // CREATE_NEW_CONSOLE, separate from the background daemon.
        cmd
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let terminal = [
            "/usr/bin/x-terminal-emulator",
            "/usr/bin/gnome-terminal",
            "/usr/bin/konsole",
            "/usr/bin/xterm",
        ]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .ok_or_else(|| {
            Failure::new(
                "app.terminal",
                "Install a desktop terminal to review Zeus accounts and approvals.",
            )
        })?;
        let mut cmd = std::process::Command::new(terminal);
        cmd.arg(if terminal.ends_with("gnome-terminal") {
            "--"
        } else {
            "-e"
        })
        .arg(&program)
        .args(args);
        cmd
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        fn sh_quote(s: &str) -> String {
            format!("'{}'", s.replace('\'', "'\\''"))
        }
        let mut words = vec!["env".to_owned()];
        for (key, value) in home.zeus_env() {
            words.push(sh_quote(&format!("{key}={value}")));
        }
        words.push(sh_quote(&program.to_string_lossy()));
        words.extend(args.iter().map(|s| sh_quote(s)));
        let line = words.join(" ").replace('\\', "\\\\").replace('"', "\\\"");
        let mut cmd = std::process::Command::new("/usr/bin/osascript");
        cmd.args([
            "-e",
            &format!("tell application \"Terminal\" to do script \"{line}\""),
            "-e",
            "tell application \"Terminal\" to activate",
        ]);
        cmd
    };
    command.env_clear();
    for key in [
        "PATH",
        "HOME",
        "USERPROFILE",
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMDATA",
        "COMSPEC",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "LANG",
        "TERM",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.envs(home.zeus_env());
    command
        .spawn()
        .map(|_| ())
        .map_err(|_| Failure::new("app.terminal", "The review terminal could not be opened."))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_closed_operations_and_plain_ids_are_accepted() {
        assert_eq!(
            arguments("configure", "").unwrap(),
            ["cloud", "accounts", "configure"]
        );
        assert_eq!(
            arguments("approve", "01AB-CD").unwrap(),
            ["approvals", "approve", "01AB-CD"]
        );
        assert_eq!(
            arguments("oauth-login", "").unwrap(),
            ["cloud", "auth", "login"]
        );
        assert_eq!(
            arguments("oauth-disconnect-remote", "research").unwrap(),
            ["cloud", "auth", "disconnect", "research", "--revoke-remote"]
        );
        for (action, id) in [
            ("oauth-login", "extra"),
            ("oauth-refresh", "--help"),
            ("oauth-status", "a;echo x"),
            ("oauth-disconnect-remote", "../other"),
            ("oauth-refresh", ""),
            ("configure", "extra"),
            ("approve", "--yes"),
            ("approve", "a;echo x"),
            ("revoke", "../../other"),
            ("shell", ""),
        ] {
            assert!(arguments(action, id).is_err());
        }
    }
    #[cfg(windows)]
    #[test]
    fn powershell_paths_remain_literal() {
        assert_eq!(
            ps_quote("D:\\someone's folder\\$(x).exe"),
            "'D:\\someone''s folder\\$(x).exe'"
        );
    }
}
