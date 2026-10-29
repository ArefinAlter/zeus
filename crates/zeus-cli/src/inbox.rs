//! Durable intake commands. Human inspection may select an owner; mutations never do.
use crate::{commands::CmdOutput, output::sanitize};
use clap::Subcommand;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};
use zeus_api::{
    ApiError, ApiResult, Client, PageRequest, call, methods::inbox::*, methods::jobs::NewJob,
    methods::*,
};
use zeus_types::HttpUrl;
/// Human terminal access to durable owner-scoped packages and exact saved batches.
#[derive(Debug, Clone, Subcommand)]
pub enum InboxCommand {
    /// List redacted packages. --owner is explicit read-only human inspection.
    List {
        /// Credential to inspect; never accepted on mutations.
        #[arg(long)]
        owner: Option<String>,
        /// Entries per page,1–100.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Continuation from the preceding page.
        #[arg(long)]
        cursor: Option<String>,
        /// Include archived packages.
        #[arg(long)]
        include_archived: bool,
    },
    /// Inspect one redacted package and a bounded entry page.
    Inspect {
        /// Package identity.
        package: String,
        /// Credential to inspect read-only.
        #[arg(long)]
        owner: Option<String>,
        /// Include retained previews and submissions explicitly.
        #[arg(long)]
        history: bool,
        /// Entries per page,1–100.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Opaque continuation.
        #[arg(long)]
        cursor: Option<String>,
        /// Omit entries already enqueued.
        #[arg(long)]
        pending_only: bool,
    },
    /// Create a package; this saves metadata and starts no download.
    Create {
        /// Untrusted display name.
        name: String,
        /// Suggested destination inside current write roots.
        #[arg(short = 'o', long)]
        output: Option<String>,
        /// Reuse only for an identical request.
        #[arg(long)]
        idempotency_key: String,
    },
    /// Retain1–200 URL requests from a UTF-8 file or stdin (-), without starting them.
    Add {
        /// Existing package.
        package: String,
        /// Exact revision from its latest response.
        #[arg(long)]
        revision: u64,
        /// URL file; at most1MiB. The entire file is validated first.
        #[arg(short = 'i', long)]
        input_file: PathBuf,
        /// Override the package destination for these entries.
        #[arg(short = 'o', long)]
        output: Option<String>,
        /// Exact request retry key.
        #[arg(long)]
        idempotency_key: String,
    },
    /// Remove pending metadata; accepted downloads are preserved.
    Remove {
        /// Package identity.
        package: String,
        /// Exact package revision.
        #[arg(long)]
        revision: u64,
        /// Entry identities to remove,1–500.
        #[arg(required = true)]
        entries: Vec<String>,
        /// Exact mutation key.
        #[arg(long)]
        idempotency_key: String,
    },
    /// Archive metadata, preserving all jobs and receipts; --restore reverses it.
    Archive {
        /// Package identity.
        package: String,
        /// Exact package revision.
        #[arg(long)]
        revision: u64,
        /// Restore ordinary list visibility instead.
        #[arg(long)]
        restore: bool,
        /// Exact mutation key.
        #[arg(long)]
        idempotency_key: String,
    },
    /// Save an immutable destination/collision preview of up to50 pending entries.
    Preview {
        /// Package identity.
        package: String,
        /// Exact package revision.
        #[arg(long)]
        revision: u64,
        /// Select entries; otherwise use the next50 pending ones.
        #[arg(long = "entry")]
        entries: Vec<String>,
        /// Exact preview key.
        #[arg(long)]
        idempotency_key: String,
    },
    /// Dispatch an exact saved preview. Separate session approvals remain undecided.
    Submit {
        /// Package identity.
        package: String,
        /// Revision retained in the saved preview.
        #[arg(long)]
        revision: u64,
        /// Saved preview identity.
        #[arg(long)]
        preview: String,
        /// Exact dispatch key; preserve it across interruptions.
        #[arg(long)]
        idempotency_key: String,
    },
    /// Inspect one durable submission, optionally for another owner read-only.
    Submission {
        /// Package identity.
        package: String,
        /// Saved submission identity.
        batch: String,
        /// Credential to inspect read-only.
        #[arg(long)]
        owner: Option<String>,
    },
    /// Reconcile an interrupted batch using its existing hidden payload and enqueue key.
    Retry {
        /// Package identity.
        package: String,
        /// Existing submission identity.
        batch: String,
        /// Key for this exact recovery operation.
        #[arg(long)]
        idempotency_key: String,
    },
}
fn page(limit: u32, cursor: &Option<String>) -> ApiResult<PageRequest> {
    if !(1..=100).contains(&limit) {
        return Err(ApiError::invalid_params("limit must be1–100"));
    }
    Ok(PageRequest {
        limit: Some(limit),
        cursor: cursor.clone(),
    })
}
fn key(value: &str) -> ApiResult<String> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(ApiError::invalid_params(
            "idempotency key must be1–128 UTF-8 bytes without controls",
        ));
    }
    Ok(value.to_owned())
}
fn output(operation: &str, value: &impl Serialize) -> CmdOutput {
    let value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    let text = format!(
        "{operation}\n{}",
        sanitize(&serde_json::to_string_pretty(&value).unwrap_or_default())
    );
    CmdOutput::new(text, &value)
}
fn urls(reader: impl Read) -> ApiResult<Vec<InboxIntakeItem>> {
    let mut text = String::new();
    reader
        .take(1_048_577)
        .read_to_string(&mut text)
        .map_err(|_| ApiError::invalid_params("URL data must be readable UTF-8 of at most1MiB"))?;
    if text.len() > 1_048_576 {
        return Err(ApiError::invalid_params("URL data exceeds1MiB"));
    }
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for line in text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
    {
        if !seen.insert(line.to_owned()) {
            continue;
        }
        let url = HttpUrl::parse(line)
            .map_err(|_| ApiError::invalid_params("each URL must be absolute HTTP(S)"))?;
        entries.push(InboxIntakeItem {
            job: NewJob {
                url,
                hint_url: None,
                referrer: None,
                dest_dir: None,
                filename: None,
                expected_sha256: None,
                expected_size: None,
                priority: None,
                session_origin: None,
                grant_scope_hint: None,
                extra_headers: Default::default(),
                overwrite: None,
                max_connections: None,
                transport_hint: None,
                labels: Default::default(),
                run_id: None,
            },
            untrusted_label: None,
        });
        if entries.len() > 200 {
            return Err(ApiError::invalid_params(
                "add accepts1–200 unique URLs; retain larger packages with successive revision-bound additions",
            ));
        }
    }
    if entries.is_empty() {
        return Err(ApiError::invalid_params("no URL requests supplied"));
    }
    Ok(entries)
}
fn read_input(path: &Path) -> ApiResult<Vec<InboxIntakeItem>> {
    if path == Path::new("-") {
        return urls(std::io::stdin().lock());
    }
    let file = std::fs::File::open(path)
        .map_err(|_| ApiError::invalid_params("URL input is not readable"))?;
    let meta = file
        .metadata()
        .map_err(|_| ApiError::invalid_params("URL input metadata is unavailable"))?;
    if !meta.is_file() || meta.len() > 1_048_576 {
        return Err(ApiError::invalid_params(
            "URL input must be a regular file of at most1MiB",
        ));
    }
    urls(file)
}
/// Execute typed intake calls. --dry-run validates add data but never persists or dispatches it.
pub async fn run<C: Client + ?Sized>(
    command: &InboxCommand,
    client: &C,
    dry_run: bool,
) -> ApiResult<CmdOutput> {
    let read_only = matches!(
        command,
        InboxCommand::List { .. } | InboxCommand::Inspect { .. } | InboxCommand::Submission { .. }
    );
    if dry_run && !read_only {
        if let InboxCommand::Add { input_file, .. } = command {
            let entries = read_input(input_file)?;
            return Ok(CmdOutput::new(
                format!("{} valid URL requests; nothing retained", entries.len()),
                &serde_json::json!({"dry_run":true,"entries":entries.len()}),
            ));
        }
        return Ok(CmdOutput::new(
            "dry run; no intake metadata changed and no download dispatched",
            &serde_json::json!({"dry_run":true}),
        ));
    }
    Ok(match command {
        InboxCommand::List {
            owner,
            limit,
            cursor,
            include_archived,
        } => {
            let result = call::<InboxPackagesList, C>(
                client,
                InboxPackagesListParams {
                    owner_credential_id: owner.clone(),
                    page: page(*limit, cursor)?,
                    include_archived: *include_archived,
                },
            )
            .await?;
            output("inbox.packages.list", &result).with_items(
                result
                    .items
                    .iter()
                    .map(|v| serde_json::to_value(v).unwrap_or_default())
                    .collect(),
            )
        }
        InboxCommand::Inspect {
            package,
            owner,
            history,
            limit,
            cursor,
            pending_only,
        } => output(
            "inbox.packages.get",
            &call::<InboxPackageGet, C>(
                client,
                InboxPackageGetParams {
                    package_id: package.parse()?,
                    owner_credential_id: owner.clone(),
                    include_history: *history,
                    page: page(*limit, cursor)?,
                    pending_only: *pending_only,
                },
            )
            .await?,
        ),
        InboxCommand::Create {
            name,
            output: dest,
            idempotency_key,
        } => output(
            "inbox.packages.put",
            &call::<InboxPackagePut, C>(
                client,
                InboxPackagePutParams {
                    package_id: None,
                    expected_revision: None,
                    untrusted_name: name.clone(),
                    default_dest_dir: dest.clone(),
                    idempotency_key: key(idempotency_key)?,
                },
            )
            .await?,
        ),
        InboxCommand::Add {
            package,
            revision,
            input_file,
            output: dest,
            idempotency_key,
        } => {
            let mut entries = read_input(input_file)?;
            for entry in &mut entries {
                entry.job.dest_dir = dest.clone();
            }
            output(
                "inbox.entries.add",
                &call::<InboxEntriesAdd, C>(
                    client,
                    InboxEntriesAddParams {
                        package_id: package.parse()?,
                        expected_revision: *revision,
                        entries,
                        idempotency_key: key(idempotency_key)?,
                    },
                )
                .await?,
            )
        }
        InboxCommand::Remove {
            package,
            revision,
            entries,
            idempotency_key,
        } => output(
            "inbox.entries.remove",
            &call::<InboxEntriesRemove, C>(
                client,
                InboxEntriesRemoveParams {
                    package_id: package.parse()?,
                    expected_revision: *revision,
                    entry_ids: entries
                        .iter()
                        .map(|id| id.parse())
                        .collect::<ApiResult<_>>()?,
                    idempotency_key: key(idempotency_key)?,
                },
            )
            .await?,
        ),
        InboxCommand::Archive {
            package,
            revision,
            restore,
            idempotency_key,
        } => output(
            "inbox.packages.archive",
            &call::<InboxPackageArchive, C>(
                client,
                InboxPackageArchiveParams {
                    package_id: package.parse()?,
                    expected_revision: *revision,
                    archived: !*restore,
                    idempotency_key: key(idempotency_key)?,
                },
            )
            .await?,
        ),
        InboxCommand::Preview {
            package,
            revision,
            entries,
            idempotency_key,
        } => output(
            "inbox.preview",
            &call::<InboxPreview, C>(
                client,
                InboxPreviewParams {
                    package_id: package.parse()?,
                    expected_revision: *revision,
                    entry_ids: entries
                        .iter()
                        .map(|id| id.parse())
                        .collect::<ApiResult<_>>()?,
                    idempotency_key: key(idempotency_key)?,
                },
            )
            .await?,
        ),
        InboxCommand::Submit {
            package,
            revision,
            preview,
            idempotency_key,
        } => output(
            "inbox.submit",
            &call::<InboxSubmit, C>(
                client,
                InboxSubmitParams {
                    package_id: package.parse()?,
                    expected_revision: *revision,
                    preview_id: preview.parse()?,
                    idempotency_key: key(idempotency_key)?,
                },
            )
            .await?,
        ),
        InboxCommand::Submission {
            package,
            batch,
            owner,
        } => output(
            "inbox.submissions.get",
            &call::<InboxSubmissionGet, C>(
                client,
                InboxSubmissionGetParams {
                    package_id: package.parse()?,
                    batch_id: batch.parse()?,
                    owner_credential_id: owner.clone(),
                },
            )
            .await?,
        ),
        InboxCommand::Retry {
            package,
            batch,
            idempotency_key,
        } => output(
            "inbox.submissions.retry",
            &call::<InboxSubmissionRetry, C>(
                client,
                InboxSubmissionRetryParams {
                    package_id: package.parse()?,
                    batch_id: batch.parse()?,
                    idempotency_key: key(idempotency_key)?,
                },
            )
            .await?,
        ),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entire_url_data_validated_before_rpc_and_signed_query_preserved() {
        let entries=urls(std::io::Cursor::new("\u{feff}#comment\nhttps://example.org/x?token=first\nhttps://example.org/x?token=first\nhttps://example.org/x?token=second\n")).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].job.url.url().as_str().ends_with("token=first"));
        assert!(urls(std::io::Cursor::new("https://example.org/ok\ninvalid")).is_err());
        assert!(urls(std::io::Cursor::new([0xff])).is_err());
        let many = (0..201)
            .map(|i| format!("https://example.org/{i}\n"))
            .collect::<String>();
        assert!(urls(std::io::Cursor::new(many)).is_err());
    }
    #[test]
    fn owner_selector_exists_only_for_reads() {
        use clap::Parser;
        for command in [
            vec!["inbox", "list", "--owner", "cli_agent"],
            vec![
                "inbox",
                "inspect",
                "pkg_x",
                "--owner",
                "cli_agent",
                "--history",
            ],
            vec![
                "inbox",
                "submission",
                "pkg_x",
                "iba_x",
                "--owner",
                "cli_agent",
            ],
        ] {
            assert!(
                crate::cli::Cli::try_parse_from(std::iter::once("zeus").chain(command)).is_ok()
            );
        }
        assert!(
            crate::cli::Cli::try_parse_from([
                "zeus",
                "inbox",
                "submit",
                "pkg_x",
                "--revision",
                "1",
                "--preview",
                "ipv_x",
                "--idempotency-key",
                "x",
                "--owner",
                "cli_agent"
            ])
            .is_err()
        );
    }
}
