//! Read-only network cost hints. Fixed OS commands, bounded lifetime, no credentials or shell input.
use zeus_api::methods::queues::NetworkCost;

pub(super) async fn detect() -> NetworkCost {
    let mut command = command();
    command.kill_on_drop(true);
    command.stdin(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), command.output()).await;
    match result {
        Ok(Ok(output)) if output.status.success() && output.stdout.len() <= 16_384 => {
            parse(&String::from_utf8_lossy(&output.stdout))
        }
        _ => NetworkCost::Unknown,
    }
}

#[cfg(windows)]
fn command() -> tokio::process::Command {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let mut c = tokio::process::Command::new(
        std::path::PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe"),
    );
    c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", r#"$ErrorActionPreference='Stop'; [void][Windows.Networking.Connectivity.NetworkInformation,Windows,ContentType=WindowsRuntime]; $p=[Windows.Networking.Connectivity.NetworkInformation]::GetInternetConnectionProfile(); if($null -eq $p){'unknown'}else{$c=$p.GetConnectionCost(); if($c.Roaming -or $c.OverDataLimit -or $c.ApproachingDataLimit -or $c.NetworkCostType -eq 'Fixed' -or $c.NetworkCostType -eq 'Variable'){'metered'}elseif($c.NetworkCostType -eq 'Unrestricted'){'unmetered'}else{'unknown'}}"#]);
    c
}

#[cfg(not(windows))]
fn command() -> tokio::process::Command {
    let mut c = tokio::process::Command::new("nmcli");
    c.env("LC_ALL", "C");
    c.args([
        "--terse",
        "--fields",
        "GENERAL.STATE,GENERAL.METERED",
        "device",
        "show",
    ]);
    c
}

fn parse(text: &str) -> NetworkCost {
    match text.trim() {
        "metered" => return NetworkCost::Metered,
        "unmetered" => return NetworkCost::Unmetered,
        _ => {}
    }
    // Multiple active interfaces: conservatively respect any metered one. Ignore disconnected devices.
    let mut connected = false;
    let mut known = false;
    let mut unknown = false;
    for line in text.lines() {
        if let Some(state) = line.strip_prefix("GENERAL.STATE:") {
            connected = state.split_whitespace().next() == Some("100");
        } else if connected && let Some(cost) = line.strip_prefix("GENERAL.METERED:") {
            match cost.trim() {
                "yes" | "guess-yes" => return NetworkCost::Metered,
                "no" | "guess-no" => known = true,
                _ => unknown = true,
            }
        }
    }
    if known && !unknown {
        NetworkCost::Unmetered
    } else {
        NetworkCost::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cost_is_truthful_and_disconnected_interfaces_do_not_pause_work() {
        assert_eq!(parse("metered\r\n"), NetworkCost::Metered);
        assert_eq!(parse("unknown"), NetworkCost::Unknown);
        assert_eq!(
            parse("GENERAL.STATE:30 (disconnected)\nGENERAL.METERED:yes"),
            NetworkCost::Unknown
        );
        assert_eq!(
            parse(
                "GENERAL.STATE:100 (connected)\nGENERAL.METERED:no\nGENERAL.STATE:100 (connected)\nGENERAL.METERED:guess-yes"
            ),
            NetworkCost::Metered
        );
        assert_eq!(
            parse("GENERAL.STATE:100 (connected)\nGENERAL.METERED:guess-no"),
            NetworkCost::Unmetered
        );
    }
}
