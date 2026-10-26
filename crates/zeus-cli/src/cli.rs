//! The command tree (ARCHITECTURE section 11).

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

/// Zeus: the session-aware download and file layer for AI agents.
#[derive(Debug, Parser)]
#[command(
    name = "zeus",
    version,
    about,
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Flags shared by every command.
    #[command(flatten)]
    pub global: GlobalArgs,
    /// The command.
    #[command(subcommand)]
    pub command: Command,
}

/// Global flags. Data goes to stdout, diagnostics and progress to stderr; `NO_COLOR` is honoured.
#[derive(Debug, Clone, Default, Args)]
pub struct GlobalArgs {
    /// Print one JSON document on stdout instead of text.
    #[arg(long, global = true, conflicts_with = "ndjson")]
    pub json: bool,
    /// Print newline-delimited JSON (one object per line), for lists and event streams.
    #[arg(long, global = true)]
    pub ndjson: bool,
    /// Print nothing except errors (and, with --json, the document).
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Never use colour (also set by the NO_COLOR environment variable).
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Show what would happen without doing it, where the command supports it.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Assume yes for confirmations that are safe to skip. **Never** applies to approvals: those always need a person
    /// at the terminal.
    #[arg(long, global = true)]
    pub yes: bool,
    /// Configuration profile.
    #[arg(long, global = true, env = "ZEUS_PROFILE")]
    pub profile: Option<String>,
}

/// Priority of a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PriorityOpt {
    /// Background.
    Low,
    /// Default.
    Normal,
    /// Soon.
    High,
}

/// Job states for `zeus ls --state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StateOpt {
    /// Waiting for a slot.
    Queued,
    /// Being probed.
    Probing,
    /// Waiting for a person.
    AwaitingApproval,
    /// Waiting for the browser.
    AwaitingSession,
    /// Transferring.
    Downloading,
    /// Paused.
    Paused,
    /// Waiting to retry.
    RetryScheduled,
    /// Being verified.
    Verifying,
    /// Being finalized.
    Finalizing,
    /// Done.
    Completed,
    /// Failed.
    Failed,
    /// Canceled.
    Canceled,
}

/// `zeus get`.
#[derive(Debug, Clone, Args)]
pub struct GetArgs {
    /// One or more http(s) URLs.
    #[arg(required_unless_present = "input_file", num_args = 1..)]
    pub urls: Vec<String>,
    /// Read one HTTP(S) URL per line (UTF-8, at most 1 MiB). Blank lines and # comments are ignored.
    /// Combined inputs allow at most 50 unique URLs; exact duplicates are removed in input order.
    #[arg(short = 'i', long)]
    pub input_file: Option<PathBuf>,
    /// Destination directory inside a write root (curl-style `-o`; a file name is not accepted: names come from the
    /// server and are sanitised).
    #[arg(short = 'o', long = "output")]
    pub output: Option<String>,
    /// Extra request header `Name: value`. Only Accept, Accept-Language, If-Modified-Since and X-Requested-With are
    /// allowed; Cookie and Authorization are refused (use --cookie-from-extension).
    #[arg(short = 'H', long = "header")]
    pub headers: Vec<String>,
    /// Ask for the user's logged-in browser session for the URL's site, through the extension. This creates a
    /// request that a person must approve; Zeus never reads cookie files.
    #[arg(long)]
    pub cookie_from_extension: bool,
    /// Wait until the downloads finish and exit with their status.
    #[arg(short, long)]
    pub wait: bool,
    /// Expected SHA-256 (single URL only).
    #[arg(long)]
    pub sha256: Option<String>,
    /// Priority.
    #[arg(long, value_enum)]
    pub priority: Option<PriorityOpt>,
    /// Idempotency key (default: derived from the URLs).
    #[arg(long)]
    pub idempotency_key: Option<String>,
}

/// `zeus approvals`.
#[derive(Debug, Clone, Subcommand)]
pub enum ApprovalsCommand {
    /// List approvals waiting for you.
    List,
    /// Review and approve a request. Needs a terminal; `--yes` is ignored.
    Approve {
        /// The approval id (`ap_...`).
        id: String,
        /// Scope to grant: once, site, workflow (the options depend on the request).
        #[arg(long)]
        scope: Option<String>,
    },
    /// Review and deny a request. Needs a terminal.
    Deny {
        /// The approval id.
        id: String,
        /// Also block this site from now on.
        #[arg(long)]
        never: bool,
    },
}

/// `zeus sessions`.
#[derive(Debug, Clone, Subcommand)]
pub enum SessionsCommand {
    /// List session permissions you have granted.
    List,
    /// Revoke a grant now.
    Revoke {
        /// The grant id (`gr_...`).
        id: String,
    },
}

/// Kind of credential `zeus clients add` creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ClientKindOpt {
    /// An agent or bridge (the agent ceiling).
    Agent,
    /// A first-party unattended program such as Ogun (agent ceiling plus its own roots).
    Service,
}

/// Role of a credential `zeus clients add` creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ClientRoleOpt {
    /// An MCP server or bridge.
    Mcp,
    /// An A2A bridge.
    A2a,
    /// An application embedding a Zeus client.
    Embedded,
    /// A service program (kind `service` only).
    Service,
}

/// `zeus clients`: credentials for agents and first-party services. Admin only, from a physical terminal.
#[derive(Debug, Clone, Subcommand)]
pub enum ClientsCommand {
    /// List client credentials. Tokens and verifiers are never shown.
    List {
        /// Also list revoked credentials.
        #[arg(long)]
        include_revoked: bool,
    },
    /// Create an agent or service credential and print the path of its owner-only token file.
    Add {
        /// Name: lowercase letters, digits, - and _, starting with a letter (becomes `<name>.token`).
        #[arg(long)]
        name: String,
        /// Kind of credential.
        #[arg(long, value_enum, default_value = "agent")]
        kind: ClientKindOpt,
        /// Role; defaults to `service` for services and `mcp` for agents.
        #[arg(long, value_enum)]
        role: Option<ClientRoleOpt>,
        /// Narrow the permissions to these (repeatable), for example `jobs.read`.
        #[arg(long = "permission")]
        permissions: Vec<String>,
        /// Make a retried command safe: the same key returns the first result.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Revoke a credential, close its connections and withdraw its pending requests.
    Revoke {
        /// The credential id (`cli_...`).
        credential_id: String,
    },
}

/// `zeus audit`.
#[derive(Debug, Clone, Subcommand)]
pub enum AuditCommand {
    /// Show the newest audit records.
    Tail {
        /// How many (1 to 100).
        #[arg(short, long, default_value_t = 20)]
        limit: u32,
    },
    /// Verify the audit hash chain.
    Verify,
}

/// `zeus daemon`.
#[derive(Debug, Clone, Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon in the background.
    Start,
    /// Stop the daemon.
    Stop,
    /// Show whether the daemon runs and what it is doing.
    Status,
    /// Stop all work and revoke active session grants.
    KillSwitch,
    /// Release the stop latch; jobs stay paused and permissions stay revoked.
    Release,
    /// Create first-run state (secrets and credentials).
    Install,
}

/// Browsers for host registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BrowserOpt {
    /// Google Chrome.
    Chrome,
    /// Microsoft Edge.
    Edge,
    /// Brave.
    Brave,
    /// Chromium.
    Chromium,
    /// Mozilla Firefox.
    Firefox,
}

/// `zeus native-host`.
#[derive(Debug, Clone, Subcommand)]
pub enum NativeHostCommand {
    /// Register the native-messaging host with browsers (per user).
    Install {
        /// Browsers to register with. Default: all.
        #[arg(long = "browser", value_enum)]
        browsers: Vec<BrowserOpt>,
        /// Production extension id to pin (repeatable). Exact ids only.
        #[arg(long = "extension-id")]
        extension_ids: Vec<String>,
        /// Development extension id to pin (repeatable). The host logs a warning on every start.
        #[arg(long = "dev-extension-id")]
        dev_extension_ids: Vec<String>,
        /// Path of the zeus-native-host executable. Default: next to this program.
        #[arg(long)]
        host_path: Option<String>,
    },
    /// Remove the registrations.
    Uninstall {
        /// Browsers to unregister. Default: all.
        #[arg(long = "browser", value_enum)]
        browsers: Vec<BrowserOpt>,
    },
    /// Show where the host is registered and whether the files exist.
    Status,
}

/// `zeus config`.
#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCommand {
    /// Print a value, or all values.
    Get {
        /// Dotted key.
        key: Option<String>,
    },
    /// Set a value.
    Set {
        /// Dotted key.
        key: String,
        /// New value (JSON, or a bare string).
        value: String,
    },
    /// Print the configuration and state directories.
    Path,
}

/// The commands.
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Plan and manage explicitly approved cloud deliveries.
    #[command(subcommand)]
    Cloud(crate::cloud::CloudCommand),
    /// Sample pages, validate a plan, dry-run it and start an approval-bound workflow run.
    #[command(subcommand)]
    Workflow(crate::workflow::WorkflowCommand),
    /// List files, organise a directory from rules (approval-bound) or undo an operation.
    #[command(subcommand)]
    Files(crate::files::FilesCommand),
    /// Inspect artifact records and provenance, or register a local file.
    #[command(subcommand)]
    Artifacts(crate::artifacts::ArtifactsCommand),
    /// Download queues: list them, start or stop one, set the speed limit.
    #[command(subcommand)]
    Queues(crate::queues::QueuesCommand),
    /// Download one or more URLs.
    Get(GetArgs),
    /// Check a URL: size, resumability, type, whether a session is needed.
    Probe {
        /// The URL.
        url: String,
    },
    /// List jobs.
    Ls {
        /// Only these states.
        #[arg(long, value_enum)]
        state: Vec<StateOpt>,
        /// How many (1 to 100).
        #[arg(short, long, default_value_t = 20)]
        limit: u32,
    },
    /// Show one job, or the daemon status when no job is given.
    Status {
        /// The job id (`job_...`).
        job: Option<String>,
    },
    /// Wait for jobs to finish.
    Wait {
        /// Job ids.
        #[arg(required = true)]
        jobs: Vec<String>,
        /// Give up after this many seconds (exit status 7; the job keeps running).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Pause a job.
    Pause {
        /// The job id.
        job: String,
    },
    /// Resume a paused job.
    Resume {
        /// The job id.
        job: String,
    },
    /// Cancel a job and delete its partial data.
    Cancel {
        /// The job id.
        job: String,
    },
    /// Retry a failed job.
    Retry {
        /// The job id.
        job: String,
    },
    /// Re-hash a finished download.
    Verify {
        /// The job id.
        job: String,
        /// Compare with this SHA-256.
        #[arg(long)]
        sha256: Option<String>,
    },
    /// Review approval requests (needs a terminal to decide).
    #[command(subcommand)]
    Approvals(ApprovalsCommand),
    /// Manage session permissions.
    #[command(subcommand)]
    Sessions(SessionsCommand),
    /// Manage agent and service credentials (admin, physical terminal).
    #[command(subcommand)]
    Clients(ClientsCommand),
    /// Inspect the audit log.
    #[command(subcommand)]
    Audit(AuditCommand),
    /// Control the daemon.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Run the MCP server on stdio for an agent.
    Mcp {
        /// Serve Streamable HTTP on 127.0.0.1 instead (needs the `http` feature; off by default).
        #[arg(long)]
        http: bool,
        /// Port for --http.
        #[arg(long, default_value_t = 4747)]
        port: u16,
    },
    /// Run the optional authenticated A2A bridge on loopback.
    A2a {
        /// Dedicated JSON peer-token file; never use an IPC credential as a bearer token.
        #[arg(long)]
        token_file: PathBuf,
        /// Loopback port (the daemon itself never listens on TCP).
        #[arg(long, default_value_t = 4750)]
        port: u16,
    },
    /// Install or remove the browser integration.
    #[command(name = "native-host", subcommand)]
    NativeHost(NativeHostCommand),
    /// Read or change configuration.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Set up model providers (optional; nothing is sent to a model unless you do).
    Auth,
    /// Check that everything is set up.
    Doctor,
    /// Open the terminal UI.
    Tui,
    /// Print a shell completion script.
    Completions {
        /// The shell.
        shell: Shell,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("zeus").chain(args.iter().copied()))
    }

    #[test]
    fn the_clap_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_documented_command_parses() {
        for args in [
            vec!["get", "https://a.example/x.zip"],
            vec![
                "get",
                "-o",
                "papers",
                "--wait",
                "https://a/1",
                "https://a/2",
            ],
            vec!["probe", "https://a.example/x"],
            vec!["ls", "--state", "downloading", "--state", "failed"],
            vec!["status"],
            vec!["status", "job_01M45NHJ2AHWMXT8JVJ7BEEJX7"],
            vec!["wait", "job_x", "--timeout", "30"],
            vec!["pause", "job_x"],
            vec!["resume", "job_x"],
            vec!["cancel", "job_x"],
            vec!["retry", "job_x"],
            vec!["verify", "job_x", "--sha256", "ab"],
            vec!["approvals", "list"],
            vec!["approvals", "approve", "ap_x", "--scope", "site"],
            vec!["approvals", "deny", "ap_x", "--never"],
            vec!["sessions", "list"],
            vec!["sessions", "revoke", "gr_x"],
            vec!["audit", "tail", "--limit", "5"],
            vec!["audit", "verify"],
            vec!["daemon", "start"],
            vec!["daemon", "stop"],
            vec!["daemon", "status"],
            vec!["daemon", "install"],
            vec!["mcp"],
            vec![
                "native-host",
                "install",
                "--browser",
                "chrome",
                "--extension-id",
                "abcdefghijklmnopabcdefghijklmnop",
            ],
            vec!["native-host", "uninstall"],
            vec!["native-host", "status"],
            vec!["config", "get"],
            vec!["config", "set", "scheduler.concurrent_jobs", "8"],
            vec!["config", "path"],
            vec!["clients", "list", "--include-revoked"],
            vec!["clients", "add", "--name", "ogun", "--kind", "service"],
            vec![
                "clients",
                "add",
                "--name",
                "bridge",
                "--role",
                "a2a",
                "--permission",
                "jobs.read",
            ],
            vec!["clients", "revoke", "cli_x"],
            vec![
                "workflow",
                "plan",
                "--goal",
                "papers",
                "--url",
                "https://a.example/list",
                "--candidate",
                "plan.json",
            ],
            vec!["workflow", "dry-run", "plan_x", "--items", "5"],
            vec!["workflow", "run", "plan_x", "--plan-hash", "ab"],
            vec!["workflow", "get", "run_x"],
            vec!["workflow", "ls"],
            vec!["workflow", "pause", "run_x"],
            vec!["workflow", "resume", "run_x"],
            vec!["workflow", "cancel", "run_x"],
            vec!["workflow", "manifest", "run_x", "--format", "croissant"],
            vec!["--profile", "work", "status"],
            vec!["files", "list", "inbox", "--recursive"],
            vec![
                "files",
                "organize",
                "inbox",
                "--rules",
                "rules.json",
                "--apply",
            ],
            vec!["files", "undo", "fop_x"],
            vec!["auth"],
            vec!["doctor"],
            vec!["tui"],
            vec!["completions", "bash"],
            vec![
                "--json",
                "--quiet",
                "--no-color",
                "--dry-run",
                "--profile",
                "work",
                "ls",
            ],
        ] {
            parse(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
    }

    #[test]
    fn json_and_ndjson_conflict_and_urls_are_required() {
        assert!(parse(&["--json", "--ndjson", "ls"]).is_err());
        assert!(parse(&["get"]).is_err());
        assert!(parse(&["wait"]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn global_flags_work_after_the_subcommand_too() {
        let cli = parse(&["ls", "--json", "--yes"]).unwrap();
        assert!(cli.global.json && cli.global.yes);
    }

    #[test]
    fn there_is_no_flag_that_approves_by_itself() {
        // `--yes` exists globally but approvals ignore it (enforced in trust.rs); no approve-all flag may exist.
        let help = Cli::command().render_long_help().to_string();
        for forbidden in ["--approve-all", "--auto-approve", "--force-approve"] {
            assert!(!help.contains(forbidden));
        }
    }
}
