//! Registered executable checks for human decision channels.
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zeus_api::{ApiError, ApiResult, CallContext, ClientRole};
use zeus_types::Surface;

#[derive(Debug, Serialize, Deserialize)]
struct Program {
    role: ClientRole,
    path: PathBuf,
    blake3: String,
}

/// Immutable executable pins loaded when the daemon starts.
#[derive(Debug, Serialize, Deserialize)]
pub struct TrustedPrograms {
    programs: Vec<Program>,
}

fn digest(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn installed_candidates(directory: &Path) -> ApiResult<TrustedPrograms> {
    let mut programs = Vec::new();
    for (role, name) in [
        (ClientRole::Cli, "zeus"),
        (ClientRole::Tui, "zeus"),
        (ClientRole::NativeHost, "zeus-native-host"),
    ] {
        let path=directory.join(format!("{name}{}",std::env::consts::EXE_SUFFIX)).canonicalize()
            .map_err(|_|ApiError::new("api.unavailable","Install both zeus and zeus-native-host beside zeusd before registering trusted clients"))?;
        if !path.is_file() {
            return Err(ApiError::invalid_params(
                "installed client is not a regular file",
            ));
        }
        programs.push(Program {
            role,
            blake3: digest(&path)
                .map_err(|_| ApiError::internal("cannot hash installed client"))?,
            path,
        });
    }
    Ok(TrustedPrograms { programs })
}

fn installed_directory() -> ApiResult<PathBuf> {
    std::env::current_exe()
        .map_err(|_| ApiError::internal("cannot locate installed daemon"))?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| ApiError::internal("installed daemon has no directory"))
}

fn read_pins(path: &Path) -> ApiResult<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ApiError::internal("cannot inspect executable pins")),
        Ok(metadata) => {
            if metadata.len() > 65536 {
                return Err(ApiError::invalid_params(
                    "executable pins exceed size limit",
                ));
            }
            zeus_api::transport::secure_private_file(path)
                .map_err(|_| ApiError::internal("executable pins are not private"))?;
            std::fs::read(path)
                .map(Some)
                .map_err(|_| ApiError::internal("cannot read executable pins"))
        }
    }
}

fn create_private(path: &Path) -> ApiResult<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|_| ApiError::internal("cannot create private pin file"))?;
    zeus_api::transport::secure_private_file(path)
        .map_err(|_| ApiError::internal("cannot protect pin file"))?;
    Ok(file)
}

impl TrustedPrograms {
    /// Register a complete installed package on explicit first-run initialization. Existing pins
    /// are preserved, including when an installed executable has changed.
    pub fn initialize_installed(state: &Path) -> ApiResult<()> {
        use zeus_fs::Filesystem;
        let path = state.join("trusted-programs.json");
        if let Some(bytes) = read_pins(&path)? {
            let _: Self = serde_json::from_slice(&bytes)
                .map_err(|_| ApiError::internal("invalid stored executable pins"))?;
            return Ok(());
        }
        let candidates = installed_candidates(&installed_directory()?)?;
        let bytes = serde_json::to_vec_pretty(&candidates)
            .map_err(|_| ApiError::internal("cannot encode executable pins"))?;
        let mut file = create_private(&path)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| ApiError::internal("cannot persist executable pins"))?;
        zeus_fs::RealFs
            .sync_dir(state)
            .map_err(|_| ApiError::internal("cannot sync pin directory"))?;
        Ok(())
    }
}

fn terminal_text(value: &str) -> String {
    value.chars().filter(|c|!c.is_control() && !matches!(c,'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2060}'..='\u{2069}'|'\u{feff}')).collect()
}

fn open_review_terminal() -> ApiResult<(std::fs::File, zeus_api::transport::TerminalOutput)> {
    use std::io::IsTerminal;
    #[cfg(windows)]
    let (input_path, output_path) = ("CONIN$", "CONOUT$");
    #[cfg(not(windows))]
    let (input_path, output_path) = ("/dev/tty", "/dev/tty");
    let input = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(input_path)
        .map_err(|_| ApiError::human_required("controlling-terminal executable review"))?;
    // Windows IsTerminal uses GetConsoleMode, which requires GENERIC_READ even on CONOUT$.
    // Keep the controlling-console check; a detached process must still be refused.
    let output = std::fs::OpenOptions::new()
        .read(cfg!(windows))
        .write(true)
        .open(output_path)
        .map_err(|_| ApiError::human_required("controlling-terminal executable review"))?;
    if !input.is_terminal() || !output.is_terminal() {
        return Err(ApiError::human_required(
            "controlling-terminal executable review",
        ));
    }
    let output = zeus_api::transport::TerminalOutput::new(output)
        .map_err(|_| ApiError::human_required("controlling-terminal executable review"))?;
    Ok((input, output))
}

/// Explicit offline recovery after an installed client upgrade. The normal startup path never
/// repins changed binaries. This function holds the same state lock as `zeusd run` throughout.
pub fn refresh_offline(paths: &zeus_api::endpoint::ZeusPaths) -> ApiResult<()> {
    use std::io::{BufRead, BufReader};
    zeus_api::transport::secure_private_directory(&paths.state_dir)
        .map_err(|_| ApiError::internal("private initialized state directory required"))?;
    // Open the same inode used by the daemon; replacing a held lock file is never allowed.
    let lock_path = paths.lock_file();
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if lock_path.exists() {
        zeus_api::transport::secure_private_file(&lock_path)
            .map_err(|_| ApiError::internal("invalid daemon lock file"))?;
    }
    let lock = options
        .open(&lock_path)
        .map_err(|_| ApiError::internal("cannot open daemon lock"))?;
    zeus_api::transport::secure_private_file(&lock_path)
        .map_err(|_| ApiError::internal("cannot protect daemon lock"))?;
    lock.try_lock()
        .map_err(|_| ApiError::new("api.conflict", "Stop zeusd before updating trusted clients"))?;
    let path = paths.state_dir.join("trusted-programs.json");
    let old = read_pins(&path)?;
    let candidates = installed_candidates(&installed_directory()?)?;
    let bytes = serde_json::to_vec_pretty(&candidates)
        .map_err(|_| ApiError::internal("cannot encode executable pins"))?;
    let (mut input, mut output) = open_review_terminal()?;
    writeln!(output,"Trusted client upgrade review\nState: {}\nThe daemon must remain stopped. These executables will be allowed to present human decisions.",terminal_text(&paths.state_dir.display().to_string())).map_err(|_|ApiError::internal("cannot display trust review"))?;
    if let Some(previous) = &old {
        let pins: TrustedPrograms = serde_json::from_slice(previous)
            .map_err(|_| ApiError::internal("invalid old pins; no change was made"))?;
        writeln!(output, "Previously registered:")
            .map_err(|_| ApiError::internal("cannot display trust review"))?;
        for pin in pins.programs {
            writeln!(
                output,
                "  {:?}  {}\n    BLAKE3 {}",
                pin.role,
                terminal_text(&pin.path.display().to_string()),
                pin.blake3
            )
            .map_err(|_| ApiError::internal("cannot display trust review"))?;
        }
    } else {
        writeln!(output, "No existing client registration.")
            .map_err(|_| ApiError::internal("cannot display trust review"))?;
    }
    writeln!(output, "Proposed registration:")
        .map_err(|_| ApiError::internal("cannot display trust review"))?;
    for pin in &candidates.programs {
        writeln!(
            output,
            "  {:?}  {}\n    BLAKE3 {}",
            pin.role,
            terminal_text(&pin.path.display().to_string()),
            pin.blake3
        )
        .map_err(|_| ApiError::internal("cannot display trust review"))?;
    }
    let nonce = zeus_api::auth::Nonce::generate();
    let confirmation = format!("trust {}", &nonce.as_str()[..12]);
    writeln!(output,"Review every path and digest, then type {confirmation} to replace the registration. Enter cancels.").and_then(|_|output.flush()).map_err(|_|ApiError::internal("cannot display trust confirmation"))?;
    std::thread::sleep(std::time::Duration::from_millis(750));
    zeus_api::transport::discard_terminal_input(&input)
        .map_err(|_| ApiError::human_required("fresh terminal input"))?;
    let mut text = String::new();
    let n = BufReader::new(&mut input)
        .take(256)
        .read_line(&mut text)
        .map_err(|_| ApiError::human_required("fresh terminal input"))?;
    if n == 0 || n >= 256 || text.trim() != confirmation {
        return Err(ApiError::new(
            "api.canceled",
            "Executable registration canceled; existing pins preserved",
        ));
    }
    // Rehash after human review; binaries changed during review cannot inherit the decision.
    let current = serde_json::to_vec_pretty(&installed_candidates(&installed_directory()?)?)
        .map_err(|_| ApiError::internal("cannot encode executable pins"))?;
    if current != bytes || read_pins(&path)? != old {
        return Err(ApiError::new(
            "api.conflict",
            "Installed clients or pins changed during review; review again",
        ));
    }
    replace_pins_audited(&paths.state_dir, &path, old.as_deref(), &bytes)?;
    writeln!(
        output,
        "Trusted client registration updated. Start zeusd when ready."
    )
    .map_err(|_| ApiError::internal("cannot display trust result"))?;
    Ok(())
}

fn replace_pins_audited(
    state: &Path,
    path: &Path,
    old: Option<&[u8]>,
    bytes: &[u8],
) -> ApiResult<()> {
    use zeus_fs::Filesystem;
    use zeus_verify::audit::{AuditEntry, AuditLog, AuditOutcome, AuditSink, VerifyOptions};
    let journal = zeus_journal::Journal::open(
        &state.join("journal.redb"),
        &Default::default(),
        std::sync::Arc::new(zeus_control::SystemClock::new()),
    )
    .map_err(|_| ApiError::internal("cannot exclusively open state journal"))?;
    let meta = journal
        .meta()
        .map_err(|_| ApiError::internal("cannot read journal anchor"))?;
    let audit_dir = state.join("audit");
    std::fs::create_dir_all(&audit_dir)
        .map_err(|_| ApiError::internal("cannot open audit directory"))?;
    zeus_api::transport::secure_private_directory(&audit_dir)
        .map_err(|_| ApiError::internal("cannot protect audit directory"))?;
    let audit = AuditLog::open(
        &audit_dir,
        meta.audit_head.as_ref().map(|h| h.chain_id),
        meta.store_id,
        env!("CARGO_PKG_VERSION"),
        Default::default(),
    )
    .map_err(|_| ApiError::internal("cannot open audit log"))?;
    audit
        .verify_snapshot(&VerifyOptions {
            journal_head: meta.audit_head,
            ..Default::default()
        })
        .map_err(|_| ApiError::internal("audit chain does not match journal anchor"))?;
    let operation = zeus_api::auth::Nonce::generate();
    let record = |phase: &str| -> ApiResult<()> {
        let mut entry = AuditEntry::new(
            zeus_types::Timestamp::now(),
            zeus_types::Actor::human(Surface::CliInteractive),
            "config.changed",
            AuditOutcome::Ok,
        );
        entry
            .data
            .insert("key".into(), serde_json::json!("trusted_programs"));
        entry.data.insert("phase".into(), serde_json::json!(phase));
        entry
            .data
            .insert("operation_id".into(), serde_json::json!(operation.as_str()));
        entry.data.insert(
            "old_blake3".into(),
            serde_json::json!(old.map(|v| blake3::hash(v).to_hex().to_string())),
        );
        entry.data.insert(
            "new_blake3".into(),
            serde_json::json!(blake3::hash(bytes).to_hex().to_string()),
        );
        audit
            .append(entry)
            .map_err(|_| ApiError::internal("cannot audit executable registration"))?;
        let head = audit
            .head()
            .map_err(|_| ApiError::internal("cannot read audit head"))?;
        journal
            .transact(|tx| tx.set_audit_head(head))
            .map_err(|_| ApiError::internal("cannot persist audit anchor"))?;
        Ok(())
    };
    let temp = state.join(format!(".trusted-programs-{}.tmp", operation.as_str()));
    let backup = state.join(format!(
        "trusted-programs-{}.previous.json",
        operation.as_str()
    ));
    let result = (|| {
        let mut file = create_private(&temp)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| ApiError::internal("cannot write proposed executable registration"))?;
        drop(file);
        record("intent")?;
        if old.is_some() {
            zeus_fs::RealFs
                .replace_with_backup(&temp, path, &backup)
                .map_err(|_| ApiError::internal("cannot atomically replace executable pins"))?;
        } else {
            zeus_fs::RealFs
                .rename_noreplace(&temp, path)
                .map_err(|_| ApiError::internal("cannot atomically install executable pins"))?;
        }
        zeus_fs::RealFs
            .sync_dir(state)
            .map_err(|_| ApiError::internal("cannot sync executable registration"))?;
        zeus_api::transport::secure_private_file(path)
            .map_err(|_| ApiError::internal("cannot protect updated executable registration"))?;
        record("committed")
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

impl TrustedPrograms {
    /// Load installer-owned executable pins. Missing pins allow only unattended authority.
    pub fn load_or_initialize(state: &Path) -> ApiResult<Self> {
        let path = state.join("trusted-programs.json");
        if !path.exists() {
            return Ok(Self {
                programs: Vec::new(),
            });
        }
        zeus_api::transport::secure_private_file(&path)
            .map_err(|_| ApiError::internal("cannot secure executable pins"))?;
        serde_json::from_slice(
            &std::fs::read(path).map_err(|_| ApiError::internal("cannot read executable pins"))?,
        )
        .map_err(|_| ApiError::internal("invalid executable pins"))
    }

    /// Check the OS-observed executable against both its installed path and digest.
    pub fn verified(&self, ctx: &CallContext) -> bool {
        let Some(path) = ctx
            .peer
            .executable
            .as_ref()
            .and_then(|p| p.canonicalize().ok())
        else {
            return false;
        };
        self.programs.iter().any(|p| {
            p.role == ctx.identity.role
                && p.path == path
                && digest(&path).is_ok_and(|hash| hash == p.blake3)
        })
    }

    /// Evidence derived from the observed peer after checking the terminal channel.
    pub(crate) fn terminal_evidence(
        &self,
        ctx: &CallContext,
        surface: Surface,
        confirmation: zeus_api::methods::sessions::ConfirmationLevel,
    ) -> ApiResult<serde_json::Value> {
        self.terminal(ctx, surface)?;
        Ok(
            serde_json::json!({"surface":surface,"role":ctx.identity.role,"client_id":ctx.identity.credential_id.as_str(),
            "credential_kind":ctx.identity.kind,"effective_kind":ctx.identity.kind,"exe_verified":true,"peer_pid":ctx.peer.pid,
            "tty":if cfg!(windows){"asserted"}else{"verified"},"confirmation":confirmation}),
        )
    }

    /// Require a verified terminal; browser decisions use the additional native-messaging attestation path.
    pub fn terminal(&self, ctx: &CallContext, surface: Surface) -> ApiResult<zeus_types::Actor> {
        let actor = ctx.identity.decision_actor(surface)?;
        if !matches!(
            (ctx.identity.role, surface),
            (ClientRole::Cli, Surface::CliInteractive) | (ClientRole::Tui, Surface::Tui)
        ) || !ctx.peer.interactive
            || !self.verified(ctx)
        {
            return Err(ApiError::human_required("verified terminal decision"));
        }
        Ok(actor)
    }
}

#[cfg(test)]
mod offline_tests {
    use super::*;
    #[cfg(windows)]
    mod console_regression {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/test-support/windows-console.rs"
        ));
    }

    #[cfg(windows)]
    #[test]
    fn offline_review_requires_a_real_controlling_console() {
        console_regression::assert_console_gate(
            "trust::offline_tests::offline_review_requires_a_real_controlling_console",
            || open_review_terminal().is_ok(),
        );
    }

    #[test]
    fn incomplete_installation_is_refused_and_candidates_bind_exact_paths_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        assert!(installed_candidates(dir.path()).is_err());
        let cli = dir
            .path()
            .join(format!("zeus{}", std::env::consts::EXE_SUFFIX));
        let host = dir
            .path()
            .join(format!("zeus-native-host{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&cli, b"cli-v1").unwrap();
        assert!(installed_candidates(dir.path()).is_err());
        std::fs::write(&host, b"host-v1").unwrap();
        let pins = installed_candidates(dir.path()).unwrap();
        assert_eq!(pins.programs.len(), 3);
        assert_eq!(pins.programs[0].path, cli.canonicalize().unwrap());
        assert_eq!(
            pins.programs[0].blake3,
            blake3::hash(b"cli-v1").to_hex().to_string()
        );
        std::fs::write(&cli, b"cli-v2").unwrap();
        assert_ne!(
            pins.programs[0].blake3,
            installed_candidates(dir.path()).unwrap().programs[0].blake3
        );
    }
    #[test]
    fn explicit_init_preserves_existing_registration_and_startup_does_not_create_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted-programs.json");
        let _ = TrustedPrograms::load_or_initialize(dir.path()).unwrap();
        assert!(!path.exists());
        std::fs::write(&path, b"{\"programs\":[]}").unwrap();
        TrustedPrograms::initialize_installed(dir.path()).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"{\"programs\":[]}");
    }
    #[test]
    fn upgrade_is_atomic_audited_and_anchored_in_the_real_journal() {
        use zeus_verify::audit::AuditSink;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted-programs.json");
        let old = b"{\"programs\":[]}";
        let new = b"{\"programs\":[],\"test_revision\":2}";
        std::fs::write(&path, old).unwrap();
        zeus_api::transport::secure_private_file(&path).unwrap();
        replace_pins_audited(dir.path(), &path, Some(old), new).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), new);
        let clock = std::sync::Arc::new(zeus_control::SystemClock::new());
        let journal = zeus_journal::Journal::open(
            &dir.path().join("journal.redb"),
            &Default::default(),
            clock,
        )
        .unwrap();
        let meta = journal.meta().unwrap();
        let audit = zeus_verify::audit::AuditLog::open(
            &dir.path().join("audit"),
            None,
            meta.store_id,
            "test",
            Default::default(),
        )
        .unwrap();
        assert_eq!(meta.audit_head, audit.head().unwrap());
        let (records, _) = audit.tail(0, 100).unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|r| r.data.get("key") == Some(&serde_json::json!("trusted_programs")))
                .count(),
            2
        );
        assert_eq!(records.last().unwrap().data["phase"], "committed");
        audit
            .verify_snapshot(&zeus_verify::audit::VerifyOptions {
                journal_head: meta.audit_head,
                ..Default::default()
            })
            .unwrap();
    }
    #[test]
    fn running_daemon_lock_blocks_offline_review_before_terminal_input() {
        let dir = tempfile::tempdir().unwrap();
        let paths = zeus_api::endpoint::ZeusPaths {
            state_dir: dir.path().to_owned(),
            config_dir: dir.path().to_owned(),
            runtime_dir: dir.path().to_owned(),
            user_identity: "test".into(),
            profile: None,
        };
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(paths.lock_file())
            .unwrap();
        lock.try_lock().unwrap();
        assert_eq!(
            refresh_offline(&paths)
                .unwrap_err()
                .envelope()
                .code
                .as_str(),
            "api.conflict"
        );
    }
}
