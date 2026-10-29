//! Rule-based file organisation with approval-bound execution and undo (05 section 9.6, 07 section 4.4).
//!
//! `files.organize` plans moves, renames and trash operations inside one directory from a rule set. A dry run
//! returns the steps and stores nothing. Applying stores the exact operation list, binds a `destructive_op`
//! approval to its BLAKE3 and returns; nothing moves until a person approves at a verified terminal. The
//! maintenance tick then executes the approved list step by step. Each step re-checks the planned file (size and
//! modification time), stays inside its write root, never overwrites (no-replace renames), sends deletions to the
//! application trash, and is recorded before it runs, so a restart confirms or resumes it instead of repeating
//! it. `files.undo` plans the inverse of the steps that ran and goes through the same approval.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use zeus_api::methods::files::FileStep;
use zeus_api::{ApiError, ApiResult, ClientIdentity};
use zeus_types::{ApprovalId, ApprovalState, Hex256, Timestamp, ToEnvelope};
use zeus_verify::audit::{AuditEntry, AuditOutcome, AuditSink};

const OPERATIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("file_operations_v1");
/// Routing rules per rule set (the 08 limit for `post.rules`).
pub const MAX_RULES: usize = 16;
/// Files one plan may examine.
pub const MAX_FILES_SCANNED: usize = 10_000;
/// Steps one operation may contain.
pub const MAX_STEPS: usize = 1_000;
/// Steps the maintenance tick executes at once, so a large operation never stalls the daemon.
const STEPS_PER_TICK: usize = 64;
/// Directory components a `subdir` template may produce (08 naming rules).
const MAX_SUBDIR_COMPONENTS: usize = 8;

fn fail() -> ApiError {
    ApiError::internal("file operation store unavailable")
}
fn invalid(message: &str) -> ApiError {
    ApiError::invalid_params(message.to_owned())
}
fn outside() -> ApiError {
    ApiError::new(
        zeus_types::codes::POLICY_PATH_OUTSIDE_ROOTS,
        "the directory is outside the roots visible to this client",
    )
}

/// A rule set. Untrusted input: unknown members are refused, regular expressions are size-limited.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleSet {
    /// Also organise files in sub-directories of `dir`.
    #[serde(default)]
    recursive: bool,
    /// 1 to 16 rules; the first whose `when` holds decides a file's step. Files no rule matches stay.
    rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    #[serde(default)]
    when: When,
    /// Relative directory template below `dir`, for example `papers/{year}`.
    #[serde(default)]
    subdir: Option<String>,
    /// File name template, for example `{stem}-{year}{month}.{ext}`.
    #[serde(default)]
    rename: Option<String>,
    /// Send matching files to the application trash.
    #[serde(default)]
    trash: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct When {
    /// Extensions without the dot, compared case-insensitively.
    #[serde(default)]
    ext: Vec<String>,
    /// Regular expression over the file name.
    #[serde(default)]
    name_regex: Option<String>,
    #[serde(default)]
    min_bytes: Option<u64>,
    #[serde(default)]
    max_bytes: Option<u64>,
    /// Last modified at least this many days ago.
    #[serde(default)]
    older_than_days: Option<u32>,
}

struct CompiledRule {
    rule: Rule,
    ext: BTreeSet<String>,
    name: Option<regex::Regex>,
}

fn template_ok(template: &str) -> bool {
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            return false;
        };
        if !matches!(
            &rest[start + 1..start + end],
            "name" | "stem" | "ext" | "year" | "month" | "day"
        ) {
            return false;
        }
        rest = &rest[start + end + 1..];
    }
    !rest.contains('}')
}

fn compile(rules: &serde_json::Value) -> ApiResult<(bool, Vec<CompiledRule>)> {
    let set: RuleSet = serde_json::from_value(rules.clone()).map_err(|_| {
        invalid("rules must be a rule set: {recursive?, rules: [{when, subdir?, rename?, trash?}]}")
    })?;
    if set.rules.is_empty() || set.rules.len() > MAX_RULES {
        return Err(invalid("a rule set has 1 to 16 rules"));
    }
    let mut compiled = Vec::new();
    for rule in set.rules {
        let moves = rule.subdir.is_some() || rule.rename.is_some();
        if rule.trash == moves {
            return Err(invalid(
                "each rule either moves or renames (subdir, rename) or sends to the trash",
            ));
        }
        for template in rule.subdir.iter().chain(&rule.rename) {
            if template.is_empty() || template.len() > 255 || !template_ok(template) {
                return Err(invalid(
                    "templates use only {name}, {stem}, {ext}, {year}, {month} and {day}",
                ));
            }
        }
        if rule.subdir.as_ref().is_some_and(|s| {
            s.starts_with('/') || s.contains('\\') || s.split('/').any(|c| c == ".." || c == ".")
        }) {
            return Err(invalid(
                "subdir is relative and cannot climb out of the directory",
            ));
        }
        let name = rule
            .when
            .name_regex
            .as_deref()
            .map(|pattern| {
                regex::RegexBuilder::new(pattern)
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|_| invalid("name_regex is not a valid bounded regular expression"))
            })
            .transpose()?;
        let ext = rule
            .when
            .ext
            .iter()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .collect();
        compiled.push(CompiledRule { rule, ext, name });
    }
    Ok((set.recursive, compiled))
}

struct Candidate {
    path: PathBuf,
    name: String,
    size: u64,
    modified_ms: i64,
}

fn render(template: &str, file: &Candidate) -> String {
    let path = Path::new(&file.name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&file.name);
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let date = time::OffsetDateTime::from_unix_timestamp(file.modified_ms.div_euclid(1000))
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    template
        .replace("{name}", &file.name)
        .replace("{stem}", stem)
        .replace("{ext}", ext)
        .replace("{year}", &format!("{:04}", date.year()))
        .replace("{month}", &format!("{:02}", u8::from(date.month())))
        .replace("{day}", &format!("{:02}", date.day()))
}

fn sanitized(component: &str) -> Option<String> {
    // A field value never creates directories: separators become `_` before sanitising (08 naming rules).
    let component = component.replace(['/', '\\'], "_");
    zeus_fs::sanitize_filename(&component, &Default::default())
        .ok()
        .map(|n| n.name)
        .filter(|n| !n.is_empty() && n != "." && n != "..")
}

/// One step of an operation list. The list is hashed into `operation_set_blake3`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PlannedStep {
    op: StepOp,
    from: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<PathBuf>,
    size_bytes: u64,
    modified_ms: i64,
    quarantined: bool,
    /// Undo of a trash step: the record that restores the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restore: Option<zeus_fs::trash::TrashEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StepOp {
    Move,
    Rename,
    Trash,
    Restore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
enum StepState {
    Pending,
    /// Recorded before the effect; a restart inspects the files to decide whether it happened.
    Started,
    Done {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trash: Option<zeus_fs::trash::TrashEntry>,
    },
    Skipped {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
enum OperationState {
    AwaitingApproval,
    Applying,
    Applied,
    /// Denied, expired, withdrawn, or a binding that no longer matches: nothing (more) ran.
    Dropped {
        reason: String,
    },
    /// Interrupted by the kill switch; finished steps stay recorded for undo.
    Stopped {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OperationRecord {
    v: u8,
    operation_id: String,
    owner: String,
    root: PathBuf,
    dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    undo_of: Option<String>,
    created_at: Timestamp,
    steps: Vec<PlannedStep>,
    operation_set_blake3: Hex256,
    approval_id: ApprovalId,
    state: OperationState,
    outcomes: Vec<StepState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    undone_by: Option<String>,
}

/// A plan before it is stored.
pub(crate) struct Plan {
    root: PathBuf,
    dir: PathBuf,
    steps: Vec<PlannedStep>,
    undo_of: Option<String>,
}

impl Plan {
    /// The steps as the API shows them: paths relative to the write root.
    pub(crate) fn view(&self) -> Vec<FileStep> {
        self.steps.iter().map(|s| view(&self.root, s)).collect()
    }
    /// True when nothing would change.
    pub(crate) fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn view(root: &Path, step: &PlannedStep) -> FileStep {
    let (op, from) = match step.op {
        StepOp::Move => ("move", relative(root, &step.from)),
        StepOp::Rename => ("rename", relative(root, &step.from)),
        StepOp::Trash => ("trash", relative(root, &step.from)),
        StepOp::Restore => (
            "move",
            format!(
                "trash:{}",
                step.restore
                    .as_ref()
                    .map(|e| e.id.as_str())
                    .unwrap_or_default()
            ),
        ),
    };
    FileStep {
        op: op.into(),
        from,
        to: step.to.as_deref().map(|to| relative(root, to)),
        destructive: step.op == StepOp::Trash,
    }
}

fn operation_hash(steps: &[PlannedStep]) -> ApiResult<Hex256> {
    let value = serde_json::to_value(steps).map_err(|_| fail())?;
    zeus_verify::canonical::request_hash(&value).map_err(|_| fail())
}

fn modified_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// Durable file-operation records and their executor.
pub struct Organizer {
    db: Database,
    trash: zeus_fs::trash::Trash,
    journal: zeus_journal::Journal,
    consent: Arc<crate::consent::ConsentManager>,
    audit: Arc<dyn AuditSink>,
    gate: std::sync::Mutex<()>,
}

impl Organizer {
    /// Open `<state>/files.redb` and the application trash `<state>/trash`.
    pub fn open(
        state: &Path,
        journal: zeus_journal::Journal,
        consent: Arc<crate::consent::ConsentManager>,
        audit: Arc<dyn AuditSink>,
    ) -> ApiResult<Self> {
        let path = state.join("files.redb");
        if path.is_symlink() {
            return Err(fail());
        }
        let db = Database::create(&path).map_err(|_| fail())?;
        let tx = db.begin_write().map_err(|_| fail())?;
        tx.open_table(OPERATIONS).map_err(|_| fail())?;
        tx.commit().map_err(|_| fail())?;
        Ok(Self {
            db,
            trash: zeus_fs::trash::Trash::new(state.join("trash")),
            journal,
            consent,
            audit,
            gate: Default::default(),
        })
    }

    fn get(&self, id: &str) -> ApiResult<Option<OperationRecord>> {
        let tx = self.db.begin_read().map_err(|_| fail())?;
        let table = tx.open_table(OPERATIONS).map_err(|_| fail())?;
        table
            .get(id)
            .map_err(|_| fail())?
            .map(|v| serde_json::from_slice(v.value()).map_err(|_| fail()))
            .transpose()
    }

    /// Read a bounded progress projection; never disclose another agent's operation.
    pub(crate) fn operation_view(
        &self,
        identity: &ClientIdentity,
        roots: &[PathBuf],
        id: &str,
    ) -> ApiResult<zeus_api::methods::files::FilesOperationGetResult> {
        if id.is_empty()
            || id.len() > 64
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(invalid("operation_id is a bounded operation identifier"));
        }
        let missing = || ApiError::new("api.not_found", "file operation not found");
        let record = self.get(id)?.ok_or_else(missing)?;
        if !identity.kind.is_human_capable() && record.owner != identity.credential_id.as_str() {
            return Err(missing());
        }
        // Plans store canonical paths (including Windows verbatim prefixes). Compare them
        // with canonical current roots, not the user's unnormalized configuration spelling.
        // The historical directory need not still exist to read its operation receipt.
        let visible = zeus_fs::WriteRoots::open(roots).map_err(|_| missing())?;
        if !visible
            .roots()
            .iter()
            .any(|root| record.dir.starts_with(root.path()))
        {
            return Err(missing());
        }
        let (state, mut reason) = match &record.state {
            OperationState::AwaitingApproval => ("awaiting_approval", None),
            OperationState::Applying => ("applying", None),
            OperationState::Applied => ("applied", None),
            OperationState::Dropped { reason } => ("dropped", Some(reason.clone())),
            OperationState::Stopped { reason } => ("stopped", Some(reason.clone())),
        };
        if reason.is_none() && record.undone_by.is_none() && record.outcomes.iter().any(|outcome| {
            matches!(outcome, StepState::Done { trash: Some(entry) }
                if self.trash.receipt_state(entry).ok() == Some(zeus_fs::trash::TrashReceiptState::LegacyManualRecovery))
        }) {
            reason = Some("legacy_trash_manual_recovery".into());
        }
        let count = |test: fn(&StepState) -> bool| {
            record.outcomes.iter().filter(|s| test(s)).count() as u32
        };
        Ok(zeus_api::methods::files::FilesOperationGetResult {
            operation_id: record.operation_id,
            state: state.into(),
            reason,
            steps: record
                .steps
                .iter()
                .map(|step| view(&record.root, step))
                .collect(),
            completed: count(|s| matches!(s, StepState::Done { .. })),
            skipped: count(|s| matches!(s, StepState::Skipped { .. })),
            pending: count(|s| matches!(s, StepState::Pending | StepState::Started)),
            approval_id: record.approval_id,
            undone_by: record.undone_by,
        })
    }

    fn put(&self, record: &OperationRecord) -> ApiResult<()> {
        let bytes = serde_json::to_vec(record).map_err(|_| fail())?;
        let mut tx = self.db.begin_write().map_err(|_| fail())?;
        tx.set_durability(redb::Durability::Immediate)
            .map_err(|_| fail())?;
        tx.open_table(OPERATIONS)
            .map_err(|_| fail())?
            .insert(record.operation_id.as_str(), bytes.as_slice())
            .map_err(|_| fail())?;
        tx.commit().map_err(|_| fail())
    }

    fn unfinished(&self) -> ApiResult<Vec<OperationRecord>> {
        let tx = self.db.begin_read().map_err(|_| fail())?;
        let table = tx.open_table(OPERATIONS).map_err(|_| fail())?;
        let mut records = Vec::new();
        for row in table.iter().map_err(|_| fail())? {
            let (_, value) = row.map_err(|_| fail())?;
            let record: OperationRecord =
                serde_json::from_slice(value.value()).map_err(|_| fail())?;
            if matches!(
                record.state,
                OperationState::AwaitingApproval | OperationState::Applying
            ) {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// Plan `files.organize` for `dir` (absolute, or relative to the first visible root) inside `roots`.
    pub(crate) fn plan(
        &self,
        roots: &[PathBuf],
        dir: &str,
        rules: &serde_json::Value,
    ) -> ApiResult<Plan> {
        let (recursive, rules) = compile(rules)?;
        let set = zeus_fs::WriteRoots::open(roots).map_err(|_| outside())?;
        let first = roots.first().ok_or_else(outside)?;
        let requested = if Path::new(dir).is_absolute() {
            PathBuf::from(dir)
        } else {
            first.join(dir)
        };
        let dir = set.resolve_dest_dir(&requested).map_err(|_| outside())?;
        if !dir.is_dir() {
            return Err(invalid("dir must be an existing directory"));
        }
        let root = set
            .root_for(&dir)
            .map_err(|_| outside())?
            .path()
            .to_path_buf();
        let records = self
            .journal
            .read(|tx| tx.jobs(&zeus_journal::store::JobFilter::default()))
            .map_err(|e| ApiError::from(e.to_envelope()))?;
        // Files of unfinished jobs never move; completed files carry Zeus's quarantine flag.
        let mut busy = BTreeSet::new();
        let mut quarantined = BTreeMap::new();
        for record in &records {
            if let Some(result) = &record.job.result {
                quarantined.insert(PathBuf::from(&result.path), record.job.quarantined);
            } else {
                busy.insert(PathBuf::from(&record.internal.dest_path));
            }
        }
        let mut candidates = Vec::new();
        let mut pending = vec![dir.clone()];
        while let Some(current) = pending.pop() {
            for entry in std::fs::read_dir(&current).map_err(|_| fail())? {
                let entry = entry.map_err(|_| fail())?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let kind = entry.file_type().map_err(|_| fail())?;
                if name == ".zeus" || kind.is_symlink() {
                    continue;
                }
                if kind.is_dir() {
                    if recursive {
                        pending.push(entry.path());
                    }
                    continue;
                }
                let path = entry.path();
                if !kind.is_file() || zeus_fs::finalize::is_zpart(&path) || busy.contains(&path) {
                    continue;
                }
                let metadata = entry.metadata().map_err(|_| fail())?;
                candidates.push(Candidate {
                    path,
                    name,
                    size: metadata.len(),
                    modified_ms: modified_ms(&metadata),
                });
                if candidates.len() > MAX_FILES_SCANNED {
                    return Err(ApiError::new(
                        "api.payload_too_large",
                        "more than 10000 files; organise a narrower directory",
                    ));
                }
            }
        }
        candidates.sort_by(|a, b| a.path.cmp(&b.path));
        let now_ms = Timestamp::now().unix_millis();
        let mut taken: BTreeSet<PathBuf> = BTreeSet::new();
        let mut steps = Vec::new();
        for file in &candidates {
            let ext = Path::new(&file.name)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let Some(rule) = rules.iter().find(|r| {
                let w = &r.rule.when;
                (r.ext.is_empty() || r.ext.contains(&ext))
                    && r.name.as_ref().is_none_or(|re| re.is_match(&file.name))
                    && w.min_bytes.is_none_or(|n| file.size >= n)
                    && w.max_bytes.is_none_or(|n| file.size <= n)
                    && w.older_than_days.is_none_or(|days| {
                        now_ms.saturating_sub(file.modified_ms) >= i64::from(days) * 86_400_000
                    })
            }) else {
                continue;
            };
            let quarantined = quarantined.get(&file.path).copied().unwrap_or(false);
            if rule.rule.trash {
                steps.push(PlannedStep {
                    op: StepOp::Trash,
                    from: file.path.clone(),
                    to: None,
                    size_bytes: file.size,
                    modified_ms: file.modified_ms,
                    quarantined,
                    restore: None,
                });
            } else {
                let mut target = file.path.parent().unwrap_or(&dir).to_path_buf();
                if let Some(subdir) = &rule.rule.subdir {
                    target = dir.clone();
                    let rendered = render(subdir, file);
                    let components: Vec<_> =
                        rendered.split('/').filter(|c| !c.is_empty()).collect();
                    if components.len() > MAX_SUBDIR_COMPONENTS {
                        return Err(invalid("subdir produces more than 8 directory levels"));
                    }
                    for component in components {
                        target.push(
                            sanitized(component).ok_or_else(|| {
                                invalid("subdir renders an empty or invalid name")
                            })?,
                        );
                    }
                }
                let name = match &rule.rule.rename {
                    Some(template) => sanitized(&render(template, file))
                        .ok_or_else(|| invalid("rename renders an empty or invalid name"))?,
                    None => file.name.clone(),
                };
                let mut to = target.join(&name);
                if to == file.path {
                    continue;
                }
                // Collisions take " (n)" suffixes; nothing is ever overwritten.
                let mut n = 1;
                while to.exists() || taken.contains(&to) {
                    n += 1;
                    if n > 999 {
                        return Err(invalid("too many name collisions"));
                    }
                    let stem = Path::new(&name)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    to = target.join(match Path::new(&name).extension() {
                        Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
                        None => format!("{stem} ({n})"),
                    });
                }
                taken.insert(to.clone());
                steps.push(PlannedStep {
                    op: if to.parent() == file.path.parent() {
                        StepOp::Rename
                    } else {
                        StepOp::Move
                    },
                    from: file.path.clone(),
                    to: Some(to),
                    size_bytes: file.size,
                    modified_ms: file.modified_ms,
                    quarantined,
                    restore: None,
                });
            }
            if steps.len() > MAX_STEPS {
                return Err(ApiError::new(
                    "api.payload_too_large",
                    "more than 1000 steps; organise a narrower directory",
                ));
            }
        }
        Ok(Plan {
            root,
            dir,
            steps,
            undo_of: None,
        })
    }

    /// Plan the inverse of the steps an operation ran. Only its owner, or a person, may undo it, once.
    pub(crate) fn plan_undo(
        &self,
        identity: &ClientIdentity,
        operation_id: &str,
    ) -> ApiResult<Plan> {
        let missing = || ApiError::new("api.not_found", "file operation not found");
        let record = self.get(operation_id)?.ok_or_else(missing)?;
        if !identity.kind.is_human_capable() && record.owner != identity.credential_id.as_str() {
            return Err(missing());
        }
        if record.undo_of.is_some() {
            return Err(invalid(
                "an undo cannot itself be undone; organise again instead",
            ));
        }
        if record.undone_by.is_some() {
            return Err(ApiError::new(
                "api.invalid_transition",
                "this operation was already undone",
            ));
        }
        if !matches!(
            record.state,
            OperationState::Applied | OperationState::Stopped { .. }
        ) {
            return Err(ApiError::new(
                "api.invalid_transition",
                "only an applied or stopped operation can be undone",
            ));
        }
        let mut steps = Vec::new();
        for (step, outcome) in record.steps.iter().zip(&record.outcomes).rev() {
            match (step.op, outcome) {
                (StepOp::Move | StepOp::Rename, StepState::Done { .. }) => {
                    let to = step.to.clone().ok_or_else(fail)?;
                    let Ok(metadata) = std::fs::symlink_metadata(&to) else {
                        continue;
                    };
                    steps.push(PlannedStep {
                        op: step.op,
                        from: to,
                        to: Some(step.from.clone()),
                        size_bytes: metadata.len(),
                        modified_ms: modified_ms(&metadata),
                        quarantined: step.quarantined,
                        restore: None,
                    });
                }
                (StepOp::Trash, StepState::Done { trash: Some(entry) }) => {
                    if self.trash.receipt_state(entry).map_err(|_| fail())?
                        == zeus_fs::trash::TrashReceiptState::LegacyManualRecovery
                    {
                        return Err(ApiError::new(
                            "files.legacy_trash_manual_recovery",
                            "This older trash receipt has no recorded identity. Its files are preserved for manual recovery; automated restore and purge are unavailable.",
                        ));
                    }
                    steps.push(PlannedStep {
                        op: StepOp::Restore,
                        from: entry.trash_path.clone(),
                        to: Some(step.from.clone()),
                        size_bytes: step.size_bytes,
                        modified_ms: step.modified_ms,
                        quarantined: step.quarantined,
                        restore: Some(entry.clone()),
                    })
                }
                _ => {}
            }
        }
        Ok(Plan {
            root: record.root,
            dir: record.dir,
            steps,
            undo_of: Some(record.operation_id),
        })
    }

    /// Store a non-empty plan and raise its `destructive_op` approval. Returns the operation and approval ids.
    pub(crate) async fn submit(
        &self,
        identity: &ClientIdentity,
        plan: Plan,
    ) -> ApiResult<(String, ApprovalId)> {
        let operation_set_blake3 = operation_hash(&plan.steps)?;
        let count = |op: StepOp| plan.steps.iter().filter(|s| s.op == op).count() as u64;
        let sample: Vec<_> = plan
            .steps
            .iter()
            .take(20)
            .map(|s| {
                serde_json::json!({
                    "op": match s.op { StepOp::Trash => "delete", StepOp::Rename => "rename", _ => "move" },
                    "path": s.from.display().to_string(),
                    "to": s.to.as_ref().map(|t| t.display().to_string()),
                    "size_bytes": s.size_bytes,
                })
            })
            .collect();
        let deletes = count(StepOp::Trash);
        let facts = crate::consent::DestructiveFacts {
            operation_set_blake3,
            counts: serde_json::json!({
                "delete": deletes,
                "overwrite": 0,
                "move": count(StepOp::Move) + count(StepOp::Restore),
                "rename": count(StepOp::Rename),
            }),
            total_bytes: plan
                .steps
                .iter()
                .filter(|s| s.op == StepOp::Trash)
                .map(|s| s.size_bytes)
                .sum(),
            quarantined_count: plan.steps.iter().filter(|s| s.quarantined).count() as u64,
            sample,
            sample_omitted: plan.steps.len().saturating_sub(20) as u64,
            title: if plan.undo_of.is_some() {
                format!("Undo file changes in {}", plan.dir.display())
            } else {
                format!("Organise {}", plan.dir.display())
            },
            deletes: deletes > 0,
        };
        let approval = self.consent.request_destructive(identity, facts).await?;
        let operation_id = format!("fop_{}", zeus_types::IdGenerator::new().next_ulid());
        self.put(&OperationRecord {
            v: 1,
            operation_id: operation_id.clone(),
            owner: identity.credential_id.to_string(),
            root: plan.root,
            dir: plan.dir,
            undo_of: plan.undo_of,
            created_at: Timestamp::now(),
            outcomes: vec![StepState::Pending; plan.steps.len()],
            steps: plan.steps,
            operation_set_blake3,
            approval_id: approval,
            state: OperationState::AwaitingApproval,
            undone_by: None,
        })?;
        Ok((operation_id, approval))
    }

    /// Execute approved operations, at most 64 steps per call. Blocking file I/O: run it off the async executor.
    pub(crate) fn tick(&self, kill_switch_engaged: bool) -> ApiResult<()> {
        let _gate = self.gate.lock().map_err(|_| fail())?;
        let mut budget = STEPS_PER_TICK;
        for mut record in self.unfinished()? {
            if record.state == OperationState::AwaitingApproval {
                let approval = self
                    .journal
                    .read(|tx| tx.approval(record.approval_id))
                    .map_err(|e| ApiError::from(e.to_envelope()))?;
                match approval.as_ref().map(|a| a.state) {
                    Some(ApprovalState::Pending) => continue,
                    Some(ApprovalState::Approved) => {
                        // 07 section 4.4 rule 5: recompute the binding before anything runs.
                        let bound = approval
                            .as_ref()
                            .and_then(|a| a.facts.as_ref())
                            .and_then(|f| f.get("operation_set_blake3"))
                            .and_then(|v| v.as_str())
                            .map(str::to_owned);
                        if bound.as_deref()
                            != Some(operation_hash(&record.steps)?.to_string().as_str())
                            || bound.as_deref()
                                != Some(record.operation_set_blake3.to_string().as_str())
                        {
                            record.state = OperationState::Dropped {
                                reason: "binding_mismatch".into(),
                            };
                        } else {
                            record.state = OperationState::Applying;
                        }
                    }
                    Some(state) => {
                        record.state = OperationState::Dropped {
                            reason: serde_json::to_value(state)
                                .ok()
                                .and_then(|v| v.as_str().map(str::to_owned))
                                .unwrap_or_else(|| "closed".into()),
                        };
                    }
                    None => {
                        record.state = OperationState::Dropped {
                            reason: "approval_missing".into(),
                        };
                    }
                }
                self.put(&record)?;
            }
            if record.state != OperationState::Applying {
                continue;
            }
            if kill_switch_engaged {
                record.state = OperationState::Stopped {
                    reason: "kill_switch".into(),
                };
                self.put(&record)?;
                continue;
            }
            for index in 0..record.steps.len() {
                if budget == 0 {
                    return Ok(());
                }
                if matches!(
                    record.outcomes[index],
                    StepState::Done { .. } | StepState::Skipped { .. }
                ) {
                    continue;
                }
                budget -= 1;
                let recovered = record.outcomes[index] == StepState::Started;
                let step = record.steps[index].clone();
                let outcome = if recovered {
                    self.recover(&record, &step)
                } else {
                    None
                };
                let outcome = match outcome {
                    Some(outcome) => outcome,
                    None => {
                        record.outcomes[index] = StepState::Started;
                        self.put(&record)?;
                        self.execute(&record, &step)?
                    }
                };
                record.outcomes[index] = outcome;
                self.put(&record)?;
            }
            record.state = OperationState::Applied;
            if let Some(original) = record.undo_of.clone()
                && let Some(mut undone) = self.get(&original)?
            {
                undone.undone_by = Some(record.operation_id.clone());
                self.put(&undone)?;
            }
            self.put(&record)?;
        }
        Ok(())
    }

    /// After a restart, decide from the files whether a started step already happened.
    fn recover(&self, record: &OperationRecord, step: &PlannedStep) -> Option<StepState> {
        match step.op {
            StepOp::Move | StepOp::Rename | StepOp::Restore => {
                let to = step.to.as_ref()?;
                let source_gone = !step.from.exists();
                let arrived = std::fs::symlink_metadata(to)
                    .is_ok_and(|m| m.file_type().is_file() && m.len() == step.size_bytes);
                (source_gone && arrived).then_some(StepState::Done { trash: None })
            }
            StepOp::Trash => {
                if step.from.exists() {
                    return None;
                }
                let entry =
                    self.trash.entries().ok()?.into_iter().find(|e| {
                        e.original_path == step.from && e.trashed_at >= record.created_at
                    })?;
                Some(StepState::Done { trash: Some(entry) })
            }
        }
    }

    fn execute(&self, record: &OperationRecord, step: &PlannedStep) -> ApiResult<StepState> {
        let skipped = |reason: &str| {
            Ok(StepState::Skipped {
                reason: reason.into(),
            })
        };
        let Ok(root) = zeus_fs::SafeRoot::open(&record.root) else {
            return skipped("root_unavailable");
        };
        let Ok(root_cap) = zeus_fs::DirectoryCap::open_root(root.path()) else {
            return skipped("root_unavailable");
        };
        if root_cap.file_id().ok().as_ref() != root.identity() {
            return skipped("root_changed");
        }
        let source_parent = if step.op != StepOp::Restore {
            let Some(parent) = step.from.parent() else {
                return skipped("source_changed");
            };
            let Ok(relative) = parent.strip_prefix(root.path()) else {
                return skipped("source_changed");
            };
            match root_cap.open_dir(relative) {
                Ok(cap) => Some(cap),
                Err(_) => return skipped("source_changed"),
            }
        } else {
            None
        };
        let source_checked = if step.op == StepOp::Restore {
            step.restore.as_ref().is_some_and(|entry| {
                self.trash.receipt_state(entry).ok()
                    == Some(zeus_fs::trash::TrashReceiptState::IdentityBound)
            })
        } else {
            source_parent
                .as_ref()
                .and_then(|parent| {
                    step.from
                        .file_name()
                        .and_then(|name| parent.entry_metadata(name).ok())
                })
                .is_some_and(|m| {
                    m.is_regular_file && m.len == step.size_bytes && m.mtime_ms == step.modified_ms
                })
        };
        if !source_checked {
            return skipped("source_changed");
        }
        let destination_parent = if let Some(to) = &step.to {
            let Some(parent) = to.parent() else {
                return skipped("destination_invalid");
            };
            let Ok(relative) = parent.strip_prefix(root.path()) else {
                return skipped("destination_outside_root");
            };
            let Ok(cap) = root_cap.create_dir_all(relative) else {
                return skipped("destination_outside_root");
            };
            let Some(name) = to.file_name() else {
                return skipped("destination_invalid");
            };
            match cap.entry_exists(name) {
                Ok(false) => Some(cap),
                Ok(true) => return skipped("destination_exists"),
                Err(_) => return skipped("destination_unavailable"),
            }
        } else {
            None
        };
        let (action, outcome) = match step.op {
            StepOp::Move | StepOp::Restore => {
                (zeus_verify::audit::actions::FILE_MOVED, AuditOutcome::Ok)
            }
            StepOp::Rename => (zeus_verify::audit::actions::FILE_RENAMED, AuditOutcome::Ok),
            StepOp::Trash => (zeus_verify::audit::actions::FILE_DELETED, AuditOutcome::Ok),
        };
        let mut entry = AuditEntry::new(
            Timestamp::now(),
            zeus_types::Actor::system(),
            action,
            outcome,
        );
        entry.subject.path = Some(step.from.display().to_string());
        entry.subject.approval_id = Some(record.approval_id);
        entry.data.insert(
            "operation_id".into(),
            serde_json::json!(record.operation_id),
        );
        entry.data.insert(
            "to".into(),
            serde_json::json!(step.to.as_ref().map(|t| t.display().to_string())),
        );
        entry
            .data
            .insert("size_bytes".into(), serde_json::json!(step.size_bytes));
        if step.op == StepOp::Trash {
            entry
                .data
                .insert("to_trash".into(), serde_json::json!(true));
        }
        // Write-ahead: no file changes without its record (04 section 4.6 item 8).
        self.audit
            .append(entry)
            .map_err(|e| ApiError::from(e.to_envelope()))?;
        Ok(match step.op {
            StepOp::Move | StepOp::Rename => {
                let parent = source_parent.as_ref().ok_or_else(fail)?;
                let name = step.from.file_name().ok_or_else(fail)?;
                let destination = destination_parent.as_ref().ok_or_else(fail)?;
                let target = step
                    .to
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .ok_or_else(fail)?;
                let operation = (|| {
                    let source = parent.prepare_rename(name)?;
                    let meta = source.metadata()?;
                    if !meta.is_regular_file
                        || meta.len != step.size_bytes
                        || meta.mtime_ms != step.modified_ms
                    {
                        return Err(zeus_fs::FsError::OutsideRoots);
                    }
                    source.rename_noreplace(destination, target)?;
                    parent.sync()?;
                    destination.sync()
                })();
                match operation {
                    Ok(()) => StepState::Done { trash: None },
                    Err(zeus_fs::FsError::AlreadyExists) => StepState::Skipped {
                        reason: "destination_exists".into(),
                    },
                    Err(_) => StepState::Skipped {
                        reason: "move_failed".into(),
                    },
                }
            }
            StepOp::Trash => match self.trash.trash_pinned(
                source_parent.as_ref().ok_or_else(fail)?,
                step.from.file_name().ok_or_else(fail)?,
                &step.from,
                Timestamp::now(),
            ) {
                Ok(entry) => StepState::Done { trash: Some(entry) },
                Err(_) => StepState::Skipped {
                    reason: "trash_failed".into(),
                },
            },
            StepOp::Restore => match step
                .restore
                .as_ref()
                .zip(destination_parent.as_ref())
                .map(|(e, parent)| self.trash.restore_pinned(e, parent))
            {
                Some(Ok(_)) => StepState::Done { trash: None },
                Some(Err(zeus_fs::FsError::AlreadyExists)) => StepState::Skipped {
                    reason: "destination_exists".into(),
                },
                _ => StepState::Skipped {
                    reason: "restore_failed".into(),
                },
            },
        })
    }

    /// The state of an operation, for tests and diagnostics: `(state, done, skipped)`.
    #[cfg(test)]
    pub(crate) fn summary(&self, id: &str) -> Option<(String, usize, usize)> {
        let record = self.get(id).ok()??;
        let state = serde_json::to_value(&record.state).ok()?["state"]
            .as_str()?
            .to_owned();
        let done = record
            .outcomes
            .iter()
            .filter(|o| matches!(o, StepState::Done { .. }))
            .count();
        let skipped = record
            .outcomes
            .iter()
            .filter(|o| matches!(o, StepState::Skipped { .. }))
            .count();
        Some((state, done, skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_sets_are_closed_bounded_and_unambiguous() {
        assert!(
            compile(
                &serde_json::json!({"rules":[{"when":{"ext":["pdf"]},"subdir":"papers/{year}"}]})
            )
            .is_ok()
        );
        for bad in [
            serde_json::json!({"rules":[]}),
            serde_json::json!({"rules":[{"subdir":"x","trash":true}]}),
            serde_json::json!({"rules":[{"when":{}}]}),
            serde_json::json!({"rules":[{"subdir":"../escape"}]}),
            serde_json::json!({"rules":[{"subdir":"/abs"}]}),
            serde_json::json!({"rules":[{"rename":"{secret}"}]}),
            serde_json::json!({"rules":[{"rename":"x","surprise":1}]}),
            serde_json::json!({"rules":[{"when":{"name_regex":"("},"rename":"x"}]}),
            serde_json::json!("put my files somewhere nice"),
        ] {
            assert!(compile(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn templates_render_dates_and_never_create_extra_directories() {
        let file = Candidate {
            path: PathBuf::from("/r/a.pdf"),
            name: "report.final.pdf".into(),
            size: 1,
            modified_ms: 1_791_289_200_000,
        };
        assert_eq!(render("{year}/{month}", &file), "2026/10");
        assert_eq!(render("{stem}-{day}.{ext}", &file), "report.final-06.pdf");
        assert_eq!(sanitized("a/b").as_deref(), Some("a_b"));
        assert_ne!(sanitized("..").as_deref(), Some(".."));
    }
}
