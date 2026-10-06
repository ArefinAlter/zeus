//! Output rules (ARCHITECTURE section 11): data on stdout, diagnostics and progress on stderr; colour and live
//! progress only on a terminal; `NO_COLOR` honoured; `--json` prints one document, `--ndjson` one object per line.
//!
//! Text that came from a remote server or a file (names, paths, titles) is **untrusted**: [`sanitize`] removes
//! control characters (so a name cannot inject ANSI escape sequences or move the cursor), bidirectional overrides
//! and path separators before it reaches a terminal (00 section 11 rule 3).

use serde::Serialize;
use serde_json::Value;
use zeus_types::{JobState, JobSummary};

use crate::cli::GlobalArgs;

/// How results are printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Text for people.
    Human,
    /// One JSON document.
    Json,
    /// Newline-delimited JSON.
    Ndjson,
}

/// Resolved output settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    /// Format.
    pub mode: OutputMode,
    /// Use ANSI colour.
    pub color: bool,
    /// Print nothing but errors.
    pub quiet: bool,
    /// Draw live progress (only on a terminal, in human mode, not quiet).
    pub live_progress: bool,
}

impl Output {
    /// Decide from flags, environment and terminal detection. `no_color_env` is whether `NO_COLOR` is set to a
    /// non-empty value; `stdout_tty` and `stderr_tty` come from the caller (testable).
    pub fn resolve(
        global: &GlobalArgs,
        no_color_env: bool,
        stdout_tty: bool,
        stderr_tty: bool,
    ) -> Self {
        let mode = if global.json {
            OutputMode::Json
        } else if global.ndjson {
            OutputMode::Ndjson
        } else {
            OutputMode::Human
        };
        let human = mode == OutputMode::Human;
        Self {
            mode,
            color: human && stdout_tty && !global.no_color && !no_color_env,
            quiet: global.quiet,
            live_progress: human && stderr_tty && !global.quiet,
        }
    }
}

/// Remove everything that must not reach a terminal from untrusted text: control characters (including ESC, so no
/// escape sequences), DEL, bidirectional overrides and isolates, zero-width characters, and path separators.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '/' | '\\' => Some('_'),
            c if c.is_control() => None,
            '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}' => None,
            c => Some(c),
        })
        .collect()
}

/// Binary units: `1.5 GiB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `43s`, `2m05s`, `1h02m`.
pub fn format_eta(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60),
    }
}

/// `[#####-----]` with width cells; unknown totals render as a moving-free `[?????]`.
pub fn progress_bar(done: u64, total: Option<u64>, width: usize) -> String {
    match total {
        Some(total) if total > 0 => {
            let filled =
                ((u128::from(done.min(total)) * width as u128) / u128::from(total)) as usize;
            format!("[{}{}]", "#".repeat(filled), "-".repeat(width - filled))
        }
        _ => format!("[{}]", "?".repeat(width)),
    }
}

/// Percentage, or `--` when the total is unknown.
pub fn percent(done: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total > 0 => format!(
            "{:>3}%",
            (u128::from(done.min(total)) * 100 / u128::from(total)) as u64
        ),
        _ => " --".to_string(),
    }
}

fn state_color(state: JobState) -> &'static str {
    match state {
        JobState::Completed => "32",
        JobState::Failed => "31",
        JobState::AwaitingApproval | JobState::AwaitingSession | JobState::RetryScheduled => "33",
        JobState::Canceled | JobState::Paused => "90",
        _ => "36",
    }
}

/// Wrap `text` in an ANSI colour when `color` is on.
pub fn paint(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// One line per job: id, state, bar, sizes, name (sanitised).
pub fn job_line(summary: &JobSummary, color: bool) -> String {
    let state = serde_json::to_value(summary.state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let name = summary
        .filename
        .as_deref()
        .map(sanitize)
        .unwrap_or_else(|| "-".to_string());
    let sizes = match summary.bytes_total {
        Some(total) => format!(
            "{} / {}",
            format_bytes(summary.bytes_done),
            format_bytes(total)
        ),
        None => format_bytes(summary.bytes_done),
    };
    let mut line = format!(
        "{}  {:<17} {} {} {}  {}",
        summary.id,
        paint(&state, state_color(summary.state), color),
        progress_bar(summary.bytes_done, summary.bytes_total, 20),
        percent(summary.bytes_done, summary.bytes_total),
        sizes,
        name
    );
    if let Some(error) = &summary.last_error {
        line.push_str(&format!("  [{}]", error.code));
    }
    line
}

/// Print a value as pretty JSON (single document) or one compact line (NDJSON).
pub fn json_text<T: Serialize>(value: &T, mode: OutputMode) -> String {
    let value: Value = serde_json::to_value(value).unwrap_or(Value::Null);
    match mode {
        OutputMode::Ndjson => serde_json::to_string(&value).unwrap_or_default(),
        _ => serde_json::to_string_pretty(&value).unwrap_or_default(),
    }
}

/// NDJSON lines for a list: one compact object per element.
pub fn ndjson_lines<T: Serialize>(items: &[T]) -> String {
    items
        .iter()
        .map(|i| json_text(i, OutputMode::Ndjson))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The message printed on stderr for an error: `<code>: <message>` plus an optional hint.
pub fn error_line(code: &str, message: &str, hint: Option<&str>) -> String {
    match hint {
        Some(hint) => format!("zeus: {code}: {}\n  hint: {hint}", sanitize(message)),
        None => format!("zeus: {code}: {}", sanitize(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn global(json: bool, ndjson: bool, no_color: bool, quiet: bool) -> GlobalArgs {
        GlobalArgs {
            json,
            ndjson,
            no_color,
            quiet,
            ..GlobalArgs::default()
        }
    }

    #[test]
    fn colour_and_progress_only_on_terminals_in_human_mode() {
        let tty = Output::resolve(&global(false, false, false, false), false, true, true);
        assert!(tty.color && tty.live_progress && tty.mode == OutputMode::Human);
        let piped = Output::resolve(&global(false, false, false, false), false, false, false);
        assert!(!piped.color && !piped.live_progress);
        let no_color_env = Output::resolve(&global(false, false, false, false), true, true, true);
        assert!(!no_color_env.color && no_color_env.live_progress);
        let flag = Output::resolve(&global(false, false, true, false), false, true, true);
        assert!(!flag.color);
        let json = Output::resolve(&global(true, false, false, false), false, true, true);
        assert!(!json.color && !json.live_progress && json.mode == OutputMode::Json);
        let quiet = Output::resolve(&global(false, false, false, true), false, true, true);
        assert!(!quiet.live_progress);
    }

    #[test]
    fn untrusted_names_cannot_inject_terminal_control() {
        let hostile = "report\x1b[2J\x1b]0;pwned\x07.pdf\u{202e}fdp.exe\u{200b}/../x";
        let clean = sanitize(hostile);
        assert!(
            !clean.contains('\x1b')
                && !clean.contains('\x07')
                && !clean.contains('\u{202e}')
                && !clean.contains('\u{200b}')
        );
        assert!(!clean.contains('/'));
        assert_eq!(sanitize("plain name (1).pdf"), "plain name (1).pdf");
    }

    proptest! {
        #[test]
        fn sanitised_text_has_no_control_characters(text in ".{0,100}") {
            let clean = sanitize(&text);
            prop_assert!(clean.chars().all(|c| !c.is_control() && c != '/' && c != '\\'));
        }
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(1_288_490_188), "1.2 GiB");
        assert_eq!(format_eta(43), "43s");
        assert_eq!(format_eta(125), "2m05s");
        assert_eq!(format_eta(3720), "1h02m");
        assert_eq!(progress_bar(50, Some(100), 10), "[#####-----]");
        assert_eq!(progress_bar(500, Some(100), 10), "[##########]");
        assert_eq!(progress_bar(5, None, 4), "[????]");
        assert_eq!(percent(1, Some(3)), " 33%");
        assert_eq!(percent(1, None), " --");
    }

    #[test]
    fn json_modes() {
        let value = serde_json::json!({"a": 1});
        assert!(json_text(&value, OutputMode::Json).contains('\n'));
        assert!(!json_text(&value, OutputMode::Ndjson).contains('\n'));
        assert_eq!(ndjson_lines(&[value.clone(), value]).lines().count(), 2);
    }

    #[test]
    fn error_lines_are_sanitised() {
        let line = error_line(
            "net.dns_failure",
            "name \x1b[31mfails",
            Some("check your network"),
        );
        assert!(!line.contains('\x1b') && line.contains("hint: check your network"));
    }
}
