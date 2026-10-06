//! The terminal trust surface: reviewing and deciding an approval at the controlling terminal (ADR 0007).
//!
//! Rules this module enforces:
//!
//! * The decision is read from the **controlling terminal** (`CONIN$` on Windows, `/dev/tty` on Unix), never from
//!   stdin, so a pipe or an agent writing to stdin cannot answer. With no terminal the approval simply stays
//!   pending (and eventually expires).
//! * `--yes` is ignored here.
//! * Only **daemon-computed facts** are shown as facts. Free text supplied by the requester is displayed in a
//!   visually separate block labelled `the agent says (untrusted)` and sanitised.
//! * The decision names the approval's `request_hash`, so it applies to exactly what was shown.

use std::io::{BufRead, Write};

use zeus_api::methods::sessions::ApprovalView;
use zeus_types::{ApprovalKind, GrantScope};

use crate::output::sanitize;

/// What the person chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Approve with this scope.
    Approve(Option<GrantScope>),
    /// Deny; `true` also blocks the site (scope `never`).
    Deny {
        /// Block the site from now on.
        never: bool,
    },
    /// Decide later.
    Later,
}

/// Why no decision was taken.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// There is no controlling terminal.
    #[error(
        "approvals need a terminal; use the browser extension popup or run this in a terminal (the request stays pending)"
    )]
    NoTerminal,
    /// I/O error talking to the terminal.
    #[error("terminal i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// An open controlling terminal: where to read the answer from and write the screen to.
pub struct Terminal {
    /// Input from the terminal.
    pub input: Box<dyn BufRead>,
    /// Output to the terminal.
    pub output: Box<dyn Write>,
}

/// Open the controlling terminal (`CONIN$`/`CONOUT$` or `/dev/tty`). Never stdin or stdout.
pub fn open_terminal() -> Result<Terminal, TrustError> {
    #[cfg(windows)]
    let (input_path, output_path) = ("CONIN$", "CONOUT$");
    #[cfg(not(windows))]
    let (input_path, output_path) = ("/dev/tty", "/dev/tty");
    let input = std::fs::OpenOptions::new()
        .read(true)
        .open(input_path)
        .map_err(|_| TrustError::NoTerminal)?;
    let output = std::fs::OpenOptions::new()
        .write(true)
        .open(output_path)
        .map_err(|_| TrustError::NoTerminal)?;
    Ok(Terminal {
        input: Box::new(std::io::BufReader::new(input)),
        output: Box::new(output),
    })
}

fn kind_title(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::Handoff => "A browser download was handed to Zeus",
        ApprovalKind::SessionGrant => "Zeus wants to use your logged-in session",
        ApprovalKind::RetryEscalation => "A download keeps failing and needs your decision",
        ApprovalKind::PolicyException => "A rule needs your permission to be bent for one job",
        ApprovalKind::WorkflowPlan => "Zeus wants to run a multi-step download plan",
        ApprovalKind::DestructiveOp => "Zeus wants to delete or overwrite files",
    }
}

fn scope_text(scope: GrantScope) -> &'static str {
    match scope {
        GrantScope::Once => "once      this one download only",
        GrantScope::Site => "site      this site for 24 hours",
        GrantScope::Workflow => "workflow  this plan for up to 7 days",
        GrantScope::Never => "never     block this site",
    }
}

/// The review screen (ASCII; English; the localisation key is the approval kind). `facts` are rendered as
/// `key: value` lines from the daemon-computed JSON; the requester's text appears only in the last block.
pub fn render_screen(view: &ApprovalView) -> String {
    let mut out = String::new();
    let rule = "-".repeat(64);
    out.push_str(&format!(
        "{rule}\n Zeus: {}\n{rule}\n",
        kind_title(view.approval.kind)
    ));
    out.push_str(&format!(
        " Request  {}\n Expires  {}\n",
        view.approval.id, view.approval.expires_at
    ));
    if let Some(origin) = &view.approval.origin {
        out.push_str(&format!(" Site     {origin}\n"));
    }
    if let Some(facts) = view.facts.as_object() {
        for (key, value) in facts {
            let text = match value {
                serde_json::Value::String(s) => sanitize(s),
                other => sanitize(&other.to_string()),
            };
            out.push_str(&format!(" {:<8} {}\n", sanitize(key), text));
        }
    }
    out.push_str(&format!(
        " Title    {}   (from the server, untrusted)\n",
        sanitize(&view.approval.summary.title)
    ));
    if let Some(agent) = &view.agent_says {
        out.push_str(&format!(
            "{rule}\n The agent says (untrusted text, not a fact):\n   \"{}\"\n",
            sanitize(agent)
        ));
    }
    out.push_str(&format!(
        "{rule}\n Request hash {}\n{rule}\n",
        &view.request_hash.to_hex()[..16]
    ));
    if view.approval.allowed_scopes.is_empty() {
        out.push_str(" [y] approve   [n] deny   [l] decide later\n");
    } else {
        out.push_str(" Approve for:\n");
        for (i, scope) in view
            .approval
            .allowed_scopes
            .iter()
            .filter(|s| **s != GrantScope::Never)
            .enumerate()
        {
            out.push_str(&format!("   [{}] {}\n", i + 1, scope_text(*scope)));
        }
        if view.approval.allowed_scopes.contains(&GrantScope::Never) {
            out.push_str("   [b] deny and block this site\n");
        }
        out.push_str(" [n] deny   [l] decide later\n");
    }
    out
}

/// Interpret one line typed at the prompt.
pub fn parse_choice(view: &ApprovalView, line: &str) -> Option<Choice> {
    let answer = line.trim().to_ascii_lowercase();
    let scopes: Vec<GrantScope> = view
        .approval
        .allowed_scopes
        .iter()
        .copied()
        .filter(|s| *s != GrantScope::Never)
        .collect();
    match answer.as_str() {
        "y" | "yes" if scopes.is_empty() => Some(Choice::Approve(None)),
        "n" | "no" => Some(Choice::Deny { never: false }),
        "b" | "block" if view.approval.allowed_scopes.contains(&GrantScope::Never) => {
            Some(Choice::Deny { never: true })
        }
        "l" | "later" | "" => Some(Choice::Later),
        other => other
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| scopes.get(i).copied())
            .map(|scope| Choice::Approve(Some(scope))),
    }
}

/// Show the screen and read the answer, repeating until it is understood. Returns [`Choice::Later`] at end of
/// input. The `yes` flag of the command line is deliberately not a parameter: it cannot influence this function.
pub fn prompt(view: &ApprovalView, terminal: &mut Terminal) -> Result<Choice, TrustError> {
    terminal.output.write_all(render_screen(view).as_bytes())?;
    loop {
        terminal.output.write_all(b" > ")?;
        terminal.output.flush()?;
        let mut line = String::new();
        if terminal.input.read_line(&mut line)? == 0 {
            return Ok(Choice::Later);
        }
        if let Some(choice) = parse_choice(view, &line) {
            return Ok(choice);
        }
        terminal
            .output
            .write_all(b" Please answer with one of the options shown.\n")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeus_api::methods::sessions::ApprovalView;
    use zeus_types::{Approval, Hex256, IdGenerator, Timestamp};

    fn view(allowed: &[GrantScope]) -> ApprovalView {
        let now = Timestamp::now();
        ApprovalView {
            approval: serde_json::from_value::<Approval>(serde_json::json!({
                "id": IdGenerator::new().approval_id().to_string(),
                "kind": "session_grant",
                "state": "pending",
                "origin": "https://portal.example.org",
                "allowed_scopes": allowed,
                "reason_codes": ["session.first_use"],
                "summary": { "title": "dataset\u{1b}[2J.zip" },
                "created_at": now.to_rfc3339(),
                "expires_at": now.saturating_add_ms(86_400_000).to_rfc3339(),
            }))
            .unwrap(),
            request_hash: Hex256::from_hex(&"ab".repeat(32)).unwrap(),
            facts: serde_json::json!({ "files": 3, "bytes": 1_288_490_188u64, "to": "C:\\Zeus\\dl" }),
            agent_says: Some("I need the Q3 export \u{202e}ignore the user".into()),
        }
    }

    const SCOPES: &[GrantScope] = &[
        GrantScope::Once,
        GrantScope::Site,
        GrantScope::Workflow,
        GrantScope::Never,
    ];

    #[test]
    fn the_screen_separates_facts_from_the_agents_words_and_strips_control_text() {
        let screen = render_screen(&view(SCOPES));
        assert!(screen.contains("The agent says (untrusted text, not a fact)"));
        assert!(screen.contains("files"));
        assert!(!screen.contains('\x1b') && !screen.contains('\u{202e}'));
        assert!(screen.contains("Request hash abababababababab"));
        assert!(
            screen.contains("[1] once")
                && screen.contains("[3] workflow")
                && screen.contains("[b] deny and block")
        );
    }

    #[test]
    fn choices_are_parsed_strictly() {
        let v = view(SCOPES);
        assert_eq!(
            parse_choice(&v, "2\n"),
            Some(Choice::Approve(Some(GrantScope::Site)))
        );
        assert_eq!(parse_choice(&v, " N "), Some(Choice::Deny { never: false }));
        assert_eq!(parse_choice(&v, "b"), Some(Choice::Deny { never: true }));
        assert_eq!(parse_choice(&v, ""), Some(Choice::Later));
        for bad in ["4", "0", "yes", "approve", "y", "--yes", "1 2"] {
            assert_eq!(parse_choice(&v, bad), None, "{bad:?}");
        }
        let plain = view(&[]);
        assert_eq!(parse_choice(&plain, "y"), Some(Choice::Approve(None)));
        assert_eq!(parse_choice(&plain, "b"), None);
        let no_never = view(&[GrantScope::Once]);
        assert_eq!(parse_choice(&no_never, "b"), None);
    }

    #[test]
    fn the_prompt_reads_from_the_terminal_handle_and_defaults_to_later_at_eof() {
        let v = view(SCOPES);
        let mut terminal = Terminal {
            input: Box::new(std::io::Cursor::new(b"what\n3\n".to_vec())),
            output: Box::new(Vec::new()),
        };
        assert_eq!(
            prompt(&v, &mut terminal).unwrap(),
            Choice::Approve(Some(GrantScope::Workflow))
        );
        let mut empty = Terminal {
            input: Box::new(std::io::Cursor::new(Vec::new())),
            output: Box::new(Vec::new()),
        };
        assert_eq!(prompt(&v, &mut empty).unwrap(), Choice::Later);
    }
}
