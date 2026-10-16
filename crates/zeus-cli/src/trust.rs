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

/// Verify the full current request before any approval is offered. Legacy, unknown and altered
/// facts are refused; both CLI and TUI use this gate.
pub fn validate_request(value: &serde_json::Value) -> zeus_api::ApiResult<()> {
    use jsonschema::{Registry, Resource};
    use serde_json::{Value, json};
    let denied = || {
        zeus_api::ApiError::new(
            zeus_api::error::codes::HUMAN_REQUIRED,
            "This approval failed its integrity check or uses unsupported facts; update Zeus and review it again.",
        )
    };
    let schemas: Vec<Value> = [
        include_str!("../../../docs/spec/schema/common.schema.json"),
        include_str!("../../../docs/spec/schema/cloud.v1.schema.json"),
        include_str!("../../../docs/spec/schema/domain.schema.json"),
        include_str!("../../../docs/spec/schema/approvals.v1.schema.json"),
        include_str!("../../../docs/spec/schema/native-messaging.v1.schema.json"),
    ]
    .iter()
    .map(|text| serde_json::from_str(text).expect("bundled schema"))
    .collect();
    let mut registry = Registry::new();
    for schema in strictify(&schemas) {
        let id = schema["$id"].as_str().ok_or_else(denied)?.to_string();
        registry = registry
            .add(&id, Resource::from_contents(schema))
            .map_err(|_| denied())?;
    }
    let registry = registry.prepare().map_err(|_| denied())?;
    let validator = jsonschema::options()
        .with_registry(&registry)
        .build(&json!({"$ref":"https://zeus.invalid/schema/approvals.v1#/$defs/ApprovalRequest"}))
        .map_err(|_| denied())?;
    if !validator.is_valid(value) {
        return Err(denied());
    }
    let facts = &value["facts"];
    let bytes = zeus_verify::canonical::to_jcs_bytes(facts).map_err(|_| denied())?;
    let hash = zeus_verify::canonical::request_hash(facts).map_err(|_| denied())?;
    let requester = facts["requester"]["client_id"].as_str().unwrap_or("daemon");
    let agent_hash = value
        .get("agent_says")
        .and_then(Value::as_str)
        .map(|s| blake3::hash(s.as_bytes()).to_hex().to_string());
    if bytes.len() > 16_384
        || value["request_hash"] != hash.to_hex()
        || facts["approval_id"] != value["id"]
        || facts["revision"] != value["revision"]
        || facts["kind"] != value["kind"]
        || facts["expires_at"] != value["expires_at"]
        || facts["allowed_scopes"] != value["allowed_scopes"]
        || facts["reason_codes"] != value["reason_codes"]
        || value["requested_by"] != requester
        || facts["agent_says_blake3"] != json!(agent_hash)
        || (facts.get("origin").is_some() && facts["origin"] != value["origin"])
    {
        return Err(denied());
    }
    if facts["kind"] == "cloud_delivery" {
        let plan = &facts["plan"];
        if facts["delivery_id"] != plan["delivery_id"]
            || facts["plan_blake3"] != plan["plan_hash"]
            || facts["shares_full_source_url_with_provider"]
                != json!(plan["mode"] == "remote_import")
            || facts["remote_byte_cap_enforced"] != plan["hard_byte_cap"]
            || facts["remote_cancellation_supported"] != plan["remote_cancellation_supported"]
            || plan["supported"] != true
        {
            return Err(denied());
        }
    }
    Ok(())
}

/// Minimum confirmation for the selected approve option. OS authentication is not implemented.
pub fn confirmation(view: &ApprovalView, scope: Option<GrantScope>) -> zeus_api::ApiResult<&str> {
    let key = match scope {
        Some(GrantScope::Once) => "once",
        Some(GrantScope::Site) => "site",
        Some(GrantScope::Workflow) => "workflow",
        _ => "approve",
    };
    match view.facts["confirmation"][key].as_str() {
        Some(level @ ("gesture" | "typed_code")) => Ok(level),
        _ => Err(zeus_api::ApiError::new(
            zeus_api::error::codes::HUMAN_REQUIRED,
            "This request needs an unsupported confirmation method.",
        )),
    }
}

/// Human-readable cross-check code (spec 07 §4.6).
pub fn bind_code(hash: &zeus_types::Hex256) -> String {
    let hex = hash.to_hex();
    let mut n = u32::from_str_radix(&hex[..8], 16).unwrap_or(0) >> 2;
    let alphabet = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut code = [b'0'; 6];
    for i in (0..6).rev() {
        code[i] = alphabet[(n & 31) as usize];
        n >>= 5;
    }
    format!(
        "{}-{}",
        std::str::from_utf8(&code[..3]).unwrap_or(""),
        std::str::from_utf8(&code[3..]).unwrap_or("")
    )
}
fn normalize_code(value: &str) -> String {
    value
        .to_ascii_uppercase()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-'))
        .map(|c| match c {
            'O' => '0',
            'I' | 'L' => '1',
            _ => c,
        })
        .collect()
}

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
    /// The requested confirmation cannot be performed.
    #[error("this request requires a supported, verified confirmation method")]
    Unsupported,
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
    /// Retained OS terminal handle for flushing typeahead; absent only in injected unit-test IO.
    pub(crate) physical_input: Option<std::fs::File>,
}

impl Terminal {
    /// Discard input queued before a decision is armed, including a BufReader's cached bytes.
    pub fn discard_pending(&mut self) -> std::io::Result<()> {
        if let Some(file) = &self.physical_input {
            crate::terminal_os::discard_input(file)?;
            self.input = Box::new(std::io::BufReader::new(file.try_clone()?));
        }
        Ok(())
    }
}

/// Open the controlling terminal (`CONIN$`/`CONOUT$` or `/dev/tty`). Never stdin or stdout.
pub fn open_terminal() -> Result<Terminal, TrustError> {
    #[cfg(windows)]
    let (input_path, output_path) = ("CONIN$", "CONOUT$");
    #[cfg(not(windows))]
    let (input_path, output_path) = ("/dev/tty", "/dev/tty");
    let input = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(input_path)
        .map_err(|_| TrustError::NoTerminal)?;
    let output = std::fs::OpenOptions::new()
        .write(true)
        .open(output_path)
        .map_err(|_| TrustError::NoTerminal)?;
    use std::io::IsTerminal;
    if !input.is_terminal() || !output.is_terminal() {
        return Err(TrustError::NoTerminal);
    }
    let physical_input = Some(input.try_clone()?);
    Ok(Terminal {
        input: Box::new(std::io::BufReader::new(input)),
        output: Box::new(output),
        physical_input,
    })
}

fn kind_title(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::CloudDelivery => "Zeus wants to deliver a file to cloud storage",
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

// Approval facts preserve path separators. Invisible controls are shown as escapes rather
// than changing the destination the person is reviewing.
fn fact_text(text: &str) -> String {
    text.chars().flat_map(|c|{
        if c.is_control() || matches!(c,'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2060}'..='\u{2064}'|'\u{2066}'..='\u{2069}'|'\u{feff}'){
            c.escape_default().collect::<Vec<_>>()
        }else{vec![c]}
    }).collect()
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
    if view.approval.kind == ApprovalKind::CloudDelivery {
        out.push_str(" Cloud delivery: review the account revision, destination, mode and source disclosure.\n Provider imports may continue after local cancellation when remote cancellation is unavailable.\n A source hash and remote size do not imply a matching remote hash.\n");
    }
    if let Some(facts) = view.facts.as_object() {
        for (key, value) in facts {
            if view.approval.kind == ApprovalKind::CloudDelivery
                && key == "plan"
                && let Some(plan) = value.as_object()
            {
                out.push_str(" Delivery plan (daemon facts):\n");
                for (field, value) in plan {
                    let label = match field.as_str() {
                        "account_id" => "Account",
                        "account_revision" => "Account revision",
                        "provider" => "Provider",
                        "destination" => "Folder",
                        "remote_name" => "Object name",
                        "mode" => "Delivery mode",
                        "source_redacted" => "Source",
                        "size_bytes" => "Exact bytes",
                        "max_bytes" => "Byte ceiling",
                        "max_seconds" => "Lifetime (seconds)",
                        "hard_byte_cap" => "Hard byte cap",
                        "remote_cancellation_supported" => "Remote cancellation",
                        "expected_sha256" => "Expected SHA-256",
                        "allow_unbounded_remote" => "Weaker remote limits",
                        "plan_hash" => "Plan hash",
                        "source_hash" => "Source identity hash",
                        other => other,
                    };
                    let text = value
                        .as_str()
                        .map(fact_text)
                        .unwrap_or_else(|| fact_text(&value.to_string()));
                    out.push_str(&format!("   {:<22} {}\n", fact_text(label), text));
                }
                continue;
            }
            let text = match value {
                serde_json::Value::String(s) => fact_text(s),
                other => fact_text(&other.to_string()),
            };
            out.push_str(&format!(" {:<8} {}\n", fact_text(key), text));
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
        "{rule}\n Request code {}   hash {}\n{rule}\n",
        bind_code(&view.request_hash),
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
    terminal.output.flush()?;
    std::thread::sleep(std::time::Duration::from_millis(750));
    terminal.discard_pending()?;
    loop {
        terminal.output.write_all(b" > ")?;
        terminal.output.flush()?;
        let mut line = String::new();
        if terminal.input.read_line(&mut line)? == 0 {
            return Ok(Choice::Later);
        }
        if let Some(choice) = parse_choice(view, &line) {
            if let Choice::Approve(scope) = choice {
                let required = confirmation(view, scope).map_err(|_| TrustError::Unsupported)?;
                if required == "typed_code" {
                    terminal.output.write_all(b" Type the request code: ")?;
                    terminal.output.flush()?;
                    terminal.discard_pending()?;
                    let mut code = String::new();
                    if terminal.input.read_line(&mut code)? == 0 {
                        return Ok(Choice::Later);
                    }
                    if normalize_code(&code) != normalize_code(&bind_code(&view.request_hash)) {
                        terminal
                            .output
                            .write_all(b" The code did not match. Nothing was approved.\n")?;
                        return Ok(Choice::Later);
                    }
                }
            }
            return Ok(choice);
        }
        terminal
            .output
            .write_all(b" Please answer with one of the options shown.\n")?;
    }
}

#[cfg(test)]
pub(crate) fn test_request() -> serde_json::Value {
    include_str!("../../../docs/spec/07-approvals-trust-surface-and-consent.md")
        .split("```")
        .filter_map(|block| {
            serde_json::from_str::<serde_json::Value>(
                &block.lines().skip(1).collect::<Vec<_>>().join("\n"),
            )
            .ok()
        })
        .find(|value| value["kind"] == "handoff" && value.get("facts").is_some())
        .expect("normative handoff request")
}

fn strictify(schemas: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut targets = std::collections::BTreeSet::new();
    for s in schemas {
        collect_allof_targets(s, s["$id"].as_str().unwrap_or_default(), &mut targets);
    }
    schemas
        .iter()
        .map(|s| {
            let mut clone = s.clone();
            walk_strict(
                &mut clone,
                "",
                false,
                s["$id"].as_str().unwrap_or_default(),
                &targets,
            );
            clone
        })
        .collect()
}

fn collect_allof_targets(
    node: &serde_json::Value,
    id: &str,
    out: &mut std::collections::BTreeSet<String>,
) {
    match node {
        serde_json::Value::Array(items) => {
            items.iter().for_each(|n| collect_allof_targets(n, id, out))
        }
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Array(members)) = map.get("allOf") {
                for r in members
                    .iter()
                    .filter_map(|m| m.get("$ref").and_then(serde_json::Value::as_str))
                {
                    out.insert(if r.starts_with('#') {
                        format!("{id}{r}")
                    } else {
                        r.to_owned()
                    });
                }
            }
            map.values().for_each(|v| collect_allof_targets(v, id, out));
        }
        _ => {}
    }
}

fn walk_strict(
    node: &mut serde_json::Value,
    path: &str,
    in_allof: bool,
    id: &str,
    targets: &std::collections::BTreeSet<String>,
) {
    match node {
        serde_json::Value::Array(items) => {
            let is_allof = path.ends_with("/allOf");
            for (i, n) in items.iter_mut().enumerate() {
                walk_strict(n, &format!("{path}/{i}"), is_allof, id, targets);
            }
        }
        serde_json::Value::Object(map) => {
            let def_name = path
                .strip_prefix("/$defs/")
                .filter(|rest| !rest.contains('/'));
            let skip =
                in_allof || def_name.is_some_and(|d| targets.contains(&format!("{id}#/$defs/{d}")));
            let has_props = map
                .get("properties")
                .is_some_and(serde_json::Value::is_object);
            let type_ok = map.get("type").is_none_or(|t| t == "object");
            let allof_without_ref = map.get("allOf").is_some_and(serde_json::Value::is_array)
                && !map.contains_key("$ref");
            if !skip
                && ((has_props && type_ok) || allof_without_ref)
                && !map.contains_key("unevaluatedProperties")
                && !map.contains_key("additionalProperties")
            {
                map.insert(
                    "unevaluatedProperties".into(),
                    serde_json::Value::Bool(false),
                );
                map.entry("type")
                    .or_insert_with(|| serde_json::Value::String("object".into()));
            }
            let keys: Vec<String> = map.keys().cloned().collect();
            for k in keys {
                if matches!(k.as_str(), "enum" | "const" | "default" | "examples") {
                    continue;
                }
                if let Some(child) = map.get_mut(&k) {
                    walk_strict(child, &format!("{path}/{k}"), false, id, targets);
                }
            }
        }
        _ => {}
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
            facts: serde_json::json!({ "files": 3, "bytes": 1_288_490_188u64, "to": "C:\\Zeus\\dl", "confirmation": {"approve":"gesture","once":"gesture","site":"gesture","workflow":"gesture"} }),
            agent_says: Some("I need the Q3 export \u{202e}ignore the user".into()),
        }
    }

    #[test]
    fn cloud_approval_requires_current_bound_facts_and_explicit_disclosure() {
        use serde_json::json;
        let mut request = test_request();
        request.as_object_mut().unwrap().remove("origin");
        request.as_object_mut().unwrap().remove("job_id");
        request["kind"] = json!("cloud_delivery");
        request["allowed_scopes"] = json!([]);
        request["reason_codes"] = json!(["cloud.delivery_requested"]);
        let common = [
            "t",
            "approval_id",
            "revision",
            "kind",
            "requester",
            "issued_at",
            "expires_at",
            "allowed_scopes",
            "confirmation",
            "reason_codes",
            "agent_says_blake3",
        ];
        request["facts"]
            .as_object_mut()
            .unwrap()
            .retain(|key, _| common.contains(&key.as_str()));
        request["facts"]["kind"] = json!("cloud_delivery");
        request["facts"]["allowed_scopes"] = json!([]);
        request["facts"]["reason_codes"] = request["reason_codes"].clone();
        request["facts"]["confirmation"] = json!({"approve":"typed_code"});
        let delivery = "cld_01M48AKW2RDVBCR29C5HZ0ZD6Y";
        let hash = "a".repeat(64);
        request["facts"]["plan"] = json!({"plan_id":"clp_01M48AKW2RDVBCR29C5HZ0ZD6Y","delivery_id":delivery,"plan_hash":hash,"account_id":"research","account_revision":1,"provider":"yandex_disk","mode":"remote_import","source_redacted":"https://example.org","source_hash":"b".repeat(64),"destination":"disk:/Research","remote_name":"report.zip.zeus-cld_SAMPLE","size_bytes":42,"max_bytes":42,"max_seconds":1800,"supported":true,"allow_unbounded_remote":true,"remote_cancellation_supported":false,"hard_byte_cap":false,"expires_at":request["expires_at"]});
        request["facts"]["delivery_id"] = json!(delivery);
        request["facts"]["plan_blake3"] = json!(hash);
        request["facts"]["shares_full_source_url_with_provider"] = json!(true);
        request["facts"]["remote_byte_cap_enforced"] = json!(false);
        request["facts"]["remote_cancellation_supported"] = json!(false);
        request["request_hash"] = json!(
            zeus_verify::canonical::request_hash(&request["facts"])
                .unwrap()
                .to_hex()
        );
        assert!(validate_request(&request).is_ok());
        let mut altered = request.clone();
        altered["facts"]["plan"]["destination"] = json!("disk:/Other");
        assert!(validate_request(&altered).is_err());
        let mut inconsistent = request.clone();
        inconsistent["facts"]["shares_full_source_url_with_provider"] = json!(false);
        inconsistent["request_hash"] = json!(
            zeus_verify::canonical::request_hash(&inconsistent["facts"])
                .unwrap()
                .to_hex()
        );
        assert!(validate_request(&inconsistent).is_err());
        request["facts"]["plan"]["credential"] = json!("ZEUS-CANARY-forbidden");
        request["request_hash"] = json!(
            zeus_verify::canonical::request_hash(&request["facts"])
                .unwrap()
                .to_hex()
        );
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn cloud_review_keeps_disclosure_and_verification_limits_visible() {
        let mut request = view(&[]);
        request.approval.kind = ApprovalKind::CloudDelivery;
        request.facts = serde_json::json!({"mode":"remote_import","account_id":"research","account_revision":3,"source_disclosed":true,"remote_cancellation_supported":false,"destination":"disk:/Research/Exports"});
        let screen = render_screen(&request);
        assert!(screen.contains("cloud storage"));
        assert!(screen.contains("Provider imports may continue"));
        assert!(screen.contains("do not imply a matching remote hash"));
        assert!(screen.contains("account_revision") && screen.contains("remote_import"));
        assert!(screen.contains("disk:/Research/Exports"));
        assert_eq!(fact_text("/a\u{1b}/b"), "/a\\u{1b}/b");
        assert!(screen.contains("[n] deny"));
        request.facts = serde_json::json!({"plan":{"account_id":"research","account_revision":3,"destination":"disk:/Research/Exports","remote_name":"report.zip.zeus-cld_SAMPLE","mode":"staged_upload","size_bytes":42,"max_bytes":42,"max_seconds":1800}});
        let screen = render_screen(&request);
        assert!(screen.contains("Account revision") && screen.contains("Object name"));
        assert!(screen.contains("disk:/Research/Exports") && screen.contains("Lifetime (seconds)"));
    }

    const SCOPES: &[GrantScope] = &[
        GrantScope::Once,
        GrantScope::Site,
        GrantScope::Workflow,
        GrantScope::Never,
    ];

    #[test]
    fn current_request_is_verified_and_unknown_or_changed_facts_are_refused() {
        let request = test_request();
        validate_request(&request).unwrap();
        assert_eq!(
            bind_code(&Hex256::from_hex(request["request_hash"].as_str().unwrap()).unwrap()),
            "17M-4F1"
        );
        let mut altered = request.clone();
        altered["facts"]["transfer"]["total_bytes"] = serde_json::json!(1);
        assert!(validate_request(&altered).is_err());
        let mut unknown = request;
        unknown["facts"]["browser"]["unrecognised_authority"] = serde_json::json!(true);
        unknown["request_hash"] = serde_json::json!(
            zeus_verify::canonical::request_hash(&unknown["facts"])
                .unwrap()
                .to_hex()
        );
        assert!(validate_request(&unknown).is_err());
        assert!(validate_request(&serde_json::json!({"facts": {"files": 1}})).is_err());
    }

    #[test]
    fn the_screen_separates_facts_from_the_agents_words_and_strips_control_text() {
        let screen = render_screen(&view(SCOPES));
        assert!(screen.contains("The agent says (untrusted text, not a fact)"));
        assert!(screen.contains("files"));
        assert!(!screen.contains('\x1b') && !screen.contains('\u{202e}'));
        assert!(screen.contains("hash abababababababab"));
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
            physical_input: None,
        };
        assert_eq!(
            prompt(&v, &mut terminal).unwrap(),
            Choice::Approve(Some(GrantScope::Workflow))
        );
        let mut empty = Terminal {
            input: Box::new(std::io::Cursor::new(Vec::new())),
            output: Box::new(Vec::new()),
            physical_input: None,
        };
        assert_eq!(prompt(&v, &mut empty).unwrap(), Choice::Later);
    }
}
