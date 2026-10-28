//! Optional, pinned read-only adapter to Zeus's source-built SDK2 broker. Each query gets a private copy
//! of the verified executable/configuration and a total process deadline. No GUI settings, index
//! files, run history, credentials or ambient ES configuration are read or changed by Zeus.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeus_app::failure::{Failure, Result};
use zeus_fs::{Filesystem, RealFs};

use crate::{
    config::{Excluder, norm},
    paths::{RootIdentity, resolve_inside, shown},
    scan,
    search::in_scope,
    store::Row,
};

/// Explicit local helper pin; enabling it never grants new file scopes or agent permissions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Opt in to the external read-only index provider.
    pub enabled: bool,
    /// Absolute local path to a reviewed Zeus Everything broker executable.
    pub helper: String,
    /// Exact SHA-256 approved at this native settings surface.
    pub sha256: String,
    /// Everything instance, commonly `1.5a` for installed alpha releases.
    pub instance: String,
    /// File identity at the time the helper was enabled.
    pub file_id: Option<zeus_types::FileId>,
}
/// A literal filename query within one configured folder, with live offset paging.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// An existing folder inside the current read roots.
    pub root: String,
    /// Literal filename text, at most 512 bytes; never arbitrary CLI flags or query syntax.
    pub text: String,
    /// Live result offset, not a stable snapshot cursor.
    #[serde(default)]
    pub offset: usize,
    /// Name, path, size or modified.
    #[serde(default)]
    pub sort: String,
    /// Reverse the selected sort.
    #[serde(default)]
    pub descending: bool,
}
/// One scope-filtered page from the optional live index.
#[derive(Debug, Serialize)]
pub struct Page {
    /// Provider explicitly differs from Zeus's independent directory-watcher index.
    pub provider: &'static str,
    /// Live IPC offsets can shift when Everything changes; refresh restarts the query.
    pub consistency: &'static str,
    /// Confined current file metadata.
    pub files: Vec<Row>,
    /// Next raw offset, if another raw page exists.
    pub next: Option<usize>,
    /// Excluded, stale, linked or out-of-scope rows discarded without exposing their names.
    pub skipped: usize,
    /// Total execution time including helper launch.
    pub elapsed_ms: u64,
}
/// Optional provider configuration and its private per-query workspace.
pub struct Everything {
    directory: PathBuf,
    settings: Mutex<Settings>,
}
impl Everything {
    /// Load an existing explicit opt-in; a missing or unreadable pin stays disabled.
    pub fn new(directory: PathBuf) -> Self {
        let settings = std::fs::read(directory.join("settings.json"))
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default();
        Self {
            directory,
            settings: Mutex::new(settings),
        }
    }
    /// Return status without launching the broker or querying any existing index.
    pub fn config(&self) -> Value {
        let settings = self.settings.lock().map(|x| x.clone()).unwrap_or_default();
        let suggested = zeus_app::supervisor::bin_dir()
            .ok()
            .map(|dir| dir.join("zeus-everything-query.exe"))
            .filter(|path| path.is_file())
            .map(|path| shown(&path));
        json!({"settings":settings,"supported":cfg!(windows),"provider":"everything_ipc2","independent_usn_backend":false,"suggested_helper":suggested})
    }
    /// Save the person's explicit pin after reading only that executable, without running it.
    pub fn configure(&self, mut settings: Settings) -> Result<Value> {
        if settings.enabled {
            if !cfg!(windows) {
                return Err(Failure::new(
                    "app.everything_unsupported",
                    "Everything IPC is available on Windows. Zeus's own index remains available here.",
                ));
            }
            if settings.instance.len() > 64
                || !settings
                    .instance
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            {
                return Err(Failure::invalid(
                    "Use an Everything instance name of up to 64 letters, numbers, dots, underscores or hyphens.",
                ));
            }
            let inspected = inspect(&settings.helper)?;
            if settings.sha256.len() != 64
                || !settings.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                || inspected["sha256"].as_str()
                    != Some(settings.sha256.to_ascii_lowercase().as_str())
            {
                return Err(Failure::new(
                    "app.helper_changed",
                    "The helper SHA-256 does not match the reviewed executable.",
                ));
            }
            settings.helper = inspected["helper"].as_str().unwrap_or_default().to_owned();
            settings.sha256 = settings.sha256.to_ascii_lowercase();
            settings.file_id = serde_json::from_value(inspected["file_id"].clone()).ok();
        }
        zeus_app::engine_config::write_atomic(
            &self.directory.join("settings.json"),
            &serde_json::to_vec_pretty(&settings)?,
        )?;
        *self
            .settings
            .lock()
            .map_err(|_| Failure::new("app.io", "Everything settings are busy."))? = settings;
        Ok(self.config())
    }
    /// Run one read-only, pre-scoped query; helper failure never substitutes broader results.
    pub async fn query(
        &self,
        request: &Request,
        roots: &[String],
        exclusions: &Excluder,
    ) -> Result<Page> {
        if !cfg!(windows) {
            return Err(Failure::new(
                "app.everything_unsupported",
                "Everything IPC is available on Windows. Use the Zeus index on this platform.",
            ));
        }
        validate_request(request)?;
        let settings = self
            .settings
            .lock()
            .map_err(|_| Failure::new("app.io", "Everything settings are busy."))?
            .clone();
        if !settings.enabled {
            return Err(Failure::new(
                "app.everything_disabled",
                "Enable and pin the Zeus Everything broker in native file-index settings first.",
            ));
        }
        let started = Instant::now();
        let root = resolve_inside(&request.root, roots)?;
        if !root.is_dir() {
            return Err(Failure::invalid(
                "Choose an included folder for this query.",
            ));
        }
        let pin = RootIdentity::capture(&root).ok_or_else(|| {
            Failure::new("app.path_changed", "The selected folder cannot be pinned.")
        })?;
        let inspected = inspect(&settings.helper)?;
        if inspected["sha256"].as_str() != Some(settings.sha256.as_str())
            || serde_json::from_value::<zeus_types::FileId>(inspected["file_id"].clone()).ok()
                != settings.file_id
        {
            return Err(Failure::new(
                "app.helper_changed",
                "The Everything broker executable changed after it was enabled. Review and pin it again.",
            ));
        }
        let query_dir = self
            .directory
            .join(format!("query-{}", zeus_app::review::random_id()));
        std::fs::create_dir_all(&self.directory)?;
        std::fs::create_dir(&query_dir)?;
        let guard = QueryDirectory(query_dir.clone());
        let exe = query_dir.join("zeus-everything-query.exe");
        {
            let source = std::fs::File::open(&settings.helper)?;
            let mut output = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&exe)?;
            let copied = std::io::copy(&mut source.take(64 * 1024 * 1024 + 1), &mut output)?;
            if copied > 64 * 1024 * 1024 {
                return Err(Failure::new(
                    "app.helper_changed",
                    "The helper grew beyond its64MiB bound during staging.",
                ));
            }
            output.sync_all()?;
        }
        if hash_file(&exe)? != settings.sha256 {
            return Err(Failure::new(
                "app.helper_changed",
                "The helper changed while Zeus prepared its private copy.",
            ));
        }
        // The source-built broker has no INI/profile/config code; only the explicit query tuple
        // below can reach its read-only SDK facade.
        let output = query_dir.join("results.json");
        let args = arguments(request, &shown(&root), &settings.instance, &shown(&output));
        let mut command = tokio::process::Command::new(&exe);
        command
            .args(&args)
            .current_dir(&query_dir)
            .env_clear()
            .env("APPDATA", &query_dir)
            .env("LOCALAPPDATA", &query_dir)
            .env("TEMP", &query_dir)
            .env("TMP", &query_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for key in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let mut child = command.spawn().map_err(|_| {
            Failure::new(
                "app.everything_unavailable",
                "The pinned Everything broker could not be started.",
            )
        })?;
        let status = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
            Ok(Ok(status)) => status,
            _ => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(Failure::new(
                    "app.everything_timeout",
                    "The Everything query exceeded ten seconds. The helper was stopped; the Zeus index remains available.",
                ));
            }
        };
        if !status.success() {
            return Err(match status.code() {
                Some(8) => Failure::new(
                    "app.everything_not_running",
                    "The selected Everything instance is not running. Start it yourself or use the Zeus index.",
                ),
                Some(6) => Failure::new(
                    "app.everything_version",
                    "The selected helper does not support the Zeus SDK2 broker v1 contract. Use the source-built Zeus Everything broker.",
                ),
                _ => Failure::new(
                    "app.everything_unavailable",
                    "The Everything query failed. Check the selected instance and helper version; the Zeus index remains available.",
                ),
            });
        }
        if !pin.current() {
            return Err(Failure::new(
                "app.path_changed",
                "The selected folder changed during the query.",
            ));
        }
        let metadata = std::fs::symlink_metadata(&output)?;
        if !metadata.is_file() || scan::is_link(&metadata) || metadata.len() > 8 * 1024 * 1024 {
            return Err(Failure::new(
                "app.everything_output",
                "The broker response exceeded its 8 MiB bound or changed type.",
            ));
        }
        let bytes = std::fs::read(&output)?;
        let mut page = parse_page(&bytes, request, roots, exclusions)?;
        page.elapsed_ms = started.elapsed().as_millis() as u64;
        drop(guard);
        Ok(page)
    }
}
struct QueryDirectory(PathBuf);
impl Drop for QueryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Read/hash only the explicitly selected helper executable; do not execute or auto-trust it.
pub fn inspect(path: &str) -> Result<Value> {
    let source = Path::new(path);
    if !source.is_absolute()
        || source
            .extension()
            .is_none_or(|e| !e.eq_ignore_ascii_case("exe"))
        || path.contains('\0')
    {
        return Err(Failure::invalid(
            "Choose an absolute path to an Everything broker executable (.exe).",
        ));
    }
    #[cfg(windows)]
    if !matches!(source.components().next(),Some(std::path::Component::Prefix(p)) if matches!(p.kind(),std::path::Prefix::Disk(_)|std::path::Prefix::VerbatimDisk(_)))
    {
        return Err(Failure::invalid(
            "Choose an Everything broker executable on a local drive, rather than a network share.",
        ));
    }
    let canonical = std::fs::canonicalize(source)?;
    let m = RealFs
        .metadata(&canonical)
        .map_err(|_| {
            Failure::new(
                "app.helper_changed",
                "The helper metadata could not be read.",
            )
        })?
        .ok_or_else(|| Failure::new("app.helper_changed", "The helper is missing."))?;
    if !m.is_regular_file || m.is_symlink || m.len > 64 * 1024 * 1024 || m.file_id.is_none() {
        return Err(Failure::invalid(
            "The helper must be an identified regular executable of at most 64 MiB.",
        ));
    }
    let sha256 = hash_file(&canonical)?;
    if RealFs.metadata(&canonical).ok().flatten().as_ref() != Some(&m) {
        return Err(Failure::new(
            "app.helper_changed",
            "The helper changed during inspection.",
        ));
    }
    Ok(
        json!({"helper":shown(&canonical),"sha256":sha256,"file_id":m.file_id,"bytes":m.len,"modified_ms":m.mtime_ms}),
    )
}
fn hash_file(path: &Path) -> Result<String> {
    let mut input = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut read = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        if read > 64 * 1024 * 1024 {
            return Err(Failure::invalid("The helper exceeds 64 MiB."));
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn validate_request(request: &Request) -> Result<()> {
    if request.text.len() > 512
        || request.text.chars().any(char::is_control)
        || request.offset > 1_000_000
        || !["", "name", "path", "size", "modified"].contains(&request.sort.as_str())
    {
        return Err(Failure::invalid(
            "Use literal filename text up to 512 bytes, supported ordering and an offset below 1,000,000.",
        ));
    }
    Ok(())
}
fn arguments(request: &Request, root: &str, instance: &str, output: &str) -> Vec<String> {
    let field = match request.sort.as_str() {
        "path" => 2,
        "size" => 4,
        "modified" => 6,
        _ => 0,
    };
    let sort = field + usize::from(request.descending);
    let literal = if request.text.is_empty() {
        "(?:)".into()
    } else {
        request
            .text
            .chars()
            .map(|ch| format!("\\x{{{:x}}}", ch as u32))
            .collect::<String>()
    };
    vec![
        "--zeus-everything-sdk2-v1".into(),
        root.into(),
        literal,
        instance.into(),
        request.offset.to_string(),
        sort.to_string(),
        output.into(),
    ]
}

fn parse_page(
    bytes: &[u8],
    request: &Request,
    roots: &[String],
    exclusions: &Excluder,
) -> Result<Page> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        Failure::new(
            "app.everything_output",
            "The Everything broker did not return the expected JSON format.",
        )
    })?;
    let rows = value.as_array().ok_or_else(|| {
        Failure::new(
            "app.everything_output",
            "The broker response must be a bounded row array.",
        )
    })?;
    if rows.len() > 501 {
        return Err(Failure::new(
            "app.everything_output",
            "The broker response exceeded 501 requested rows.",
        ));
    }
    let scopes: Vec<_> = roots.iter().map(|r| norm(r)).collect();
    let query_root = norm(&request.root);
    let mut files = Vec::new();
    let mut skipped = 0;
    let mut seen = std::collections::HashSet::new();
    for row in rows.iter().take(500) {
        let Some(path) = [
            "filename",
            "full_path",
            "full_path_and_name",
            "path_and_name",
        ]
        .iter()
        .find_map(|key| row.get(key).and_then(Value::as_str)) else {
            return Err(Failure::new(
                "app.everything_output",
                "The broker response is missing its full-path column.",
            ));
        };
        if path.len() > 32768
            || !in_scope(path, &scopes)
            || !in_scope(path, std::slice::from_ref(&query_root))
        {
            skipped += 1;
            continue;
        }
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if !name.to_lowercase().contains(&request.text.to_lowercase()) {
            skipped += 1;
            continue;
        }
        let Ok(resolved) = resolve_inside(path, roots) else {
            skipped += 1;
            continue;
        };
        let shown = shown(&resolved);
        if !in_scope(&shown, std::slice::from_ref(&query_root))
            || norm(path) != norm(&shown)
            || !seen.insert(norm(&shown))
        {
            skipped += 1;
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            skipped += 1;
            continue;
        };
        if scan::is_link(&metadata)
            || (!metadata.is_file() && !metadata.is_dir())
            || !roots.iter().any(|root| {
                in_scope(path, std::slice::from_ref(&norm(root)))
                    && !exclusions.skips_below(Path::new(path), Path::new(root))
                    && !exclusions.skips_by_attributes_below(Path::new(path), Path::new(root))
            })
        {
            skipped += 1;
            continue;
        }
        files.push(Row {
            path: shown,
            size: if metadata.is_dir() { 0 } else { metadata.len() },
            mtime: scan::mtime_ms(&metadata),
            is_dir: metadata.is_dir(),
        });
    }
    Ok(Page {
        provider: "everything_ipc2",
        consistency: "live",
        files,
        next: (rows.len() == 501 && request.offset + 500 <= 1_000_000)
            .then_some(request.offset + 500),
        skipped,
        elapsed_ms: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn argument_plan_cannot_turn_text_into_options_or_expand_the_query_scope() {
        let request = Request {
            root: "D:\\Chosen Root".into(),
            text: "-exit | \\\"secret\\\"".into(),
            offset: 500,
            sort: "modified".into(),
            descending: true,
        };
        let args = arguments(&request, &request.root, "1.5a", "D:\\private\\results.json");
        assert_eq!(args[1], "D:\\Chosen Root");
        assert!(!args.iter().any(|v| v == "-exit"));
        assert_eq!(args[0], "--zeus-everything-sdk2-v1");
        let regex = &args[2];
        assert!(!regex.contains([' ', '"', '|']));
        assert!(regex.contains("\\x{7c}"));
    }
    #[test]
    fn private_outside_stale_linked_and_excluded_rows_do_not_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("visible.txt"), b"visible").unwrap();
        std::fs::write(root.join("node_modules/hidden.txt"), b"hidden").unwrap();
        let outside = dir.path().join("private.txt");
        std::fs::write(&outside, b"private").unwrap();
        let request = Request {
            root: shown(&root),
            text: String::new(),
            offset: 0,
            sort: String::new(),
            descending: false,
        };
        let value = json!([{"filename":shown(&root.join("visible.txt"))},{"filename":shown(&outside)},{"filename":shown(&root.join("missing.txt"))},{"filename":shown(&root.join("node_modules/hidden.txt"))}]);
        let page = parse_page(
            &serde_json::to_vec(&value).unwrap(),
            &request,
            &[shown(&root)],
            &Excluder::new(&crate::config::IndexConfig::default()),
        )
        .unwrap();
        assert_eq!(page.files.len(), 1);
        assert_eq!(page.skipped, 3);
        assert_eq!(page.files[0].size, 7);
    }
    #[test]
    fn helper_inspection_detects_byte_replacement_without_executing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("es.exe");
        std::fs::write(&file, b"not executable fixture").unwrap();
        let first = inspect(&shown(&file)).unwrap();
        std::fs::write(&file, b"replacement").unwrap();
        let next = inspect(&shown(&file)).unwrap();
        assert_ne!(first["sha256"], next["sha256"]);
    }
}
