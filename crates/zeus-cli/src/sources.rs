//! Immutable acquisition review and lifecycle; account secrets only enter at the controlling terminal.
use crate::commands::CmdOutput;
use clap::{Args, Subcommand, ValueEnum};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::PathBuf,
};
use zeus_api::methods::sources::*;
use zeus_api::methods::{
    SourcesAccounts, SourcesConfigure, SourcesControl, SourcesGet, SourcesInspect, SourcesList,
    SourcesRevoke, SourcesSubmit,
};
use zeus_api::{ApiError, ApiResult, Client, call};
/// Source account administration.
#[derive(Debug, Clone, Subcommand)]
pub enum AccountCommand {
    /// List visible provider scopes and revisions.
    List,
    /// Configure exact provider/account scope at the controlling terminal.
    Configure,
    /// Revoke a reviewed account revision at the controlling terminal.
    Revoke {
        /// Source account alias.
        account: String,
    },
}
/// Source lifecycle transition.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Action {
    /// Preserve verified partial bytes.
    Pause,
    /// Resume within the original deadline and account revision.
    Resume,
    /// Stop this operation.
    Cancel,
}
/// Review an immutable target from explicit JSON data.
#[derive(Debug, Clone, Args)]
pub struct InspectArgs {
    /// Human-configured source account alias.
    #[arg(long)]
    pub account: String,
    /// Exact account revision shown by accounts list.
    #[arg(long)]
    pub revision: u64,
    /// JSON SourceTarget descriptor; never executable arguments or credentials.
    #[arg(long)]
    pub target_json: PathBuf,
    /// Explicit .torrent file, only for a torrent target.
    #[arg(long)]
    pub torrent: Option<PathBuf>,
    /// Cumulative receive-byte ceiling including retries.
    #[arg(long)]
    pub max_bytes: u64,
    /// Original operation lifetime in seconds, at most86400.
    #[arg(long, default_value_t = 1800)]
    pub max_seconds: u64,
    /// Exact retry key, reuse only for identical inputs.
    #[arg(long)]
    pub idempotency_key: Option<String>,
}
/// Immutable source commands.
#[derive(Debug, Clone, Subcommand)]
pub enum SourcesCommand {
    /// Configure and revoke human-owned source grants.
    #[command(subcommand)]
    Accounts(AccountCommand),
    /// Inspect an immutable S3/Hub object or bounded torrent.
    Inspect(InspectArgs),
    /// Submit an owned, unexpired review to an allowed destination.
    Submit {
        /// Review ID returned by inspect.
        plan: String,
        /// Final filename for one selected file.
        filename: String,
        /// Destination directory within configured and caller write roots.
        #[arg(long)]
        dest: Option<String>,
        /// Maximum HTTP range connections.
        #[arg(long, default_value_t = 8)]
        connections: u8,
        /// JSON object mapping selected torrent file indices to final leaf names.
        #[arg(long)]
        torrent_names_json: Option<PathBuf>,
        /// Exact request retry key.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// List at most100 owned operations.
    List,
    /// Read owned progress, integrity and publication receipts.
    Get {
        /// Source operation ID.
        operation: String,
    },
    /// Pause, resume or cancel one owned operation.
    Control {
        /// Source operation ID.
        operation: String,
        /// Explicit lifecycle action.
        #[arg(value_enum)]
        action: Action,
        /// Exact request retry key.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
}
fn invalid() -> ApiError {
    ApiError::invalid_params("invalid bounded source descriptor")
}
fn key(value: &Option<String>) -> String {
    value
        .clone()
        .unwrap_or_else(|| format!("source-cli-{}", ulid::Ulid::generate()))
}
fn output<T: serde::Serialize>(value: &T) -> CmdOutput {
    let json = serde_json::to_value(value).unwrap_or_default();
    let text = serde_json::to_string_pretty(&json)
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect::<String>();
    CmdOutput::new(text, &json)
}
fn read(path: &PathBuf, maximum: usize) -> ApiResult<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|_| invalid())?;
    if !file.metadata().map_err(|_| invalid())?.is_file() {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > maximum {
        return Err(invalid());
    }
    Ok(bytes)
}
/// Dispatch the authenticated source API without interpreting file contents as instructions.
pub async fn run<C: Client + ?Sized>(
    command: &SourcesCommand,
    client: &C,
    dry_run: bool,
) -> ApiResult<CmdOutput> {
    if dry_run
        && !matches!(
            command,
            SourcesCommand::List
                | SourcesCommand::Get { .. }
                | SourcesCommand::Accounts(AccountCommand::List)
        )
    {
        return Err(ApiError::invalid_params(
            "source dry-run permits read-only status; inspect creates an immutable review",
        ));
    }
    match command {
        SourcesCommand::Accounts(AccountCommand::List) => Ok(output(
            &call::<SourcesAccounts, _>(client, SourcesReadParams::default()).await?,
        )),
        SourcesCommand::Accounts(AccountCommand::Configure) => configure(client).await,
        SourcesCommand::Accounts(AccountCommand::Revoke { account }) => {
            revoke(client, account).await
        }
        SourcesCommand::Inspect(args) => {
            use base64::Engine as _;
            let target = serde_json::from_slice(&read(&args.target_json, 64 * 1024)?)
                .map_err(|_| invalid())?;
            let metainfo_base64 = args
                .torrent
                .as_ref()
                .map(|p| {
                    read(p, 700 * 1024).map(|b| {
                        zeus_types::SecretString::new(
                            base64::engine::general_purpose::STANDARD.encode(b),
                        )
                    })
                })
                .transpose()?;
            Ok(output(
                &call::<SourcesInspect, _>(
                    client,
                    SourcesInspectParams {
                        account_id: args.account.clone(),
                        account_revision: args.revision,
                        target,
                        budget: SourceBudget {
                            max_received_bytes: args.max_bytes,
                            timeout_ms: args.max_seconds.checked_mul(1000).ok_or_else(invalid)?,
                        },
                        metainfo_base64,
                        idempotency_key: key(&args.idempotency_key),
                    },
                )
                .await?,
            ))
        }
        SourcesCommand::Submit {
            plan,
            filename,
            dest,
            connections,
            torrent_names_json,
            idempotency_key,
        } => {
            let torrent_filenames = torrent_names_json
                .as_ref()
                .map(|p| {
                    read(p, 64 * 1024)
                        .and_then(|b| serde_json::from_slice(&b).map_err(|_| invalid()))
                })
                .transpose()?
                .unwrap_or_else(BTreeMap::new);
            Ok(output(
                &call::<SourcesSubmit, _>(
                    client,
                    SourcesSubmitParams {
                        plan_id: plan.clone(),
                        dest_dir: dest.clone(),
                        filename: filename.clone(),
                        max_connections: *connections,
                        torrent_filenames,
                        idempotency_key: key(idempotency_key),
                    },
                )
                .await?,
            ))
        }
        SourcesCommand::List => Ok(output(
            &call::<SourcesList, _>(client, SourcesReadParams::default()).await?,
        )),
        SourcesCommand::Get { operation } => Ok(output(
            &call::<SourcesGet, _>(
                client,
                SourcesGetParams {
                    operation_id: operation.clone(),
                },
            )
            .await?,
        )),
        SourcesCommand::Control {
            operation,
            action,
            idempotency_key,
        } => Ok(output(
            &call::<SourcesControl, _>(
                client,
                SourcesControlParams {
                    operation_id: operation.clone(),
                    action: match action {
                        Action::Pause => SourceControlAction::Pause,
                        Action::Resume => SourceControlAction::Resume,
                        Action::Cancel => SourceControlAction::Cancel,
                    },
                    idempotency_key: key(idempotency_key),
                },
            )
            .await?,
        )),
    }
}
fn human(error: impl std::fmt::Display) -> ApiError {
    let _ = error;
    ApiError::human_required("verified controlling terminal")
}
async fn configure<C: Client + ?Sized>(client: &C) -> ApiResult<CmdOutput> {
    use crate::cloud::{canceled, confirm, line};
    if !client.session().kind.is_human_capable() || client.session().downgraded {
        return Err(human("identity"));
    }
    let mut tty = crate::trust::open_terminal().map_err(human)?;
    tty.discard_pending().map_err(human)?;
    tty.output.write_all(b"Immutable source grant\nSaving allows the named credentials to inspect/download within this exact provider scope and per-operation budget. No ambient account tokens are discovered.\n").map_err(human)?;
    let account_id = line(&mut tty, "Account alias: ")?;
    let untrusted_label = line(&mut tty, "Display label: ")?;
    tty.output
        .write_all(b"Provider: 1. Amazon S3  2. Hugging Face  3. Torrent\n")
        .map_err(human)?;
    let choice = line(&mut tty, "Choose provider [1-3]: ")?;
    let scope = match choice.trim() {
        "1" => {
            serde_json::json!({"provider":"s3","region":line(&mut tty,"AWS region (for example eu-west-1): ")?,"bucket":line(&mut tty,"Allowed bucket: ")?,"key_prefix":line(&mut tty,"Allowed object key prefix (blank for this whole bucket): ")?})
        }
        "2" => {
            let family = line(&mut tty, "Repository type [model/dataset/space]: ")?;
            if !matches!(family.as_str(), "model" | "dataset" | "space") {
                return Err(invalid());
            }
            serde_json::json!({"provider":"hugging_face","repository_type":family,"repository":line(&mut tty,"Allowed repository (owner/name): ")?})
        }
        "3" => {
            serde_json::json!({"provider":"torrent","max_peers":line(&mut tty,"Maximum peers [1-32]: ")?.parse::<u16>().map_err(|_|invalid())?,"max_download_bytes_per_second":line(&mut tty,"Maximum received bytes per second (positive): ")?.parse::<u64>().map_err(|_|invalid())?,"allowed_peer_ips":line(&mut tty,"Explicit allowed peer IP addresses, comma-separated: ")?.split(',').map(|v|v.trim().to_owned()).collect::<Vec<_>>()})
        }
        _ => return Err(invalid()),
    };
    let config = serde_json::from_value(scope).map_err(|_| invalid())?;
    let clients = call::<zeus_api::methods::ClientsList, _>(
        client,
        zeus_api::methods::admin::ClientsListParams::default(),
    )
    .await?;
    tty.output.write_all(b"Choose the programs allowed to use this source. Nothing is selected by default. Select app for the Zeus desktop, cli for this terminal, or an explicitly named agent.\n").map_err(human)?;
    for record in clients
        .clients
        .iter()
        .filter(|record| eligible_client(record))
    {
        let label = serde_json::to_string(&record.name).map_err(|_| invalid())?;
        tty.output
            .write_all(
                format!(
                    "  {label}: {} ({:?}/{:?})\n",
                    record.credential_id, record.kind, record.role
                )
                .as_bytes(),
            )
            .map_err(human)?;
    }
    let selection = line(
        &mut tty,
        "Allowed client names or credential IDs, comma-separated (blank cancels): ",
    )?;
    if selection.trim().is_empty() {
        return Ok(canceled());
    }
    let allowed_credential_ids = select_clients(&clients.clients, &selection)?;
    let max_received_bytes = line(
        &mut tty,
        "Maximum cumulative received bytes per operation: ",
    )?
    .parse()
    .map_err(|_| invalid())?;
    let timeout_ms = line(&mut tty, "Maximum operation duration in seconds: ")?
        .parse::<u64>()
        .map_err(|_| invalid())?
        .checked_mul(1000)
        .ok_or_else(invalid)?;
    let accounts = call::<SourcesAccounts, _>(client, SourcesReadParams::default()).await?;
    let expected_revision = accounts
        .accounts
        .iter()
        .find(|a| a.account_id == account_id)
        .map(|a| a.revision);
    let credential = match choice.trim() {
        "1" => {
            let access = hidden(&mut tty, "AWS access key ID (hidden): ")?;
            let secret = hidden(&mut tty, "AWS secret access key (hidden): ")?;
            let session = hidden(
                &mut tty,
                "AWS session token, if temporary (hidden; blank otherwise): ",
            )?;
            #[derive(serde::Serialize)]
            struct S3Key<'a> {
                access_key_id: &'a str,
                secret_access_key: &'a str,
                session_token: Option<&'a str>,
            }
            let text = serde_json::to_string(&S3Key {
                access_key_id: &access,
                secret_access_key: &secret,
                session_token: (!session.is_empty()).then_some(session.as_str()),
            })
            .map_err(|_| invalid())?;
            Some(zeus_types::SecretString::new(text))
        }
        "2" => {
            let token = hidden(
                &mut tty,
                "Hugging Face token (hidden; blank for public access): ",
            )?;
            (!token.is_empty()).then(|| zeus_types::SecretString::new(token.to_string()))
        }
        _ => None,
    };
    let params = SourcesAccountConfigureParams {
        account_id,
        untrusted_label,
        config,
        allowed_credential_ids,
        budget_ceiling: SourceBudget {
            max_received_bytes,
            timeout_ms,
        },
        expected_revision,
        credential,
        idempotency_key: key(&None),
    };
    let review = serde_json::json!({"account":params.account_id,"label":params.untrusted_label,"scope":params.config,"allowed_credential_ids":params.allowed_credential_ids,"budget":params.budget_ceiling,"replacing_revision":params.expected_revision,"credential_supplied":params.credential.is_some()});
    tty.output
        .write_all(
            serde_json::to_string_pretty(&review)
                .map_err(|_| invalid())?
                .as_bytes(),
        )
        .map_err(human)?;
    tty.output.write_all(b"\n").map_err(human)?;
    if !confirm(&mut tty, "configure")? {
        return Ok(canceled());
    }
    Ok(output(&call::<SourcesConfigure, _>(client, params).await?))
}
async fn revoke<C: Client + ?Sized>(client: &C, account: &str) -> ApiResult<CmdOutput> {
    if !client.session().kind.is_human_capable() || client.session().downgraded {
        return Err(human("identity"));
    }
    let accounts = call::<SourcesAccounts, _>(client, SourcesReadParams::default()).await?;
    let row = accounts
        .accounts
        .iter()
        .find(|a| a.account_id == account)
        .ok_or_else(invalid)?;
    let mut tty = crate::trust::open_terminal().map_err(human)?;
    tty.output.write_all(format!("Revoke source {} revision {}. In-flight reads and final publication are refused after revocation.\n",serde_json::to_string(account).map_err(|_|invalid())?,row.revision).as_bytes()).map_err(human)?;
    if !crate::cloud::confirm(&mut tty, "revoke")? {
        return Ok(crate::cloud::canceled());
    }
    Ok(output(
        &call::<SourcesRevoke, _>(
            client,
            SourcesAccountRevokeParams {
                account_id: account.into(),
                expected_revision: row.revision,
                idempotency_key: key(&None),
            },
        )
        .await?,
    ))
}

fn hidden(tty: &mut crate::trust::Terminal, prompt: &str) -> ApiResult<zeroize::Zeroizing<String>> {
    tty.discard_pending().map_err(human)?;
    let input = tty
        .physical_input
        .as_ref()
        .ok_or_else(|| human("terminal"))?;
    let guard = crate::terminal_os::EchoGuard::disable(input).map_err(human)?;
    let value = crate::cloud::line(tty, prompt)?;
    drop(guard);
    tty.output.write_all(b"\n").map_err(human)?;
    Ok(zeroize::Zeroizing::new(value))
}
fn eligible_client(record: &zeus_api::methods::admin::ClientRecord) -> bool {
    !record.revoked
        && record
            .permissions
            .contains(&zeus_api::Permission::JobsWrite)
}
fn select_clients(
    clients: &[zeus_api::methods::admin::ClientRecord],
    selection: &str,
) -> ApiResult<Vec<String>> {
    let mut selected = std::collections::BTreeSet::new();
    for value in selection.split(',').map(str::trim) {
        let matches = clients
            .iter()
            .filter(|r| eligible_client(r) && (r.name == value || r.credential_id == value))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(invalid());
        }
        selected.insert(matches[0].credential_id.clone());
    }
    if selected.is_empty() {
        return Err(invalid());
    }
    Ok(selected.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn grant_selection_is_explicit_exact_and_excludes_revoked_clients() {
        let now = zeus_types::Timestamp::now();
        let app = zeus_api::methods::admin::ClientRecord {
            credential_id: "client-app".into(),
            name: "app".into(),
            kind: zeus_api::ClientKind::Agent,
            role: zeus_api::ClientRole::Gui,
            permissions: [zeus_api::Permission::JobsWrite].into_iter().collect(),
            created_at: now,
            last_used_at: None,
            revoked: false,
            revoked_at: None,
            roots: vec![],
        };
        let mut revoked = app.clone();
        revoked.name = "old".into();
        revoked.credential_id = "client-old".into();
        revoked.revoked = true;
        let mut reader = app.clone();
        reader.name = "read-only".into();
        reader.credential_id = "client-reader".into();
        reader.permissions.clear();
        let clients = vec![app, revoked, reader];
        assert_eq!(
            select_clients(&clients, "app,client-app").unwrap(),
            vec!["client-app"]
        );
        for value in [
            "",
            "old",
            "client-old",
            "read-only",
            "client-reader",
            "ap",
            "app,",
        ] {
            assert!(select_clients(&clients, value).is_err());
        }
    }
    #[test]
    fn account_configuration_accepts_no_secret_or_automatic_grant_flags() {
        assert!(crate::Cli::try_parse_from(["zeus", "sources", "accounts", "configure"]).is_ok());
        for argument in ["--credential", "--allow-app", "--yes"] {
            assert!(
                crate::Cli::try_parse_from([
                    "zeus",
                    "sources",
                    "accounts",
                    "configure",
                    argument,
                    "SECRET-CANARY"
                ])
                .is_err()
            );
        }
        assert!(
            crate::Cli::try_parse_from(["zeus", "sources", "control", "srcop_example", "cancel"])
                .is_ok()
        );
        assert!(
            crate::Cli::try_parse_from(["zeus", "sources", "control", "srcop_example", "approve"])
                .is_err()
        );
    }
}
