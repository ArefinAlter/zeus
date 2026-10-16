//! Cloud delivery commands. Account credentials are entered only at the controlling terminal.
use clap::{Args, Subcommand, ValueEnum};

/// Explicitly chosen delivery path; Zeus never falls back to another mode implicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum CloudMode {
    /// Stage and verify the complete source before uploading.
    StagedUpload,
    /// Relay public source bytes with bounded buffering.
    StreamUpload,
    /// Disclose a public source URL to a provider that can fetch it.
    RemoteImport,
}

/// Cloud delivery planning inputs.
#[derive(Debug, Clone, Args)]
pub struct PlanArgs {
    /// Public HTTP(S) source URL; mutually exclusive with --job.
    #[arg(required_unless_present = "job", conflicts_with = "job")]
    pub url: Option<String>,
    /// Existing completed, verified download.
    #[arg(long, conflicts_with = "url")]
    pub job: Option<String>,
    /// Account registration identifier from `cloud accounts list`.
    #[arg(long)]
    pub account: String,
    /// Filename only, inside the account's configured folder.
    #[arg(long)]
    pub remote_name: String,
    /// Choose the path explicitly; provider capabilities are checked by the daemon.
    #[arg(long, value_enum)]
    pub mode: CloudMode,
    /// Exact source size, required for a public URL.
    #[arg(long, required_unless_present = "job", conflicts_with = "job")]
    pub size_bytes: Option<u64>,
    /// Maximum local operation lifetime, in seconds (1–86400).
    #[arg(long, default_value_t = 1800)]
    pub max_seconds: u32,
    /// Accept that provider imports may exceed the cap or continue after local cancellation.
    #[arg(long)]
    pub allow_unbounded_remote: bool,
    /// Stable intent key; reuse only for identical plan inputs.
    #[arg(long)]
    pub idempotency_key: Option<String>,
    /// Maximum source bytes this delivery may move.
    #[arg(long)]
    pub max_bytes: u64,
    /// Expected source SHA-256, when independently known.
    #[arg(long)]
    pub sha256: Option<String>,
    /// Precomputed 4 MiB block MD5 values for Baidu streaming; comma-separated.
    #[arg(long, value_delimiter = ',', conflicts_with = "job")]
    pub block_md5: Vec<String>,
    /// UTF-8 file containing one precomputed 4 MiB block MD5 per line (up to 8192).
    #[arg(long, conflicts_with_all = ["job", "block_md5"])]
    pub block_md5_file: Option<std::path::PathBuf>,
}

/// Account operations. Configuration and revocation require a fresh human terminal review.
#[derive(Debug, Clone, Subcommand)]
pub enum AccountsCommand {
    /// List configured accounts and their advertised capabilities; never returns credentials.
    List,
    /// Configure a provider using hidden controlling-terminal input; no secret flags or stdin.
    Configure,
    /// Revoke a local account registration and its delivery authority.
    Revoke {
        /// Registered account identifier.
        account: String,
    },
}

/// Delivery control; resume may require renewed human authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CloudAction {
    /// Pause delivery; provider-side progress may require reconciliation.
    Pause,
    /// Resume only when the daemon confirms current authority.
    Resume,
    /// Cancel delivery; completed remote files are not silently deleted.
    Cancel,
}

/// `zeus cloud` command tree.
#[derive(Debug, Clone, Subcommand)]
pub enum CloudCommand {
    /// Inspect or configure cloud accounts.
    #[command(subcommand)]
    Accounts(AccountsCommand),
    /// Show account capabilities and recent deliveries.
    Status,
    /// Preview a bounded delivery; no upload or remote fetch occurs.
    Plan(PlanArgs),
    /// Submit an existing plan for human approval; never decides that approval.
    #[command(alias = "deliver")]
    Submit {
        /// Plan identifier returned by `cloud plan`.
        plan: String,
        /// Exact canonical plan hash returned by planning.
        #[arg(long)]
        plan_hash: String,
        /// Stable intent key for retries; defaults to a fresh key.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// List delivery progress and verification levels.
    List {
        /// Maximum results, 1 to 100.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Inspect delivery state, provider-confirmed bytes and verification evidence.
    Get {
        /// Delivery identifier.
        delivery: String,
    },
    /// Pause, resume or cancel a delivery.
    Control {
        /// Delivery identifier.
        delivery: String,
        /// Explicit operation.
        #[arg(value_enum)]
        action: CloudAction,
        /// Stable intent key for retries; defaults to a fresh key.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
}

use crate::{
    commands::CmdOutput,
    trust::{Terminal, open_terminal},
};
use std::io::{BufRead, Read, Write};
use zeus_api::methods::cloud::*;
use zeus_api::methods::{
    CloudAccountsConfigure, CloudAccountsList, CloudAccountsRevoke, CloudControl, CloudGet,
    CloudList, CloudPlan, CloudSubmit,
};
use zeus_api::{ApiError, ApiResult, Client, call};
use zeus_types::{Hex256, SecretString};

impl From<CloudMode> for DeliveryMode {
    fn from(value: CloudMode) -> Self {
        match value {
            CloudMode::StagedUpload => Self::StagedUpload,
            CloudMode::StreamUpload => Self::StreamUpload,
            CloudMode::RemoteImport => Self::RemoteImport,
        }
    }
}
fn key(value: &Option<String>) -> String {
    value
        .clone()
        .unwrap_or_else(|| format!("cli-cloud-{}", ulid::Ulid::generate()))
}
fn invalid() -> ApiError {
    ApiError::invalid_params("Invalid cloud source, hash or settings; no delivery was submitted")
}
fn human_error(_: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        "api.human_required",
        "Cloud account changes require a controlling terminal and explicit settings",
    )
}
fn sanitize(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() && !matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')).collect()
}
fn output<T: serde::Serialize>(value: &T) -> CmdOutput {
    let json = serde_json::to_value(value).unwrap_or_default();
    let text = serde_json::to_string_pretty(&json)
        .unwrap_or_default()
        .lines()
        .map(sanitize)
        .collect::<Vec<_>>()
        .join("\n");
    let mut result = CmdOutput::new(text, &json);
    if let Some(items) = json
        .get("deliveries")
        .or_else(|| json.get("accounts"))
        .and_then(serde_json::Value::as_array)
    {
        result.items = items.clone();
    }
    result
}
fn block_manifest(args: &PlanArgs) -> ApiResult<Option<Vec<String>>> {
    let values = if let Some(path) = &args.block_md5_file {
        let file = std::fs::File::open(path).map_err(|_| invalid())?;
        if !file.metadata().map_err(|_| invalid())?.is_file() {
            return Err(invalid());
        }
        let mut text = String::new();
        let count = file
            .take(512 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|_| invalid())?;
        if count > 512 * 1024 {
            return Err(invalid());
        }
        text.lines()
            .map(|line| line.trim().to_owned())
            .collect::<Vec<_>>()
    } else {
        args.block_md5.clone()
    };
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() > 8192
        || values
            .iter()
            .any(|value| value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(invalid());
    }
    Ok(Some(
        values
            .into_iter()
            .map(|value| value.to_ascii_lowercase())
            .collect(),
    ))
}
fn plan_params(args: &PlanArgs) -> ApiResult<CloudPlanParams> {
    let source = match (&args.url, &args.job) {
        (Some(url), None) => CloudSource::PublicUrl {
            url: url.parse().map_err(|_| invalid())?,
            size_bytes: args.size_bytes.ok_or_else(invalid)?,
            sha256: args
                .sha256
                .as_deref()
                .map(Hex256::from_hex)
                .transpose()
                .map_err(|_| invalid())?,
            block_md5: block_manifest(args)?,
        },
        (None, Some(job))
            if args.sha256.is_none()
                && args.size_bytes.is_none()
                && args.block_md5.is_empty()
                && args.block_md5_file.is_none() =>
        {
            CloudSource::CompletedJob {
                job_id: job.parse().map_err(|_| invalid())?,
            }
        }
        _ => return Err(invalid()),
    };
    Ok(CloudPlanParams {
        account_id: args.account.clone(),
        mode: args.mode.into(),
        source,
        remote_name: args.remote_name.clone(),
        max_bytes: args.max_bytes,
        max_seconds: args.max_seconds,
        allow_unbounded_remote: args.allow_unbounded_remote,
        idempotency_key: key(&args.idempotency_key),
    })
}
/// Dispatch explicit cloud operations through the daemon; planning/submitting never decides approval.
pub async fn run<C: Client + ?Sized>(
    command: &CloudCommand,
    client: &C,
    dry_run: bool,
) -> ApiResult<CmdOutput> {
    if dry_run
        && matches!(
            command,
            CloudCommand::Submit { .. }
                | CloudCommand::Control { .. }
                | CloudCommand::Accounts(
                    AccountsCommand::Configure | AccountsCommand::Revoke { .. }
                )
        )
    {
        return Err(ApiError::invalid_params(
            "Use cloud plan for a dry run; no account or delivery mutation was requested",
        ));
    }
    match command {
        CloudCommand::Accounts(AccountsCommand::List) => Ok(output(
            &call::<CloudAccountsList, _>(client, CloudAccountsListParams::default()).await?,
        )),
        CloudCommand::Accounts(AccountsCommand::Configure) => configure(client).await,
        CloudCommand::Accounts(AccountsCommand::Revoke { account }) => {
            revoke(client, account).await
        }
        CloudCommand::Status => {
            let accounts =
                call::<CloudAccountsList, _>(client, CloudAccountsListParams::default()).await?;
            let deliveries = call::<CloudList, _>(client, CloudListParams { limit: 20 }).await?;
            Ok(output(
                &serde_json::json!({"accounts":accounts.accounts,"deliveries":deliveries.deliveries}),
            ))
        }
        CloudCommand::Plan(args) => Ok(output(
            &call::<CloudPlan, _>(client, plan_params(args)?).await?,
        )),
        CloudCommand::Submit {
            plan,
            plan_hash,
            idempotency_key,
        } => Ok(output(
            &call::<CloudSubmit, _>(
                client,
                CloudSubmitParams {
                    plan_id: plan.clone(),
                    plan_hash: Hex256::from_hex(plan_hash).map_err(|_| invalid())?,
                    idempotency_key: key(idempotency_key),
                },
            )
            .await?,
        )),
        CloudCommand::List { limit } => Ok(output(
            &call::<CloudList, _>(client, CloudListParams { limit: *limit }).await?,
        )),
        CloudCommand::Get { delivery } => Ok(output(
            &call::<CloudGet, _>(
                client,
                CloudGetParams {
                    delivery_id: delivery.clone(),
                },
            )
            .await?,
        )),
        CloudCommand::Control {
            delivery,
            action,
            idempotency_key,
        } => Ok(output(
            &call::<CloudControl, _>(
                client,
                CloudControlParams {
                    delivery_id: delivery.clone(),
                    action: match action {
                        CloudAction::Pause => CloudControlAction::Pause,
                        CloudAction::Resume => CloudControlAction::Resume,
                        CloudAction::Cancel => CloudControlAction::Cancel,
                    },
                    idempotency_key: key(idempotency_key),
                },
            )
            .await?,
        )),
    }
}
fn line(terminal: &mut Terminal, prompt: &str) -> ApiResult<String> {
    terminal
        .output
        .write_all(prompt.as_bytes())
        .map_err(human_error)?;
    terminal.output.flush().map_err(human_error)?;
    let mut value = String::new();
    let count = terminal
        .input
        .as_mut()
        .take(4097)
        .read_line(&mut value)
        .map_err(human_error)?;
    if count == 0 || count > 4096 {
        return Err(human_error("input"));
    }
    while value.ends_with(['\r', '\n']) {
        value.pop();
    }
    Ok(value)
}
fn confirm(terminal: &mut Terminal, verb: &str) -> ApiResult<bool> {
    terminal.output.flush().map_err(human_error)?;
    std::thread::sleep(std::time::Duration::from_millis(750));
    terminal.discard_pending().map_err(human_error)?;
    Ok(line(
        terminal,
        &format!("Type {verb} to continue, or Enter to cancel: "),
    )?
    .trim()
        == verb)
}
fn canceled() -> CmdOutput {
    CmdOutput::new(
        "Canceled; no account change was requested.",
        &serde_json::json!({"changed":false}),
    )
}
async fn configure<C: Client + ?Sized>(client: &C) -> ApiResult<CmdOutput> {
    if client.session().kind < zeus_api::kind::ClientKind::Human || client.session().downgraded {
        return Err(human_error("untrusted connection"));
    }
    let mut terminal = open_terminal().map_err(human_error)?;
    terminal.output.write_all(b"Cloud account scope\nThis registers a destination; each delivery still needs a separate human approval.\nNative providers need an OAuth access credential obtained separately. This command does not perform sign-in or refresh it.\n1. Google Drive\n2. Yandex Disk\n3. Explicit rclone remote\n4. Baidu Netdisk app folder\n").map_err(human_error)?;
    terminal.discard_pending().map_err(human_error)?;
    let choice = line(&mut terminal, "Provider [1-4]: ")?;
    let account_id = line(
        &mut terminal,
        "Account alias (lowercase letters, digits, _ or -): ",
    )?;
    let existing = call::<CloudAccountsList, _>(client, CloudAccountsListParams::default()).await?;
    let expected_revision = existing
        .accounts
        .iter()
        .find(|a| a.account_id == account_id)
        .map(|a| a.revision);
    let label = line(&mut terminal, "Display label: ")?;
    let config_json = match choice.trim() {
        "1" => {
            serde_json::json!({"provider":"google_drive","parent_id":line(&mut terminal,"Existing Google Drive folder ID (or root): ")?})
        }
        "2" => {
            serde_json::json!({"provider":"yandex_disk","root_path":line(&mut terminal,"Existing folder (disk:/Zeus): ")?})
        }
        "3" => {
            terminal.output.write_all(b"Use a dedicated rclone config containing only this remote. Zeus will invoke this exact executable and config, without extra flags or automatic discovery. Local credential files remain your responsibility.\n").map_err(human_error)?;
            serde_json::json!({"provider":"rclone","config":{"executable":line(&mut terminal,"Absolute rclone executable path: ")?,"config_file":line(&mut terminal,"Absolute dedicated config file path: ")?,"remote":line(&mut terminal,"Configured remote name: ")?,"root_path":line(&mut terminal,"Allowed folder within that remote: ")?}})
        }
        "4" => {
            serde_json::json!({"provider":"baidu_netdisk","root_path":line(&mut terminal,"App folder (/apps/<appname>/...): ")?})
        }
        _ => return Err(invalid()),
    };
    let config: ProviderConfig = serde_json::from_value(config_json).map_err(|_| invalid())?;
    let max_bytes_per_delivery = line(
        &mut terminal,
        "Maximum bytes per delivery for this account: ",
    )?
    .trim()
    .parse()
    .map_err(|_| invalid())?;
    let credential = if choice.trim() == "3" {
        None
    } else {
        terminal.discard_pending().map_err(human_error)?;
        let input = terminal
            .physical_input
            .as_ref()
            .ok_or_else(|| human_error("terminal"))?;
        let guard = crate::terminal_os::EchoGuard::disable(input).map_err(human_error)?;
        let secret = line(
            &mut terminal,
            "OAuth access credential (hidden; empty keeps existing credential): ",
        )?;
        drop(guard);
        terminal.output.write_all(b"\n").map_err(human_error)?;
        if secret.is_empty() {
            None
        } else {
            Some(SecretString::new(secret))
        }
    };
    let params = CloudAccountsConfigureParams {
        account_id,
        label,
        config,
        max_bytes_per_delivery,
        expected_revision,
        credential,
        idempotency_key: key(&None),
    };
    let review = format!(
        "Account: {}\nLabel: {}\nProvider settings: {}\nByte ceiling: {}\nReplacing revision: {:?}\nCredential: {}\nNo transfer is authorized by saving.\n",
        sanitize(&params.account_id),
        sanitize(&params.label),
        sanitize(&serde_json::to_string(&params.config).map_err(|_| invalid())?),
        params.max_bytes_per_delivery,
        params.expected_revision,
        if params.credential.is_some() {
            "replace in OS keyring"
        } else {
            "unchanged / external dedicated config"
        }
    );
    terminal
        .output
        .write_all(review.as_bytes())
        .map_err(human_error)?;
    if !confirm(&mut terminal, "configure")? {
        return Ok(canceled());
    }
    Ok(output(
        &call::<CloudAccountsConfigure, _>(client, params).await?,
    ))
}
async fn revoke<C: Client + ?Sized>(client: &C, account: &str) -> ApiResult<CmdOutput> {
    if client.session().kind < zeus_api::kind::ClientKind::Human || client.session().downgraded {
        return Err(human_error("untrusted connection"));
    }
    let mut terminal = open_terminal().map_err(human_error)?;
    let accounts = call::<CloudAccountsList, _>(client, CloudAccountsListParams::default()).await?;
    let scope = accounts
        .accounts
        .iter()
        .find(|a| a.account_id == account)
        .ok_or_else(invalid)?;
    terminal.output.write_all(format!("Revoke account {} revision {}\nDestination: {}\nStops Zeus work and invalidates this account's authority. Provider imports may continue; this does not revoke OAuth at the provider or delete remote files.\n",sanitize(account),scope.revision,sanitize(&scope.destination)).as_bytes()).map_err(human_error)?;
    if !confirm(&mut terminal, "revoke")? {
        return Ok(canceled());
    }
    Ok(output(
        &call::<CloudAccountsRevoke, _>(
            client,
            CloudAccountsRevokeParams {
                account_id: account.into(),
                expected_revision: scope.revision,
                idempotency_key: key(&None),
            },
        )
        .await?,
    ))
}

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::Parser;

    #[test]
    fn delivery_plans_require_mode_source_account_destination_and_budget() {
        let valid = [
            "zeus",
            "cloud",
            "plan",
            "https://example.test/file",
            "--account",
            "research",
            "--remote-name",
            "report.zip",
            "--mode",
            "staged_upload",
            "--max-bytes",
            "4096",
            "--size-bytes",
            "4096",
        ];
        assert!(Cli::try_parse_from(valid).is_ok());
        assert!(
            Cli::try_parse_from(
                valid
                    .into_iter()
                    .filter(|s| !["--mode", "staged_upload"].contains(s))
            )
            .is_err()
        );
        assert!(Cli::try_parse_from(valid.into_iter().chain(["--job", "job_example"])).is_err());
        assert!(Cli::try_parse_from(["zeus", "cloud", "plan", "--account", "research"]).is_err());
    }

    #[test]
    fn submit_requires_exact_plan_hash_and_remote_limits_are_opt_in() {
        assert!(Cli::try_parse_from(["zeus", "cloud", "submit", "clp_example"]).is_err());
        let cli = Cli::try_parse_from([
            "zeus",
            "cloud",
            "plan",
            "https://example.test/file?private=ZEUS-CANARY-source",
            "--account",
            "research",
            "--remote-name",
            "file.zip",
            "--mode",
            "remote_import",
            "--size-bytes",
            "42",
            "--max-bytes",
            "42",
        ])
        .unwrap();
        let crate::cli::Command::Cloud(super::CloudCommand::Plan(args)) = cli.command else {
            panic!("plan expected")
        };
        let params = super::plan_params(&args).unwrap();
        assert!(!params.allow_unbounded_remote);
        assert_eq!(params.mode, super::DeliveryMode::RemoteImport);
        assert!(!format!("{params:?}").contains("ZEUS-CANARY-source"));
    }

    #[test]
    fn block_manifest_file_is_bounded_data_and_errors_do_not_echo_contents() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{}", "A".repeat(32)).unwrap();
        let cli = Cli::try_parse_from([
            "zeus",
            "cloud",
            "plan",
            "https://example.test/file",
            "--account",
            "research",
            "--remote-name",
            "file.zip",
            "--mode",
            "stream_upload",
            "--size-bytes",
            "42",
            "--max-bytes",
            "42",
            "--block-md5-file",
            file.path().to_str().unwrap(),
        ])
        .unwrap();
        let crate::cli::Command::Cloud(super::CloudCommand::Plan(args)) = cli.command else {
            panic!("plan expected")
        };
        assert_eq!(
            super::block_manifest(&args).unwrap(),
            Some(vec!["a".repeat(32)])
        );
        file.as_file_mut().set_len(0).unwrap();
        std::io::Seek::rewind(file.as_file_mut()).unwrap();
        file.write_all(b"ZEUS-CANARY-not-a-manifest").unwrap();
        let error = super::block_manifest(&args).unwrap_err();
        assert!(!error.to_string().contains("ZEUS-CANARY"));
        file.as_file_mut().set_len(512 * 1024 + 1).unwrap();
        assert!(super::block_manifest(&args).is_err());
    }

    #[test]
    fn human_output_preserves_paths_without_terminal_control_sequences() {
        assert_eq!(
            super::sanitize("disk:/Archive\u{1b}\u{202e}"),
            "disk:/Archive"
        );
        let output = super::output(&serde_json::json!({"destination":"disk:/Archive"}));
        assert!(output.text.contains("disk:/Archive"));
        assert!(output.text.contains('\n'));
    }

    #[test]
    fn no_command_line_account_credentials_or_arbitrary_provider_flags() {
        for argument in [
            "--token",
            "--password",
            "--secret",
            "--config",
            "--rclone-args",
        ] {
            assert!(
                Cli::try_parse_from(["zeus", "cloud", "accounts", "configure", argument, "secret"])
                    .is_err()
            );
        }
    }
}
