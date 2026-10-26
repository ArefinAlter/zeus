//! One function per command. Daemon commands are generic over [`zeus_api::Client`], so tests run them against a
//! fake daemon; local commands (completions, config path, native-host registration, daemon start) need no daemon.
//!
//! A command returns a [`CmdOutput`]: text for people, a JSON document, optional NDJSON items and an [`Exit`]
//! status. `main` decides what to print from the output flags. Errors are [`ApiError`]s; their exit status comes
//! from [`Exit::for_class`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::CommandFactory;
use serde::Serialize;
use serde_json::{Value, json};
use zeus_api::endpoint::{DiscoveryRecord, ZeusPaths};
use zeus_api::error::codes;
use zeus_api::methods::admin::{
    AuditTailParams, AuditVerifyParams, ClientsAddParams, ClientsListParams, ClientsRevokeParams,
    ConfigGetParams, ConfigSetParams, DoctorParams,
};
use zeus_api::methods::daemon::{ShutdownParams, StatusParams};
use zeus_api::methods::jobs::{
    ControlAction, EnqueueParams, JobsControlParams, JobsGetParams, JobsListParams,
    JobsVerifyParams, JobsWaitParams, NewJob, ProbeParams, WaitUntil,
};
use zeus_api::methods::queues::QUEUE_LABEL;
use zeus_api::methods::sessions::{
    ApprovalsListParams, DecideOutcome, SessionsListParams, SessionsRevokeParams,
};
use zeus_api::methods::{
    ApprovalsList, AuditTail, AuditVerify, ClientsAdd, ClientsList, ClientsRevoke, ConfigGet,
    ConfigSet, DaemonShutdown, DaemonStatus, DoctorRun, JobsControl, JobsEnqueue, JobsGet,
    JobsList, JobsProbe, JobsVerify, JobsWait, SessionsList, SessionsRevoke,
};
use zeus_api::{ApiError, ApiResult, Client, PageRequest, call};
use zeus_native_host::install::{self, Browser, InstallRequest, Os, SystemRegistrar};
use zeus_types::{
    ErrorClass, GrantScope, Hex256, HttpUrl, JobId, JobState, JobSummary, Origin, Priority, Surface,
};

use crate::cli::{
    ApprovalsCommand, AuditCommand, BrowserOpt, ClientKindOpt, ClientRoleOpt, ClientsCommand,
    Command, ConfigCommand, DaemonCommand, GetArgs, GlobalArgs, NativeHostCommand, PriorityOpt,
    SessionsCommand, StateOpt,
};
use crate::exit::Exit;
use crate::output::{error_line, format_bytes, job_line, sanitize};
use crate::trust::{self, Choice, Terminal, TrustError};

/// What a command produced.
#[derive(Debug, Clone)]
pub struct CmdOutput {
    /// Text for people (human mode).
    pub text: String,
    /// The JSON document (`--json`).
    pub json: Value,
    /// One object per line for `--ndjson`; empty means "print `json` as one line".
    pub items: Vec<Value>,
    /// The process exit status.
    pub exit: Exit,
}

impl CmdOutput {
    /// No output, success.
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            json: Value::Null,
            items: Vec::new(),
            exit: Exit::Success,
        }
    }

    pub(crate) fn new(text: impl Into<String>, json: &impl Serialize) -> Self {
        Self {
            text: text.into(),
            json: serde_json::to_value(json).unwrap_or(Value::Null),
            items: Vec::new(),
            exit: Exit::Success,
        }
    }

    pub(crate) fn with_items(mut self, items: Vec<Value>) -> Self {
        self.items = items;
        self
    }

    pub(crate) fn with_exit(mut self, exit: Exit) -> Self {
        self.exit = exit;
        self
    }
}

fn to_value(v: &impl Serialize) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn wire(v: &impl Serialize) -> String {
    to_value(v).as_str().map(str::to_string).unwrap_or_default()
}

/// Headers `zeus get -H` accepts (01 section 3.2). Everything else, in particular `Cookie` and `Authorization`, is
/// refused: session material only ever comes through the extension with a person's approval.
pub const ALLOWED_HEADERS: &[&str] = &[
    "accept",
    "accept-language",
    "if-modified-since",
    "x-requested-with",
];

/// Parse `Name: value` into an allowed header.
pub fn parse_header(text: &str) -> ApiResult<(String, String)> {
    let (name, value) = text
        .split_once(':')
        .ok_or_else(|| ApiError::invalid_params("headers look like `Name: value`"))?;
    let name = name.trim();
    if !ALLOWED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(ApiError::invalid_params(format!(
            "header `{}` is not allowed; Cookie and Authorization are never sent from the command line (use --cookie-from-extension)",
            sanitize(name)
        )));
    }
    let value = value.trim();
    if value.is_empty() || value.len() > 2048 || value.chars().any(char::is_control) {
        return Err(ApiError::invalid_params(
            "header values must be 1 to 2048 printable characters",
        ));
    }
    Ok((name.to_string(), value.to_string()))
}

fn parse_job(text: &str) -> ApiResult<JobId> {
    text.parse::<JobId>()
        .map_err(|_| ApiError::invalid_params("a job id looks like job_ followed by 26 characters"))
}

fn control_key() -> String {
    format!("cli-{}", ulid::Ulid::generate())
}

fn priority(p: PriorityOpt) -> Priority {
    match p {
        PriorityOpt::Low => Priority::Low,
        PriorityOpt::Normal => Priority::Normal,
        PriorityOpt::High => Priority::High,
    }
}

fn state(s: StateOpt) -> JobState {
    match s {
        StateOpt::Queued => JobState::Queued,
        StateOpt::Probing => JobState::Probing,
        StateOpt::AwaitingApproval => JobState::AwaitingApproval,
        StateOpt::AwaitingSession => JobState::AwaitingSession,
        StateOpt::Downloading => JobState::Downloading,
        StateOpt::Paused => JobState::Paused,
        StateOpt::RetryScheduled => JobState::RetryScheduled,
        StateOpt::Verifying => JobState::Verifying,
        StateOpt::Finalizing => JobState::Finalizing,
        StateOpt::Completed => JobState::Completed,
        StateOpt::Failed => JobState::Failed,
        StateOpt::Canceled => JobState::Canceled,
    }
}

/// Whether a job has stopped for good as far as a waiting caller is concerned: `failed` is not terminal in the state
/// machine (a manual retry can leave it, 01 section 6) but nothing more will happen without that retry.
pub fn is_settled(state: JobState) -> bool {
    matches!(
        state,
        JobState::Completed | JobState::Failed | JobState::Canceled
    )
}

/// Exit status for a set of finished-or-not jobs: the first failure's class, then quarantine, then "not finished".
pub fn exit_for_jobs(jobs: &[JobSummary]) -> Exit {
    if let Some(failed) = jobs.iter().find(|j| j.state == JobState::Failed) {
        return Exit::Failed(
            failed
                .last_error
                .as_ref()
                .map_or(ErrorClass::Permanent, |e| e.class),
        );
    }
    if jobs.iter().any(|j| j.state == JobState::Canceled) {
        return Exit::Conflict;
    }
    if jobs.iter().any(|j| !is_settled(j.state)) {
        return Exit::Timeout;
    }
    if jobs.iter().any(|j| j.quarantined) {
        return Exit::Quarantined;
    }
    Exit::Success
}

fn render_jobs(jobs: &[JobSummary], color: bool) -> String {
    jobs.iter()
        .map(|j| job_line(j, color))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run a command that needs the daemon.
pub async fn run_remote<C: Client + ?Sized>(
    command: &Command,
    global: &GlobalArgs,
    client: &C,
    color: bool,
    terminal: Option<&mut Terminal>,
) -> ApiResult<CmdOutput> {
    match command {
        Command::Cloud(command) => crate::cloud::run(command, client, global.dry_run).await,
        Command::Workflow(command) => crate::workflow::run(command, client, global.dry_run).await,
        Command::Files(command) => crate::files::run(command, client, global.dry_run).await,
        Command::Artifacts(command) => crate::artifacts::run(command, client, global.dry_run).await,
        Command::Queues(command) => crate::queues::run(command, client, global.dry_run).await,
        Command::Get(args) => get(args, global, client, color).await,
        Command::Probe { url } => probe(url, client).await,
        Command::Ls {
            state: states,
            limit,
        } => {
            let page = call::<JobsList, C>(
                client,
                JobsListParams {
                    page: PageRequest {
                        limit: Some((*limit).clamp(1, 100)),
                        cursor: None,
                    },
                    states: states.iter().copied().map(state).collect(),
                    ..Default::default()
                },
            )
            .await?;
            let items = page.items.iter().map(to_value).collect();
            Ok(CmdOutput::new(render_jobs(&page.items, color), &page).with_items(items))
        }
        Command::Status { job: Some(job) } => {
            let result = call::<JobsGet, C>(
                client,
                JobsGetParams {
                    job_id: parse_job(job)?,
                    reveal_urls: false,
                },
            )
            .await?;
            let summary = format!("{}  {}", result.job.id, wire(&result.job.state));
            Ok(CmdOutput::new(summary, &result))
        }
        Command::Status { job: None } | Command::Daemon(DaemonCommand::Status) => {
            daemon_status(client).await
        }
        Command::Wait { jobs, timeout } => {
            let ids = jobs
                .iter()
                .map(|j| parse_job(j))
                .collect::<ApiResult<Vec<_>>>()?;
            wait_for(client, ids, timeout.map(Duration::from_secs), color).await
        }
        Command::Pause { job } => control(client, job, ControlAction::Pause, color).await,
        Command::Resume { job } => control(client, job, ControlAction::Resume, color).await,
        Command::Cancel { job } => control(client, job, ControlAction::Cancel, color).await,
        Command::Retry { job } => control(client, job, ControlAction::Retry, color).await,
        Command::Verify { job, sha256 } => {
            let expected = sha256
                .as_deref()
                .map(|h| {
                    Hex256::from_hex(h)
                        .map_err(|_| ApiError::invalid_params("--sha256 must be 64 hex digits"))
                })
                .transpose()?;
            let result = call::<JobsVerify, C>(
                client,
                JobsVerifyParams {
                    job_id: parse_job(job)?,
                    expected_sha256: expected,
                },
            )
            .await?;
            let verdict = match (result.matches_recorded, result.matches_expected) {
                (_, Some(false)) | (false, _) => "MISMATCH",
                _ => "ok",
            };
            let exit = if verdict == "ok" {
                if result.quarantined {
                    Exit::Quarantined
                } else {
                    Exit::Success
                }
            } else {
                Exit::Failed(ErrorClass::Integrity)
            };
            Ok(CmdOutput::new(
                format!(
                    "{verdict}  {}  {}",
                    result.sha256.to_hex(),
                    format_bytes(result.size_bytes)
                ),
                &result,
            )
            .with_exit(exit))
        }
        Command::Approvals(sub) => approvals(sub, client, terminal).await,
        Command::Sessions(SessionsCommand::List) => {
            let page = call::<SessionsList, C>(client, SessionsListParams::default()).await?;
            let text = page
                .items
                .iter()
                .map(|g| {
                    format!(
                        "{}  {}  {}  {}  uses {}",
                        g.id,
                        g.origin,
                        wire(&g.scope),
                        wire(&g.state),
                        g.uses
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let items = page.items.iter().map(to_value).collect();
            Ok(CmdOutput::new(text, &page).with_items(items))
        }
        Command::Sessions(SessionsCommand::Revoke { id }) => {
            let grant_id = id.parse().map_err(|_| {
                ApiError::invalid_params("a grant id looks like gr_ followed by 26 characters")
            })?;
            let result = call::<SessionsRevoke, C>(
                client,
                SessionsRevokeParams {
                    grant_id,
                    surface: Surface::CliInteractive,
                },
            )
            .await?;
            Ok(CmdOutput::new(
                format!("revoked {}", result.grant.id),
                &result,
            ))
        }
        Command::Clients(ClientsCommand::List { include_revoked }) => {
            let result = call::<ClientsList, C>(
                client,
                ClientsListParams {
                    include_revoked: *include_revoked,
                },
            )
            .await?;
            let text = result
                .clients
                .iter()
                .map(|c| {
                    format!(
                        "{}  {}  {}  {}  {}{}",
                        c.credential_id,
                        c.name,
                        wire(&c.kind),
                        wire(&c.role),
                        if c.revoked { "revoked" } else { "active" },
                        if c.roots.is_empty() {
                            String::new()
                        } else {
                            format!("  roots {}", c.roots.len())
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let items = result.clients.iter().map(to_value).collect();
            Ok(CmdOutput::new(text, &result).with_items(items))
        }
        Command::Clients(ClientsCommand::Add {
            name,
            kind,
            role,
            permissions,
            idempotency_key,
        }) => {
            let permissions = if permissions.is_empty() {
                None
            } else {
                Some(
                    permissions
                        .iter()
                        .map(|p| p.parse::<zeus_api::Permission>())
                        .collect::<ApiResult<std::collections::BTreeSet<_>>>()?,
                )
            };
            let result = call::<ClientsAdd, C>(
                client,
                ClientsAddParams {
                    name: name.clone(),
                    kind: match kind {
                        ClientKindOpt::Agent => zeus_api::ClientKind::Agent,
                        ClientKindOpt::Service => zeus_api::ClientKind::Service,
                    },
                    role: role.map(|r| match r {
                        ClientRoleOpt::Mcp => zeus_api::ClientRole::Mcp,
                        ClientRoleOpt::A2a => zeus_api::ClientRole::A2a,
                        ClientRoleOpt::Embedded => zeus_api::ClientRole::Embedded,
                        ClientRoleOpt::Service => zeus_api::ClientRole::Service,
                    }),
                    permissions,
                    idempotency_key: idempotency_key.clone(),
                },
            )
            .await?;
            Ok(CmdOutput::new(
                format!(
                    "created {} ({} {})\ntoken file: {}\nPoint the program at this file; the token is never printed.",
                    result.client.name,
                    wire(&result.client.kind),
                    result.client.credential_id,
                    result.credential_file
                ),
                &result,
            ))
        }
        Command::Clients(ClientsCommand::Revoke { credential_id }) => {
            let result = call::<ClientsRevoke, C>(
                client,
                ClientsRevokeParams {
                    credential_id: credential_id.clone(),
                    idempotency_key: None,
                },
            )
            .await?;
            Ok(CmdOutput::new(
                format!(
                    "revoked {} ({}); closed {} connection(s)",
                    result.client.name, result.client.credential_id, result.connections_closed
                ),
                &result,
            ))
        }
        Command::Audit(AuditCommand::Tail { limit }) => {
            let result = call::<AuditTail, C>(
                client,
                AuditTailParams {
                    limit: Some((*limit).clamp(1, 100)),
                    after_seq: None,
                },
            )
            .await?;
            let text = result
                .records
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            let items = result.records.clone();
            Ok(CmdOutput::new(text, &result).with_items(items))
        }
        Command::Audit(AuditCommand::Verify) => {
            let result = call::<AuditVerify, C>(client, AuditVerifyParams::default()).await?;
            let text = if result.ok {
                format!("audit chain ok ({} records)", result.records_checked)
            } else {
                format!(
                    "audit chain BROKEN at record {}",
                    result
                        .first_broken_seq
                        .map_or("?".into(), |s| s.to_string())
                )
            };
            let exit = if result.ok {
                Exit::Success
            } else {
                Exit::Failed(ErrorClass::Integrity)
            };
            Ok(CmdOutput::new(text, &result).with_exit(exit))
        }
        Command::Daemon(sub @ (DaemonCommand::KillSwitch | DaemonCommand::Release)) => {
            use std::io::Write;
            use zeus_api::methods::daemon::{KillSwitchAction, KillSwitchParams};
            let terminal = terminal.ok_or_else(|| {
                ApiError::new(
                    codes::HUMAN_REQUIRED,
                    "This action needs a controlling terminal.",
                )
            })?;
            let release = matches!(sub, DaemonCommand::Release);
            terminal
                .output
                .write_all(if release {
                    b"Release the stop latch? Jobs stay paused. Type release: "
                } else {
                    b"Stop all downloads and revoke session grants? Type stop: "
                })
                .map_err(|e| ApiError::internal(e.to_string()))?;
            terminal
                .output
                .flush()
                .map_err(|e| ApiError::internal(e.to_string()))?;
            std::thread::sleep(std::time::Duration::from_millis(750));
            terminal
                .discard_pending()
                .map_err(|e| ApiError::internal(e.to_string()))?;
            let mut answer = String::new();
            terminal
                .input
                .read_line(&mut answer)
                .map_err(|e| ApiError::internal(e.to_string()))?;
            if answer.trim() != if release { "release" } else { "stop" } {
                return Ok(CmdOutput::new(
                    "Nothing changed.",
                    &json!({"changed":false}),
                ));
            }
            let result = call::<zeus_api::methods::DaemonKillSwitch, C>(
                client,
                KillSwitchParams {
                    action: if release {
                        KillSwitchAction::Release
                    } else {
                        KillSwitchAction::Engage
                    },
                    surface: Some(Surface::CliInteractive),
                    reason: None,
                },
            )
            .await?;
            Ok(CmdOutput::new(
                if result.engaged {
                    "All work stopped; session grants revoked."
                } else {
                    "Stop latch released. Resume jobs individually."
                },
                &result,
            ))
        }
        Command::Daemon(DaemonCommand::Stop) => {
            let result = call::<DaemonShutdown, C>(client, ShutdownParams::default()).await?;
            Ok(CmdOutput::new(
                if result.accepted {
                    "daemon is stopping"
                } else {
                    "daemon refused to stop"
                },
                &result,
            ))
        }
        Command::Config(ConfigCommand::Get { key }) => {
            let result = call::<ConfigGet, C>(client, ConfigGetParams { key: key.clone() }).await?;
            let text = result
                .values
                .iter()
                .map(|(k, v)| format!("{k} = {v}"))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(CmdOutput::new(text, &result))
        }
        Command::Config(ConfigCommand::Set { key, value }) => {
            let value =
                serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.clone()));
            let result = call::<ConfigSet, C>(
                client,
                ConfigSetParams {
                    key: key.clone(),
                    value,
                },
            )
            .await?;
            let text = if result.restart_required {
                "saved; restart the daemon to apply"
            } else {
                "saved"
            };
            Ok(CmdOutput::new(text, &result))
        }
        Command::Doctor => {
            let result = call::<DoctorRun, C>(client, DoctorParams {}).await?;
            let mut worst = Exit::Success;
            let mut lines = Vec::new();
            for check in &result.checks {
                let mark = match check.status {
                    zeus_api::methods::admin::CheckStatus::Ok => "ok  ",
                    zeus_api::methods::admin::CheckStatus::Warn => "warn",
                    zeus_api::methods::admin::CheckStatus::Fail => {
                        worst = Exit::Internal;
                        "FAIL"
                    }
                };
                lines.push(format!(
                    "{mark}  {}  {}",
                    check.id,
                    sanitize(&check.summary)
                ));
                if let Some(fix) = &check.fix {
                    lines.push(format!("        fix: {}", sanitize(fix)));
                }
            }
            Ok(CmdOutput::new(lines.join("\n"), &result).with_exit(worst))
        }
        other => Err(ApiError::not_implemented(match other {
            Command::Auth => "auth needs the controlling-terminal command runner",
            Command::Tui => "zeus tui",
            Command::Mcp { .. } => "zeus mcp (rmcp transport binding)",
            Command::A2a { .. } => "zeus a2a (standalone authenticated bridge)",
            _ => "this command needs no daemon connection",
        })),
    }
}

async fn daemon_status<C: Client + ?Sized>(client: &C) -> ApiResult<CmdOutput> {
    let status = call::<DaemonStatus, C>(client, StatusParams {}).await?;
    let text = format!(
        "zeusd {} (up {}s)  extension: {}  pending approvals: {}{}",
        status.daemon.version,
        status.uptime_ms / 1000,
        if status.extension_connected {
            "connected"
        } else {
            "not connected"
        },
        status.pending_approvals,
        if status.kill_switch_engaged {
            "  KILL SWITCH ENGAGED"
        } else {
            ""
        }
    );
    Ok(CmdOutput::new(text, &status))
}

async fn probe<C: Client + ?Sized>(url: &str, client: &C) -> ApiResult<CmdOutput> {
    let url = HttpUrl::parse(url)
        .map_err(|_| ApiError::invalid_params("expected an absolute http(s) URL"))?;
    let result = call::<JobsProbe, C>(
        client,
        ProbeParams {
            url,
            session_origin: None,
        },
    )
    .await?;
    let p = &result.probe;
    let text = format!(
        "status {}  size {}  ranges {}  session needed: {}  type {}  name {}",
        p.status,
        p.total_bytes.map_or("unknown".into(), format_bytes),
        wire(&p.range_support),
        if p.requires_session { "yes" } else { "no" },
        sanitize(p.content_type.as_deref().unwrap_or("unknown")),
        sanitize(&p.filename.value),
    );
    Ok(CmdOutput::new(text, &result))
}

async fn control<C: Client + ?Sized>(
    client: &C,
    job: &str,
    action: ControlAction,
    color: bool,
) -> ApiResult<CmdOutput> {
    let result = call::<JobsControl, C>(
        client,
        JobsControlParams {
            job_id: parse_job(job)?,
            action,
            priority: None,
            idempotency_key: Some(control_key()),
        },
    )
    .await?;
    Ok(CmdOutput::new(job_line(&result.job, color), &result))
}

fn default_key(urls: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    for url in urls {
        hasher.update(url.as_bytes());
        hasher.update(b"\n");
    }
    format!("cli-{}", &hasher.finalize().to_hex().as_str()[..16])
}

fn get_inputs(args: &GetArgs) -> ApiResult<Vec<String>> {
    use std::io::Read;
    const MAX_BYTES: u64 = 1024 * 1024;
    let mut urls = args.urls.clone();
    if let Some(path) = &args.input_file {
        let error = || {
            ApiError::invalid_params(
                "URL input must be a readable UTF-8 regular file of at most 1 MiB",
            )
        };
        let file = std::fs::File::open(path).map_err(|_| error())?;
        let metadata = file.metadata().map_err(|_| error())?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(error());
        }
        // Bound the read even if another process grows the file after metadata().
        let mut text = String::new();
        file.take(MAX_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|_| error())?;
        if text.len() as u64 > MAX_BYTES {
            return Err(error());
        }
        urls.extend(
            text.trim_start_matches('\u{feff}')
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_owned),
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    urls.retain(|url| seen.insert(url.clone()));
    if urls.is_empty() || urls.len() > 50 {
        return Err(ApiError::invalid_params(
            "get accepts 1–50 unique URLs per request",
        ));
    }
    Ok(urls)
}

async fn get<C: Client + ?Sized>(
    args: &GetArgs,
    global: &GlobalArgs,
    client: &C,
    color: bool,
) -> ApiResult<CmdOutput> {
    let inputs = get_inputs(args)?;
    let mut extra_headers = BTreeMap::new();
    for header in &args.headers {
        let (name, value) = parse_header(header)?;
        extra_headers.insert(name, value);
    }
    if inputs.len() > 1 && args.sha256.is_some() {
        return Err(ApiError::invalid_params("--sha256 needs exactly one URL"));
    }
    let expected = args
        .sha256
        .as_deref()
        .map(|h| {
            Hex256::from_hex(h)
                .map_err(|_| ApiError::invalid_params("--sha256 must be 64 hex digits"))
        })
        .transpose()?;
    let urls = inputs
        .iter()
        .map(|u| {
            HttpUrl::parse(u)
                .map_err(|_| ApiError::invalid_params("expected absolute http(s) URLs"))
        })
        .collect::<ApiResult<Vec<_>>>()?;
    let session_origin: Option<Origin> = if args.cookie_from_extension {
        let origins: std::collections::BTreeSet<Origin> =
            urls.iter().map(HttpUrl::origin).collect();
        if origins.len() != 1 {
            return Err(ApiError::invalid_params(
                "--cookie-from-extension needs URLs of one site; run one command per site",
            ));
        }
        origins.into_iter().next()
    } else {
        None
    };
    let jobs = urls
        .into_iter()
        .map(|url| NewJob {
            url,
            hint_url: None,
            referrer: None,
            dest_dir: args.output.clone(),
            filename: None,
            expected_sha256: expected,
            expected_size: None,
            priority: args.priority.map(priority),
            session_origin: session_origin.clone(),
            grant_scope_hint: None,
            extra_headers: extra_headers.clone(),
            overwrite: None,
            max_connections: None,
            transport_hint: None,
            labels: args
                .queue
                .iter()
                .map(|queue| (QUEUE_LABEL.to_owned(), queue.clone()))
                .collect(),
            run_id: None,
        })
        .collect();
    let key = args
        .idempotency_key
        .clone()
        .unwrap_or_else(|| default_key(&inputs));
    let result = call::<JobsEnqueue, C>(
        client,
        EnqueueParams {
            jobs,
            dry_run: global.dry_run,
            idempotency_key: Some(key),
        },
    )
    .await?;
    let mut lines = Vec::new();
    for entry in &result.jobs {
        let line = match (&entry.job_id, &entry.plan, &entry.error) {
            (Some(id), _, _) => format!(
                "{id}  {}{}",
                entry.state.map_or(String::new(), |s| wire(&s)),
                entry.approval_id.map_or(String::new(), |a| format!(
                    "  waiting for your approval ({a}); run `zeus approvals list`"
                ))
            ),
            (None, Some(plan), _) => format!(
                "would save to {}",
                sanitize(
                    plan.dest_path
                        .as_deref()
                        .unwrap_or("(destination decided at probe)")
                )
            ),
            (None, None, Some(error)) => format!("rejected: {}", error.code),
            _ => "nothing".to_string(),
        };
        lines.push(line);
    }
    let output = CmdOutput::new(lines.join("\n"), &result);
    let ids: Vec<JobId> = result.jobs.iter().filter_map(|j| j.job_id).collect();
    if args.wait && !global.dry_run && !ids.is_empty() {
        let waited = wait_for(client, ids, None, color).await?;
        return Ok(CmdOutput {
            text: format!("{}\n{}", output.text, waited.text),
            ..waited
        });
    }
    Ok(output)
}

async fn wait_for<C: Client + ?Sized>(
    client: &C,
    ids: Vec<JobId>,
    timeout: Option<Duration>,
    color: bool,
) -> ApiResult<CmdOutput> {
    let started = Instant::now();
    let mut since_seq = None;
    loop {
        let remaining = timeout.map(|t| t.saturating_sub(started.elapsed()));
        if remaining.is_some_and(|r| r.is_zero()) {
            let result = call::<JobsWait, C>(
                client,
                JobsWaitParams {
                    job_ids: ids.clone(),
                    until: WaitUntil::AnyChange,
                    timeout_ms: 1,
                    since_seq,
                    progress_bytes: None,
                },
            )
            .await?;
            return Ok(
                CmdOutput::new(render_jobs(&result.jobs, color), &result).with_exit(Exit::Timeout)
            );
        }
        let slice = remaining
            .map_or(30_000, |r| r.as_millis().min(30_000) as u64)
            .max(1);
        let result = call::<JobsWait, C>(
            client,
            JobsWaitParams {
                job_ids: ids.clone(),
                until: WaitUntil::SettledOrBlocked,
                timeout_ms: slice,
                since_seq,
                progress_bytes: None,
            },
        )
        .await?;
        since_seq = Some(result.head_seq);
        let blocked_on_person = result.jobs.iter().any(|j| {
            matches!(
                j.state,
                JobState::AwaitingApproval | JobState::AwaitingSession
            )
        });
        let all_final = result.jobs.iter().all(|j| is_settled(j.state));
        if all_final || blocked_on_person {
            let mut text = render_jobs(&result.jobs, color);
            if blocked_on_person {
                text.push_str("\nwaiting for a person: run `zeus approvals list`");
            }
            let exit = exit_for_jobs(&result.jobs);
            return Ok(CmdOutput::new(text, &result).with_exit(exit));
        }
    }
}

async fn approvals<C: Client + ?Sized>(
    sub: &ApprovalsCommand,
    client: &C,
    terminal: Option<&mut Terminal>,
) -> ApiResult<CmdOutput> {
    match sub {
        ApprovalsCommand::List => {
            let page = call::<ApprovalsList, C>(client, ApprovalsListParams::default()).await?;
            let text = page
                .items
                .iter()
                .map(|a| {
                    format!(
                        "{}  {}  {}  expires {}",
                        a.approval.id,
                        wire(&a.approval.kind),
                        sanitize(&a.approval.summary.title),
                        a.approval.expires_at
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let items = page.items.iter().map(to_value).collect();
            Ok(CmdOutput::new(text, &page).with_items(items))
        }
        ApprovalsCommand::Approve { id, scope } => {
            decide(client, id, false, false, scope.as_deref(), terminal).await
        }
        ApprovalsCommand::Deny { id, never } => {
            decide(client, id, true, *never, None, terminal).await
        }
    }
}

async fn decide<C: Client + ?Sized>(
    client: &C,
    id: &str,
    deny: bool,
    never: bool,
    wanted_scope: Option<&str>,
    terminal: Option<&mut Terminal>,
) -> ApiResult<CmdOutput> {
    // No terminal, no decision. `--yes` is not consulted anywhere on this path.
    let Some(terminal) = terminal else {
        return Err(ApiError::new(
            codes::HUMAN_REQUIRED,
            TrustError::NoTerminal.to_string(),
        ));
    };
    let approval_id = id.parse().map_err(|_| {
        ApiError::invalid_params("an approval id looks like ap_ followed by 26 characters")
    })?;
    let view = read_approval(client, approval_id).await?;
    let choice = trust::prompt(&view, terminal)
        .map_err(|e| ApiError::new(codes::HUMAN_REQUIRED, e.to_string()))?;
    let (outcome, scope) = match (choice, deny) {
        (Choice::Later, _) => {
            return Ok(CmdOutput::new("left pending", &json!({ "decided": false })));
        }
        (Choice::Approve(_), true) => {
            return Ok(CmdOutput::new(
                "you chose to approve, but this command denies; nothing was decided",
                &json!({ "decided": false }),
            ));
        }
        (Choice::Deny { .. }, false) => {
            return Ok(CmdOutput::new(
                "you chose to deny, but this command approves; nothing was decided",
                &json!({ "decided": false }),
            ));
        }
        (Choice::Approve(scope), false) => {
            if let Some(wanted) = wanted_scope
                && scope.map(|s| wire(&s)).as_deref() != Some(wanted)
            {
                return Ok(CmdOutput::new(
                    "your choice did not match --scope; nothing was decided",
                    &json!({ "decided": false }),
                ));
            }
            (DecideOutcome::Approve, scope)
        }
        (Choice::Deny { never: chose_never }, true) => (
            DecideOutcome::Deny,
            (never || chose_never).then_some(GrantScope::Never),
        ),
    };
    let result = send_decision(client, &view, outcome, scope, Surface::CliInteractive).await?;
    let text = format!("{}  decision recorded", view.approval.id);
    Ok(CmdOutput::new(text, &result))
}

async fn read_approval<C: Client + ?Sized>(
    client: &C,
    id: zeus_types::ApprovalId,
) -> ApiResult<zeus_api::methods::sessions::ApprovalView> {
    let value = client
        .call_raw("approvals.get", json!({"approval_id": id}))
        .await?;
    let request = value
        .get("approval")
        .ok_or_else(|| ApiError::internal("approval response is missing its request"))?;
    trust::validate_request(request)?;
    serde_json::from_value(request.clone())
        .map_err(|_| ApiError::internal("invalid approval from daemon"))
}

async fn send_decision<C: Client + ?Sized>(
    client: &C,
    view: &zeus_api::methods::sessions::ApprovalView,
    outcome: DecideOutcome,
    scope: Option<GrantScope>,
    surface: Surface,
) -> ApiResult<Value> {
    let hash = zeus_verify::canonical::request_hash(&view.facts)
        .map_err(|_| ApiError::internal("invalid approval facts"))?;
    let confirmation = if outcome == DecideOutcome::Deny {
        "gesture"
    } else {
        trust::confirmation(view, scope)?
    };
    let mut params = json!({"approval_id": view.approval.id, "request_hash": hash, "outcome": outcome, "channel": surface, "confirmation": confirmation});
    if let Some(scope) = scope {
        params["scope"] = json!(scope);
    }
    client.call_raw("approvals.decide", params).await
}

/// Review from the TUI using the same controlling-terminal trust surface as CLI approvals.
pub async fn review_approval<C: Client + ?Sized>(
    client: &C,
    id: zeus_types::ApprovalId,
) -> ApiResult<String> {
    let view = read_approval(client, id).await?;
    let mut terminal =
        trust::open_terminal().map_err(|e| ApiError::new(codes::HUMAN_REQUIRED, e.to_string()))?;
    let choice = trust::prompt(&view, &mut terminal)
        .map_err(|e| ApiError::new(codes::HUMAN_REQUIRED, e.to_string()))?;
    let (outcome, scope) = match choice {
        Choice::Later => return Ok("Left pending.".into()),
        Choice::Approve(scope) => (DecideOutcome::Approve, scope),
        Choice::Deny { never } => (DecideOutcome::Deny, never.then_some(GrantScope::Never)),
    };
    send_decision(client, &view, outcome, scope, Surface::CliInteractive).await?;
    Ok("Decision recorded.".into())
}

/// Run a command that needs no daemon connection.
pub async fn run_local(
    command: &Command,
    global: &GlobalArgs,
    paths: &ZeusPaths,
) -> ApiResult<CmdOutput> {
    match command {
        Command::Completions { shell } => {
            let mut buffer = Vec::new();
            let mut cmd = crate::cli::Cli::command();
            clap_complete::generate(*shell, &mut cmd, "zeus", &mut buffer);
            Ok(CmdOutput::new(
                String::from_utf8_lossy(&buffer).into_owned(),
                &json!({ "shell": shell.to_string() }),
            ))
        }
        Command::Config(ConfigCommand::Path) => {
            let text = format!(
                "config: {}\nstate:  {}\nsocket: {}",
                paths.config_file().display(),
                paths.state_dir.display(),
                paths.endpoint()
            );
            Ok(CmdOutput::new(
                text,
                &json!({ "config_file": paths.config_file(), "state_dir": paths.state_dir, "endpoint": paths.endpoint().to_string() }),
            ))
        }
        Command::Daemon(DaemonCommand::Status) => daemon_status_local(paths),
        Command::Daemon(DaemonCommand::Install) => daemon_run(paths, &["init"]),
        Command::Daemon(DaemonCommand::Start) => daemon_start(paths).await,
        Command::NativeHost(sub) => native_host(sub, global, paths),
        _ => Err(ApiError::internal("not a local command")),
    }
}

/// Which commands need the daemon.
pub fn needs_daemon(command: &Command) -> bool {
    !matches!(
        command,
        Command::Completions { .. }
            | Command::Config(ConfigCommand::Path)
            | Command::Daemon(DaemonCommand::Install | DaemonCommand::Start)
            | Command::NativeHost(_)
            | Command::Mcp { .. }
            | Command::A2a { .. }
            | Command::Tui
            | Command::Auth
    )
}

fn daemon_status_local(paths: &ZeusPaths) -> ApiResult<CmdOutput> {
    match DiscoveryRecord::read(&paths.discovery_file()) {
        Ok(Some(record)) => Ok(CmdOutput::new(
            format!(
                "discovery record present: zeusd {} pid {} (liveness is decided by connecting)",
                record.version, record.pid
            ),
            &record,
        )),
        Ok(None) => Ok(CmdOutput::new(
            "zeusd is not running (no discovery record)",
            &json!({ "running": false }),
        )
        .with_exit(Exit::Unavailable)),
        Err(e) => Err(ApiError::new(
            codes::UNAVAILABLE,
            format!("cannot read the discovery record: {e}"),
        )),
    }
}

fn zeusd_path() -> ApiResult<PathBuf> {
    let exe_name = if cfg!(windows) { "zeusd.exe" } else { "zeusd" };
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(exe_name)))
        .filter(|p| p.exists());
    beside
        .or_else(|| which::which("zeusd").ok())
        .ok_or_else(|| {
            ApiError::new(
                codes::UNAVAILABLE,
                "zeusd was not found next to zeus or on PATH",
            )
        })
}

async fn daemon_start(paths: &ZeusPaths) -> ApiResult<CmdOutput> {
    use crate::connect::{Identity, connect};
    if connect(paths, Identity::Cli).await.is_ok() {
        return Ok(CmdOutput::new(
            "daemon is already running",
            &json!({"running": true}),
        ));
    }
    // Installation is explicit: never silently create a new identity during a failed connection.
    crate::connect::load_secret(paths)?;
    crate::connect::load_credential(paths, Identity::Cli)?;
    let mut command = zeusd_command(paths)?;
    command
        .arg("run")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    }
    let mut child = command.spawn().map_err(|e| {
        ApiError::new(
            codes::UNAVAILABLE,
            format!("cannot start zeusd: {}", e.kind()),
        )
    })?;
    for _ in 0..50 {
        if connect(paths, Identity::Cli).await.is_ok() {
            return Ok(CmdOutput::new(
                "daemon started",
                &json!({"running":true,"pid":child.id()}),
            ));
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|e| ApiError::internal(e.to_string()))?
        {
            return Err(ApiError::new(
                codes::UNAVAILABLE,
                format!(
                    "zeusd stopped during startup ({status}); run zeusd run to see the diagnostic"
                ),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(ApiError::new(
        codes::UNAVAILABLE,
        "zeusd did not become ready in five seconds; run zeusd run to see the diagnostic",
    ))
}

/// A `zeusd` command bound to the same profile as this CLI, whatever `ZEUS_PROFILE` the shell holds.
fn zeusd_command(paths: &ZeusPaths) -> ApiResult<std::process::Command> {
    let mut command = std::process::Command::new(zeusd_path()?);
    command.env(
        "ZEUS_PROFILE",
        paths.profile.as_deref().unwrap_or("default"),
    );
    Ok(command)
}

fn daemon_run(paths: &ZeusPaths, args: &[&str]) -> ApiResult<CmdOutput> {
    let exe = zeusd_path()?;
    let output = zeusd_command(paths)?.args(args).output().map_err(|e| {
        ApiError::new(
            codes::UNAVAILABLE,
            format!("cannot run {}: {e}", exe.display()),
        )
    })?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let exit = if output.status.success() {
        Exit::Success
    } else {
        Exit::Internal
    };
    Ok(CmdOutput::new(text, &json!({ "exit_status": output.status.code() })).with_exit(exit))
}

fn browsers(selected: &[BrowserOpt]) -> Vec<Browser> {
    if selected.is_empty() {
        return Browser::ALL.to_vec();
    }
    selected
        .iter()
        .map(|b| match b {
            BrowserOpt::Chrome => Browser::Chrome,
            BrowserOpt::Edge => Browser::Edge,
            BrowserOpt::Brave => Browser::Brave,
            BrowserOpt::Chromium => Browser::Chromium,
            BrowserOpt::Firefox => Browser::Firefox,
        })
        .collect()
}

fn native_host(
    sub: &NativeHostCommand,
    global: &GlobalArgs,
    paths: &ZeusPaths,
) -> ApiResult<CmdOutput> {
    let home = directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or_else(|| ApiError::new(codes::UNAVAILABLE, "no home directory"))?;
    let host_exe = |explicit: &Option<String>| -> ApiResult<PathBuf> {
        match explicit {
            Some(path) => Ok(PathBuf::from(path)),
            None => {
                let name = if cfg!(windows) {
                    "zeus-native-host.exe"
                } else {
                    "zeus-native-host"
                };
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join(name)))
                    .ok_or_else(|| {
                        ApiError::new(codes::UNAVAILABLE, "cannot locate zeus-native-host")
                    })
            }
        }
    };
    match sub {
        NativeHostCommand::Install {
            browsers: selected,
            extension_ids,
            dev_extension_ids,
            host_path,
        } => {
            if extension_ids.is_empty() && dev_extension_ids.is_empty() {
                return Err(ApiError::invalid_params(
                    "give at least one --extension-id (or --dev-extension-id)",
                ));
            }
            let request = InstallRequest {
                browsers: browsers(selected),
                host_exe: host_exe(host_path)?,
                extension_ids: extension_ids.clone(),
                dev_extension_ids: dev_extension_ids.clone(),
                home,
                state_dir: paths.state_dir.clone(),
                os: Os::current(),
            };
            let steps =
                install::plan(&request).map_err(|e| ApiError::invalid_params(e.to_string()))?;
            let text = describe_steps(&steps);
            if !global.dry_run {
                install::apply(&steps, &mut SystemRegistrar)
                    .map_err(|e| ApiError::new(codes::UNAVAILABLE, e.to_string()))?;
            }
            Ok(CmdOutput::new(
                format!(
                    "{}{text}",
                    if global.dry_run {
                        "dry run, nothing changed:\n"
                    } else {
                        "installed:\n"
                    }
                ),
                &json!({ "dry_run": global.dry_run, "steps": steps.len() }),
            ))
        }
        NativeHostCommand::Uninstall { browsers: selected } => {
            let request = InstallRequest {
                browsers: browsers(selected),
                host_exe: host_exe(&None)?,
                extension_ids: vec![],
                dev_extension_ids: vec![],
                home,
                state_dir: paths.state_dir.clone(),
                os: Os::current(),
            };
            let steps = install::plan_uninstall(&request);
            let text = describe_steps(&steps);
            if !global.dry_run {
                install::apply(&steps, &mut SystemRegistrar)
                    .map_err(|e| ApiError::new(codes::UNAVAILABLE, e.to_string()))?;
            }
            Ok(CmdOutput::new(
                format!(
                    "{}{text}",
                    if global.dry_run {
                        "dry run, nothing changed:\n"
                    } else {
                        "removed:\n"
                    }
                ),
                &json!({ "dry_run": global.dry_run, "steps": steps.len() }),
            ))
        }
        NativeHostCommand::Status => {
            let mut lines = Vec::new();
            for browser in Browser::ALL {
                match install::manifest_location(browser, Os::current(), &home, &paths.state_dir) {
                    install::ManifestLocation::File(path) => lines.push(format!(
                        "{:<9} {}  {}",
                        browser.as_str(),
                        exists_mark(&path),
                        path.display()
                    )),
                    install::ManifestLocation::Registry { key, manifest_file } => {
                        lines.push(format!(
                            "{:<9} {}  HKCU\\{key} -> {}",
                            browser.as_str(),
                            exists_mark(&manifest_file),
                            manifest_file.display()
                        ));
                    }
                }
            }
            Ok(CmdOutput::new(
                lines.join("\n"),
                &json!({ "browsers": Browser::ALL.iter().map(|b| b.as_str()).collect::<Vec<_>>() }),
            ))
        }
    }
}

fn exists_mark(path: &Path) -> &'static str {
    if path.exists() { "[x]" } else { "[ ]" }
}

fn describe_steps(steps: &[install::Step]) -> String {
    steps
        .iter()
        .map(|s| match s {
            install::Step::WriteFile { path, .. } => format!("  write   {}", path.display()),
            install::Step::SetRegistryDefault { key, value } => {
                format!("  set     HKCU\\{key} = {value}")
            }
            install::Step::RemoveFile { path } => format!("  remove  {}", path.display()),
            install::Step::DeleteRegistryKey { key } => format!("  delete  HKCU\\{key}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The stderr text and exit status for a failed command.
pub fn failure(error: &ApiError) -> (String, Exit) {
    let hint = match error.class() {
        ErrorClass::Unavailable => {
            Some("is the daemon running? try `zeus daemon status` or `zeus doctor`")
        }
        ErrorClass::Unauthenticated => {
            Some("this needs a person at a terminal or the browser extension")
        }
        _ => None,
    };
    (
        error_line(
            error.envelope().code.as_str(),
            &error.envelope().message,
            hint,
        ),
        Exit::for_class(error.class()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use clap::Parser;
    use tokio::sync::broadcast;
    use zeus_api::ClientRole;
    use zeus_api::jsonrpc::Notification;
    use zeus_api::kind::ClientKind;
    use zeus_api::methods::daemon::{AuthenticateResult, ConnectionLimits};

    use crate::cli::Cli;

    struct Fake {
        calls: Mutex<Vec<(String, Value)>>,
        answers: Mutex<std::collections::HashMap<String, Vec<ApiResult<Value>>>>,
        session: AuthenticateResult,
        tx: broadcast::Sender<Notification>,
    }

    impl Fake {
        fn new() -> Self {
            let (tx, _) = broadcast::channel(4);
            Self {
                calls: Mutex::default(),
                answers: Mutex::default(),
                session: AuthenticateResult {
                    client_id: "cli_x".into(),
                    kind: ClientKind::Admin,
                    credential_kind: ClientKind::Admin,
                    downgraded: false,
                    role: ClientRole::Cli,
                    permissions: vec![],
                    limits: ConnectionLimits::STANDARD,
                    connection_id: zeus_types::IdGenerator::new().connection_id(),
                },
                tx,
            }
        }

        fn answer(&self, method: &str, result: ApiResult<Value>) {
            self.answers
                .lock()
                .unwrap()
                .entry(method.into())
                .or_default()
                .push(result);
        }

        fn calls_to(&self, method: &str) -> Vec<Value> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(m, _)| m == method)
                .map(|(_, p)| p.clone())
                .collect()
        }
    }

    #[async_trait]
    impl Client for Fake {
        async fn call_raw(&self, method: &str, params: Value) -> ApiResult<Value> {
            self.calls.lock().unwrap().push((method.into(), params));
            let mut answers = self.answers.lock().unwrap();
            match answers.get_mut(method) {
                Some(list) if list.len() > 1 => list.remove(0),
                Some(list) if list.len() == 1 => list[0].clone(),
                _ => Err(ApiError::not_implemented("fake")),
            }
        }

        fn notifications(&self) -> broadcast::Receiver<Notification> {
            self.tx.subscribe()
        }

        fn session(&self) -> &AuthenticateResult {
            &self.session
        }
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("zeus").chain(args.iter().copied())).unwrap()
    }

    #[tokio::test]
    async fn artifact_commands_preserve_paging_and_retry_keys_and_dry_run_does_not_ingest() {
        let fake = Fake::new();
        fake.answer(
            "artifacts.list",
            Ok(json!({"items":[],"next_cursor":"next-page"})),
        );
        let cli = parse(&[
            "artifacts",
            "ls",
            "--limit",
            "7",
            "--cursor",
            "old-page",
            "--state",
            "active",
        ]);
        let output = run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap();
        assert!(output.text.contains("next-page"));
        assert_eq!(fake.calls_to("artifacts.list")[0]["cursor"], "old-page");
        assert_eq!(fake.calls_to("artifacts.list")[0]["state"], "active");
        fake.answer(
            "artifacts.ingest",
            Ok(json!({
                "artifact":{"name":"data"},"created":true,"replayed":false
            })),
        );
        let cli = parse(&[
            "artifacts",
            "ingest",
            "/root/data",
            "--idempotency-key",
            "ingest-one",
        ]);
        let mut global = cli.global.clone();
        global.dry_run = true;
        assert!(
            run_remote(&cli.command, &global, &fake, false, None)
                .await
                .is_err()
        );
        assert!(fake.calls_to("artifacts.ingest").is_empty());
        run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap();
        assert_eq!(
            fake.calls_to("artifacts.ingest")[0]["idempotency_key"],
            "ingest-one"
        );
        assert!(fake.calls_to("approvals.decide").is_empty());
    }

    #[tokio::test]
    async fn artifact_lifecycle_commands_preview_first_and_apply_exact_saved_receipts() {
        let fake = Fake::new();
        let id = zeus_types::IdGenerator::new().artifact_id().to_string();
        let preview = json!({"plan":{
            "version":1,"artifact_id":id,"expected_record_blake3":"a".repeat(64),
            "blake3":"b".repeat(64),"sha256":"c".repeat(64),"size_bytes":42,"quarantined":false,
            "current_retention":"evictable","current_retain_until":null,
            "operation":{"kind":"materialize","path":"/downloads/new.txt"},"requires_approval":false,
            "expires_at":"2026-10-21T12:00:00.000Z"},"plan_hash":"d".repeat(64)});
        fake.answer("artifacts.lifecycle.preview", Ok(preview.clone()));
        let cli = parse(&["artifacts", "materialize", &id, "/downloads/new.txt"]);
        run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap();
        assert_eq!(
            fake.calls_to("artifacts.lifecycle.preview")[0]["operation"],
            preview["plan"]["operation"]
        );
        assert!(fake.calls_to("artifacts.lifecycle.apply").is_empty());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("preview.json");
        std::fs::write(&file, serde_json::to_vec(&preview).unwrap()).unwrap();
        let filename = file.display().to_string();
        let cli = parse(&[
            "artifacts",
            "apply",
            &filename,
            "--idempotency-key",
            "copy-key",
        ]);
        let mut dry = cli.global.clone();
        dry.dry_run = true;
        run_remote(&cli.command, &dry, &fake, false, None)
            .await
            .unwrap();
        assert!(fake.calls_to("artifacts.lifecycle.apply").is_empty());
        fake.answer("artifacts.lifecycle.apply", Ok(json!({"applied":true,"artifact":{"id":id},"path":"/downloads/new.txt","mode":"copy","replayed":false})));
        run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap();
        let applied = &fake.calls_to("artifacts.lifecycle.apply")[0];
        assert_eq!(applied["plan"], preview["plan"]);
        assert_eq!(applied["plan_hash"], preview["plan_hash"]);
        assert_eq!(applied["idempotency_key"], "copy-key");
        assert!(fake.calls_to("approvals.decide").is_empty());
        std::fs::write(&file, b"ZEUS-CANARY-invalid-preview").unwrap();
        let error = run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("ZEUS-CANARY-"));
        assert_eq!(fake.calls_to("artifacts.lifecycle.apply").len(), 1);
    }

    #[tokio::test]
    async fn input_files_preserve_exact_signed_urls_and_validate_the_whole_batch_before_rpc() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("urls.txt");
        let signed = "https://example.org/data?signature=ZEUS-CANARY-query";
        std::fs::write(
            &path,
            format!("\u{feff}# comment\r\n{signed}\r\n\n{signed}\nhttps://example.org/other\n"),
        )
        .unwrap();
        let cli = parse(&["get", "--input-file", path.to_str().unwrap()]);
        let Command::Get(args) = &cli.command else {
            panic!("expected get");
        };
        let urls = get_inputs(args).unwrap();
        assert_eq!(urls, vec![signed, "https://example.org/other"]);
        let fake = Fake::new();
        // A later invalid URL refuses the entire request, including earlier valid lines.
        std::fs::write(&path, format!("{signed}\nnot-a-url-ZEUS-CANARY-invalid")).unwrap();
        let error = run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap_err();
        assert!(!error.envelope().message.contains("ZEUS-CANARY-"));
        assert!(fake.calls_to("jobs.enqueue").is_empty());
        std::fs::write(&path, "x".repeat(1024 * 1024 + 1)).unwrap();
        assert!(get_inputs(args).is_err());
        std::fs::write(
            &path,
            (0..51)
                .map(|i| format!("https://example.org/{i}\n"))
                .collect::<String>(),
        )
        .unwrap();
        assert!(get_inputs(args).is_err());
    }

    #[tokio::test]
    async fn cloud_submit_dispatch_preserves_plan_hash_and_dry_run_never_mutates() {
        let client = Fake::new();
        let hash = "a".repeat(64);
        let cli = parse(&[
            "cloud",
            "submit",
            "clp_example",
            "--plan-hash",
            &hash,
            "--idempotency-key",
            "retry-key",
        ]);
        assert!(
            run_remote(&cli.command, &cli.global, &client, false, None)
                .await
                .is_err()
        );
        let calls = client.calls_to("cloud.submit");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["plan_hash"], hash);
        assert_eq!(calls[0]["idempotency_key"], "retry-key");
        assert!(client.calls_to("approvals.decide").is_empty());
        let mut global = cli.global;
        global.dry_run = true;
        assert!(
            run_remote(&cli.command, &global, &client, false, None)
                .await
                .is_err()
        );
        assert_eq!(client.calls_to("cloud.submit").len(), 1);
    }

    #[tokio::test]
    async fn workflow_commands_send_the_plan_file_and_hash_and_never_decide() {
        let client = Fake::new();
        let dir = std::env::temp_dir().join(format!("zeus-cli-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("plan.json");
        std::fs::write(
            &file,
            serde_json::json!({"plan_version":0,"name":"papers","allowed_domains":["a.example"],
                "sources":[{"type":"start_urls","urls":["https://a.example/list"]}],
                "extract":{"items":{"kind":"css","query":"a[href]"},"fields":{"url":{"extract":{"kind":"attr","name":"href"}}}},
                "fetch":{"min_interval_ms":1000,"max_concurrency_per_host":1},
                "budgets":{"max_pages":2,"max_files":5,"max_bytes":1000000,"max_duration_ms":60000,"max_depth":1},
                "sinks":{"dest_dir":"papers"}})
            .to_string(),
        )
        .unwrap();
        let plan_id = "plan_01M45NHJ2AHWMXT8JVJ7BEEJX7";
        let hash = "b".repeat(64);
        client.answer(
            "workflows.plan",
            Ok(serde_json::json!({"plan_id":plan_id,"samples":[],"plan_schema":{},"diagnostics":[],"plan_hash":hash})),
        );
        let cli = parse(&[
            "workflow",
            "plan",
            "--goal",
            "papers",
            "--url",
            "https://a.example/list",
            "--candidate",
            file.to_str().unwrap(),
        ]);
        let output = run_remote(&cli.command, &cli.global, &client, false, None)
            .await
            .unwrap();
        assert!(output.text.contains(&hash));
        let sent = client.calls_to("workflows.plan");
        assert_eq!(sent[0]["candidate"]["name"], "papers");
        assert_eq!(sent[0]["start_urls"][0], "https://a.example/list");
        client.answer(
            "workflows.run",
            Ok(serde_json::json!({"run_id":"run_01M45NHJ2AHWMXT8JVJ7BEEJX7","status":"awaiting_approval",
                "approval_id":"ap_01M45NHJ2AHWMXT8JVJ7BEEJX7"})),
        );
        let run = parse(&["workflow", "run", plan_id, "--plan-hash", &hash]);
        let output = run_remote(&run.command, &run.global, &client, false, None)
            .await
            .unwrap();
        assert!(output.text.contains("zeus approvals approve ap_"));
        assert_eq!(client.calls_to("workflows.run")[0]["plan_hash"], hash);
        assert!(client.calls_to("approvals.decide").is_empty());
        let mut global = run.global;
        global.dry_run = true;
        assert!(
            run_remote(&run.command, &global, &client, false, None)
                .await
                .is_err()
        );
        assert_eq!(client.calls_to("workflows.run").len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn clients_add_names_the_token_file_and_never_prints_a_token() {
        let client = Fake::new();
        client.answer(
            "clients.add",
            Ok(serde_json::json!({"client":{"credential_id":"cli_01M45NHJ2AHWMXT8JVJ7BEEJX7","name":"ogun",
                "kind":"service","role":"service","permissions":["jobs.read"],"created_at":"2026-10-18T00:00:00.000Z",
                "last_used_at":null,"revoked":false,"revoked_at":null},
                "credential_file":"/state/credentials/ogun.token","replayed":false})),
        );
        let cli = parse(&["clients", "add", "--name", "ogun", "--kind", "service"]);
        let output = run_remote(&cli.command, &cli.global, &client, false, None)
            .await
            .unwrap();
        assert!(output.text.contains("/state/credentials/ogun.token"));
        assert!(!output.text.contains("zt1_"));
        let sent = client.calls_to("clients.add");
        assert_eq!(sent[0]["kind"], "service");
        assert_eq!(sent[0]["name"], "ogun");
    }

    #[tokio::test]
    async fn cloud_account_setup_refuses_agent_connection_before_collecting_input() {
        let mut client = Fake::new();
        client.session.kind = ClientKind::Agent;
        client.session.downgraded = true;
        let cli = parse(&["cloud", "accounts", "configure"]);
        let error = run_remote(&cli.command, &cli.global, &client, false, None)
            .await
            .unwrap_err();
        assert_eq!(error.envelope().code.to_string(), "api.human_required");
        assert!(client.calls.lock().unwrap().is_empty());
    }

    const JOB: &str = "job_01M45NHJ2AHWMXT8JVJ7BEEJX7";

    fn summary(state: &str, error: Option<Value>) -> Value {
        json!({
            "id": JOB, "state": state, "quarantined": false, "source_kind": "cli",
            "url_redacted": "https://files.example.com/a.zip", "bytes_done": 10, "bytes_total": 10,
            "last_error": error, "created_at": "2026-10-05T09:15:30.250Z", "updated_at": "2026-10-05T09:15:30.250Z"
        })
    }

    #[test]
    fn headers_are_allow_listed_and_cookies_are_refused() {
        assert_eq!(
            parse_header("Accept: text/csv").unwrap(),
            ("Accept".to_string(), "text/csv".to_string())
        );
        assert!(parse_header("accept-language: en").is_ok());
        for bad in [
            "Cookie: sid=ZEUS-CANARY",
            "Authorization: Bearer x",
            "Host: evil",
            "Range: bytes=0-1",
            "nocolon",
            "Accept:",
            "Accept: a\nb",
        ] {
            let error = parse_header(bad).unwrap_err();
            assert!(!error.envelope().message.contains("ZEUS-CANARY"), "{bad}");
        }
    }

    #[tokio::test]
    async fn get_sends_a_stable_idempotency_key_and_no_source() {
        let fake = Fake::new();
        fake.answer("jobs.enqueue", Ok(json!({ "dry_run": false, "jobs": [{ "job_id": JOB, "state": "queued", "deduplicated": false }] })));
        let cli = parse(&[
            "get",
            "-o",
            "papers",
            "-H",
            "Accept: application/pdf",
            "https://files.example.com/a.zip",
        ]);
        let output = run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap();
        assert!(output.text.contains(JOB));
        run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap();
        let calls = fake.calls_to("jobs.enqueue");
        assert_eq!(
            calls[0]["idempotency_key"], calls[1]["idempotency_key"],
            "the same command is the same intent"
        );
        assert_eq!(calls[0]["jobs"][0]["dest_dir"], "papers");
        assert_eq!(
            calls[0]["jobs"][0]["extra_headers"]["Accept"],
            "application/pdf"
        );
        assert!(calls[0]["jobs"][0].get("source").is_none());
    }

    #[tokio::test]
    async fn cookie_from_extension_asks_for_a_grant_for_one_site_only() {
        let fake = Fake::new();
        fake.answer("jobs.enqueue", Ok(json!({ "dry_run": false, "jobs": [{ "job_id": JOB, "state": "awaiting_approval", "approval_id": "ap_01M45NHJ3XS0QXG9T42HHN2YR8", "deduplicated": false }] })));
        let cli = parse(&[
            "get",
            "--cookie-from-extension",
            "https://portal.example.org/d/1",
        ]);
        let output = run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap();
        assert_eq!(
            fake.calls_to("jobs.enqueue")[0]["jobs"][0]["session_origin"],
            "https://portal.example.org"
        );
        assert!(output.text.contains("waiting for your approval"));
        let mixed = parse(&[
            "get",
            "--cookie-from-extension",
            "https://a.example/1",
            "https://b.example/2",
        ]);
        assert!(
            run_remote(&mixed.command, &mixed.global, &fake, false, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn wait_maps_job_outcomes_to_exit_codes() {
        let failed = summary(
            "failed",
            Some(
                json!({ "code": "http.status_429", "class": "throttled", "message": "slow down", "retryable": true }),
            ),
        );
        let fake = Fake::new();
        fake.answer(
            "jobs.wait",
            Ok(json!({ "jobs": [failed], "timed_out": false, "head_seq": 1 })),
        );
        let cli = parse(&["wait", JOB]);
        let output = run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap();
        assert_eq!(output.exit.code(), 11);

        let done = Fake::new();
        done.answer(
            "jobs.wait",
            Ok(json!({ "jobs": [summary("completed", None)], "timed_out": false, "head_seq": 1 })),
        );
        assert_eq!(
            run_remote(&cli.command, &cli.global, &done, false, None)
                .await
                .unwrap()
                .exit,
            Exit::Success
        );

        let waiting = Fake::new();
        waiting.answer("jobs.wait", Ok(json!({ "jobs": [summary("awaiting_approval", None)], "timed_out": false, "head_seq": 1 })));
        let output = run_remote(&cli.command, &cli.global, &waiting, false, None)
            .await
            .unwrap();
        assert_eq!(output.exit, Exit::Timeout);
        assert!(output.text.contains("zeus approvals list"));
    }

    #[tokio::test]
    async fn approvals_need_a_terminal_and_yes_does_not_help() {
        let fake = Fake::new();
        let cli = parse(&[
            "--yes",
            "approvals",
            "approve",
            "ap_01M45NHJ3XS0QXG9T42HHN2YR8",
        ]);
        assert!(cli.global.yes);
        let error = run_remote(&cli.command, &cli.global, &fake, false, None)
            .await
            .unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::HUMAN_REQUIRED);
        assert!(
            fake.calls.lock().unwrap().is_empty(),
            "no daemon call may happen without a terminal"
        );
    }

    #[tokio::test]
    async fn an_approval_is_decided_with_the_hash_the_person_saw() {
        let fake = Fake::new();
        let view = trust::test_request();
        fake.answer("approvals.get", Ok(json!({"approval":view.clone()})));
        let mut decided = view.clone();
        decided["state"] = json!("approved");
        fake.answer("approvals.decide", Ok(json!({ "approval": decided })));
        let mut terminal = Terminal {
            input: Box::new(std::io::Cursor::new(b"2\n".to_vec())),
            output: Box::new(Vec::new()),
            physical_input: None,
        };
        let cli = parse(&["approvals", "approve", "ap_01M45NHJ3XS0QXG9T42HHN2YR8"]);
        let output = run_remote(&cli.command, &cli.global, &fake, false, Some(&mut terminal))
            .await
            .unwrap();
        assert!(output.text.contains("decision recorded"));
        let sent = &fake.calls_to("approvals.decide")[0];
        assert_eq!(sent["request_hash"], view["request_hash"]);
        assert_eq!(sent["scope"], "site");
        assert_eq!(sent["channel"], "cli_interactive");
    }

    #[tokio::test]
    async fn a_mismatched_choice_decides_nothing() {
        let fake = Fake::new();
        let view = trust::test_request();
        fake.answer("approvals.get", Ok(json!({"approval":view})));
        let mut terminal = Terminal {
            input: Box::new(std::io::Cursor::new(b"n\n".to_vec())),
            output: Box::new(Vec::new()),
            physical_input: None,
        };
        let cli = parse(&["approvals", "approve", "ap_01M45NHJ3XS0QXG9T42HHN2YR8"]);
        let output = run_remote(&cli.command, &cli.global, &fake, false, Some(&mut terminal))
            .await
            .unwrap();
        assert!(output.text.contains("nothing was decided"));
        assert!(fake.calls_to("approvals.decide").is_empty());
    }

    #[test]
    fn exit_codes_for_job_sets() {
        let job = |state: &str, class: Option<&str>| -> JobSummary {
            serde_json::from_value(summary(
                state,
                class.map(|c| json!({ "code": "http.status_403", "class": c, "message": "m", "retryable": false })),
            ))
            .unwrap()
        };
        assert_eq!(exit_for_jobs(&[job("completed", None)]), Exit::Success);
        assert_eq!(exit_for_jobs(&[job("downloading", None)]), Exit::Timeout);
        assert_eq!(
            exit_for_jobs(&[job("completed", None), job("failed", Some("blocked"))]).code(),
            14
        );
        assert_eq!(exit_for_jobs(&[job("canceled", None)]), Exit::Conflict);
    }

    #[tokio::test]
    async fn local_commands_need_no_daemon_and_completions_work() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ZeusPaths {
            state_dir: dir.path().join("s"),
            config_dir: dir.path().join("c"),
            runtime_dir: dir.path().join("r"),
            user_identity: "t".into(),
            profile: None,
        };
        let cli = parse(&["completions", "bash"]);
        assert!(!needs_daemon(&cli.command));
        let output = run_local(&cli.command, &cli.global, &paths).await.unwrap();
        assert!(output.text.contains("zeus"));
        let cli = parse(&["config", "path"]);
        assert!(
            run_local(&cli.command, &cli.global, &paths)
                .await
                .unwrap()
                .text
                .contains("config.toml")
        );
        assert!(
            needs_daemon(&parse(&["ls"]).command)
                && needs_daemon(&parse(&["approvals", "list"]).command)
        );
    }

    #[tokio::test]
    async fn native_host_install_dry_run_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ZeusPaths {
            state_dir: dir.path().join("s"),
            config_dir: dir.path().join("c"),
            runtime_dir: dir.path().join("r"),
            user_identity: "t".into(),
            profile: None,
        };
        let host = dir.path().join("zeus-native-host");
        let cli = parse(&[
            "--dry-run",
            "native-host",
            "install",
            "--browser",
            "chrome",
            "--extension-id",
            "abcdefghijklmnopabcdefghijklmnop",
            "--host-path",
            host.to_str().unwrap(),
        ]);
        let output = run_local(&cli.command, &cli.global, &paths).await.unwrap();
        assert!(output.text.starts_with("dry run"));
        assert!(!dir.path().join("allowed-extensions.json").exists());
        let none = parse(&["native-host", "install"]);
        assert!(
            run_local(&none.command, &none.global, &paths)
                .await
                .is_err()
        );
    }

    #[test]
    fn failures_render_with_hints_and_the_right_status() {
        let (text, exit) = failure(&ApiError::new(
            codes::UNAVAILABLE,
            "cannot reach the daemon",
        ));
        assert_eq!(exit, Exit::Unavailable);
        assert!(text.contains("hint: is the daemon running"));
        let (_, exit) = failure(&ApiError::invalid_params("bad"));
        assert_eq!(exit, Exit::Usage);
    }
}
