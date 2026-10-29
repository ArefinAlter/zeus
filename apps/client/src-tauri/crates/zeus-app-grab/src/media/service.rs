//! Opaque inspection/review identities and a private, persistent media task list.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;
use zeus_app::failure::{Failure, Result};
use zeus_app::home::AppHome;
use zeus_fs::{Filesystem, RealFs, SafeRoot, SanitizeOptions, sanitize_filename};

use super::http::{Network, status};
use super::manifest::{FormatView, Segment, Track, resolve};
use super::transfer::{self, Checkpoint, Progress};
use super::{dash, hls};
use super::{mux, runner};

const CACHE_TTL: Duration = Duration::from_secs(600);
const MAX_TASKS: usize = 128;
const MAX_BYTES: u64 = 100 * 1024 * 1024 * 1024;

#[derive(Clone)]
enum Choice {
    Track(Track),
    Playlist(hls::Variant),
    Discovered(Url),
}

struct Inspection {
    created: Instant,
    network: Network,
    choices: Vec<Choice>,
}

struct Plan {
    created: Instant,
    network: Network,
    track: Track,
    roots: Vec<String>,
}

#[derive(Clone)]
struct MuxPlan {
    created: Instant,
    inputs: Vec<Checkpoint>,
    container: String,
    fingerprint: String,
    helper: runner::Pin,
    roots: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MuxRequest {
    task_ids: Vec<String>,
    container: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MuxStart {
    plan_id: String,
    filename: String,
    root_index: usize,
    max_bytes: u64,
    timeout_ms: u64,
    idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectRequest {
    url: String,
    #[serde(default)]
    additional_origins: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    inspection_id: String,
    format_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    plan_id: String,
    root_index: usize,
    filename: String,
    max_bytes: u64,
    connections: usize,
    idempotency_key: String,
    #[serde(default)]
    resume_task_id: Option<String>,
}

/// The desktop's media service. It is a bounded native module, not an RPC proxy
/// or an approval authority; every acquisition starts with a reviewed plan.
pub struct Service {
    home: AppHome,
    inspections: Mutex<HashMap<String, Inspection>>,
    plans: Mutex<HashMap<String, Plan>>,
    mux_plans: Mutex<HashMap<String, MuxPlan>>,
    tasks: Mutex<BTreeMap<String, Arc<Progress>>>,
    handles: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    unreadable_checkpoints: usize,
    network_gate: tokio::sync::Semaphore,
}

fn state_error() -> Failure {
    Failure::new("media.state", "The media service state is unavailable.")
}

fn task_identity(value: &str) -> bool {
    value
        .strip_prefix("media-")
        .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn require_unused_staging(home: &AppHome, task_id: &str) -> Result<()> {
    match std::fs::symlink_metadata(home.app_dir().join("media").join(task_id)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(Failure::new(
            "media.checkpoint_unavailable",
            "Retained media data already uses this identity, but its checkpoint is unavailable. Preserve it for repair and choose a new operation identity.",
        )),
        Err(_) => Err(state_error()),
    }
}

fn recoverable(checkpoint: &Checkpoint) -> bool {
    (checkpoint.kind == "acquisition" && checkpoint.receipts.len() == checkpoint.segment_count)
        || (checkpoint.kind == "mux"
            && checkpoint.assembly_started
            && checkpoint.output_bytes.is_some()
            && checkpoint.sha256.is_some())
}

fn valid_filename(value: &str, container: Option<&str>) -> bool {
    sanitize_filename(value, &SanitizeOptions::default()).is_ok_and(|safe| safe.name == value)
        && value.len() <= 200
        && !value.starts_with('.')
        && container.is_none_or(|container| {
            value
                .to_ascii_lowercase()
                .ends_with(&format!(".{container}"))
        })
}

fn load(dir: &Path) -> (BTreeMap<String, Arc<Progress>>, usize) {
    let mut tasks = BTreeMap::new();
    let mut unreadable = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (tasks, unreadable);
    };
    for entry in entries.take(MAX_TASKS + 1) {
        let Ok(entry) = entry else {
            unreadable += 1;
            continue;
        };
        let id = entry.file_name().to_string_lossy().into_owned();
        if !task_identity(&id) {
            continue;
        }
        if tasks.len() >= MAX_TASKS {
            unreadable += 1;
            continue;
        }
        let path = entry.path().join("checkpoint.json");
        let result = (|| -> Result<Checkpoint> {
            let meta = RealFs.metadata(&entry.path()).map_err(|_| state_error())?;
            if !meta.is_some_and(|m| m.is_dir && !m.is_symlink) {
                return Err(state_error());
            }
            let file = RealFs.open_read(&path).map_err(|_| state_error())?;
            let length = file.len().map_err(|_| state_error())?;
            if length > 4 * 1024 * 1024 {
                return Err(state_error());
            }
            let mut bytes = vec![0; length as usize];
            file.read_exact_at(0, &mut bytes)
                .map_err(|_| state_error())?;
            let mut checkpoint: Checkpoint = serde_json::from_slice(&bytes)?;
            if checkpoint.task_id != id
                || !checkpoint.root.is_absolute()
                || !valid_filename(&checkpoint.filename, None)
                || checkpoint.segment_count == 0
                || checkpoint.segment_count > super::manifest::MAX_SEGMENTS
                || checkpoint.fingerprint.len() != 64
                || !checkpoint
                    .fingerprint
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
                || checkpoint.max_bytes == 0
                || checkpoint.max_bytes > MAX_BYTES
                || checkpoint.network_bytes > MAX_BYTES
                || checkpoint
                    .output_bytes
                    .is_some_and(|bytes| bytes > MAX_BYTES)
                || checkpoint
                    .receipts
                    .keys()
                    .any(|index| *index >= checkpoint.segment_count)
                || checkpoint.receipts.values().any(|receipt| {
                    receipt.sha256.len() != 64
                        || !receipt.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                        || receipt.bytes == 0
                        || receipt.bytes > MAX_BYTES
                })
            {
                return Err(state_error());
            }
            if matches!(
                checkpoint.state.as_str(),
                "downloading" | "assembling" | "converting"
            ) {
                checkpoint.state = "interrupted".into();
                checkpoint.error_code = Some("media.process_interrupted".into());
                checkpoint.error_message = Some("Inspect the same source again, select the same track and explicitly resume this task.".into());
            }
            Ok(checkpoint)
        })();
        match result {
            Ok(checkpoint) => {
                tasks.insert(id, Arc::new(Progress::new(checkpoint)));
            }
            Err(_) => unreadable += 1,
        }
    }
    (tasks, unreadable)
}

fn extension(url: &Url) -> String {
    url.path()
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn direct(url: &Url, mime: Option<&str>) -> Option<Track> {
    if mime.is_some_and(|mime| mime.starts_with("text/") || mime.starts_with("application/json")) {
        return None;
    }
    let extension = extension(url);
    let container = match extension.as_str() {
        "mp4" | "m4a" | "mp3" | "webm" | "ogg" | "opus" | "flac" | "wav" | "mov" | "mkv" | "ts" => {
            extension
        }
        _ => match mime.unwrap_or("").split(';').next().unwrap_or("") {
            "video/mp4" => "mp4".into(),
            "audio/mp4" => "m4a".into(),
            "audio/mpeg" => "mp3".into(),
            "video/webm" | "audio/webm" => "webm".into(),
            "audio/ogg" => "ogg".into(),
            "audio/flac" => "flac".into(),
            _ => return None,
        },
    };
    Some(Track {
        label: "Direct media file".into(), container, mime_type: mime.map(str::to_owned),
        bandwidth: None, width: None, height: None,
        segments: vec![Segment { url: url.clone(), range: None }], duration_ms: None,
        notices: vec!["This saves one direct media resource. The ordinary Downloads queue also supports direct media with dynamic HTTP chunking.".into()],
    })
}

fn choice_view(choice: &Choice, index: usize) -> FormatView {
    match choice {
        Choice::Track(track) => track.view(index.to_string()),
        Choice::Playlist(variant) => {
            let mut view = variant.facts.view(index.to_string());
            view.segment_count = None;
            view
        }
        Choice::Discovered(url) => FormatView {
            format_id: index.to_string(), label: format!("Page media candidate ({})", extension(url)),
            container: extension(url), mime_type: None, bandwidth: None, width: None, height: None,
            segment_count: None, duration_ms: None,
            notices: vec!["Discovered in page markup. Select it to inspect its actual media manifest; page scripts were not executed.".into()],
        },
    }
}

async fn inspect_source(network: &Network, url: &Url) -> Result<Vec<Choice>> {
    let cancel = Arc::new(AtomicBool::new(false));
    if !matches!(extension(url).as_str(), "m3u8" | "m3u" | "mpd") {
        let head = network
            .request(
                url.clone(),
                Method::HEAD,
                Default::default(),
                cancel.clone(),
            )
            .await?;
        if head.status() != reqwest::StatusCode::METHOD_NOT_ALLOWED
            && head.status() != reqwest::StatusCode::NOT_IMPLEMENTED
        {
            status(head.status())?;
            let mime = head
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok());
            if let Some(track) = direct(head.url(), mime) {
                return Ok(vec![Choice::Track(track)]);
            }
        }
    }
    let (base, content_type, text) = network.manifest(url, cancel).await?;
    if text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with("#EXTM3U")
    {
        return Ok(match hls::parse(&base, &text)? {
            hls::Playlist::Master(variants) => variants.into_iter().map(Choice::Playlist).collect(),
            hls::Playlist::Media(track) => vec![Choice::Track(track)],
        });
    }
    if extension(&base) == "mpd"
        || content_type.contains("dash+xml")
        || text.trim_start().starts_with("<?xml")
        || text.trim_start().starts_with("<MPD")
    {
        return Ok(dash::parse(&base, &text)?
            .into_iter()
            .map(Choice::Track)
            .collect());
    }
    if content_type == "text/html"
        || text.trim_start().starts_with("<!DOCTYPE html")
        || text.trim_start().starts_with("<html")
    {
        let (links, declared_base) = crate::html::extract(&text);
        let base = declared_base
            .as_ref()
            .map_or(Ok(base.clone()), |value| resolve(&base, value))?;
        network.check(&base)?;
        let mut choices = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for link in links {
            let Ok(url) = resolve(&base, &link.href) else {
                continue;
            };
            if !matches!(extension(&url).as_str(), "m3u8" | "mpd") && direct(&url, None).is_none() {
                continue;
            }
            if seen.insert(url.as_str().to_owned()) {
                choices.push(Choice::Discovered(url));
            }
            if choices.len() >= super::manifest::MAX_FORMATS {
                break;
            }
        }
        if !choices.is_empty() {
            return Ok(choices);
        }
        return Err(super::unsupported(
            "No direct media or HLS/DASH manifest was found in the page markup. Script-only players need an explicit media URL or a supported browser handoff.",
        ));
    }
    Err(super::invalid(
        "The response is not supported media or a media manifest.",
    ))
}

impl Service {
    /// Load private checkpoints. Active tasks become interrupted and require an
    /// explicit fresh source inspection; startup never resumes network activity.
    pub fn new(home: AppHome) -> Arc<Self> {
        let (tasks, unreadable_checkpoints) = load(&home.app_dir().join("media"));
        Arc::new(Self {
            home,
            inspections: Mutex::new(HashMap::new()),
            plans: Mutex::new(HashMap::new()),
            mux_plans: Mutex::new(HashMap::new()),
            tasks: Mutex::new(tasks),
            handles: Mutex::new(HashMap::new()),
            unreadable_checkpoints,
            network_gate: tokio::sync::Semaphore::new(2),
        })
    }

    /// Inspect a person-supplied URL and retain secret-bearing resource URLs only in memory.
    pub async fn inspect(&self, body: &Value) -> Result<Value> {
        let _permit = self.network_gate.try_acquire().map_err(|_| {
            Failure::new("media.busy", "Two media inspections are already running.")
        })?;
        let request: InspectRequest = serde_json::from_value(body.clone())?;
        if request.url.len() > 16 * 1024 {
            return Err(Failure::invalid("The media URL exceeds the limit."));
        }
        let url =
            Url::parse(&request.url).map_err(|_| Failure::invalid("The media URL is invalid."))?;
        let url = resolve(&url, "")?;
        let network = Network::new(&url, &request.additional_origins)?;
        let choices = inspect_source(&network, &url).await?;
        let formats: Vec<_> = choices
            .iter()
            .enumerate()
            .map(|(i, choice)| choice_view(choice, i))
            .collect();
        let inspection_id = zeus_app::review::random_id();
        let origins = network.origins();
        {
            let mut cache = self.inspections.lock().map_err(|_| state_error())?;
            cache.retain(|_, item| item.created.elapsed() < CACHE_TTL);
            if cache.len() >= 16 {
                return Err(Failure::new(
                    "media.busy",
                    "Too many media inspections are waiting. Try again after their ten-minute expiry.",
                ));
            }
            cache.insert(
                inspection_id.clone(),
                Inspection {
                    created: Instant::now(),
                    network,
                    choices,
                },
            );
        }
        Ok(
            json!({"inspection_id": inspection_id, "formats": formats, "origins": origins, "expires_in_ms": CACHE_TTL.as_millis()}),
        )
    }

    /// Resolve the chosen format and return a review containing only safe facts.
    pub async fn select(&self, body: &Value) -> Result<Value> {
        let _permit = self.network_gate.try_acquire().map_err(|_| {
            Failure::new("media.busy", "Two media inspections are already running.")
        })?;
        let selection: Selection = serde_json::from_value(body.clone())?;
        let index: usize = selection
            .format_id
            .parse()
            .map_err(|_| Failure::invalid("The media format identity is invalid."))?;
        let (network, choice) =
            {
                let cache = self.inspections.lock().map_err(|_| state_error())?;
                let inspection = cache
                    .get(&selection.inspection_id)
                    .filter(|item| item.created.elapsed() < CACHE_TTL)
                    .ok_or_else(|| {
                        Failure::new(
                            "media.review_expired",
                            "Inspect the media again before selecting a format.",
                        )
                    })?;
                (
                    inspection.network.clone(),
                    inspection.choices.get(index).cloned().ok_or_else(|| {
                        Failure::invalid("The selected media format is unavailable.")
                    })?,
                )
            };
        let track = match choice {
            Choice::Track(track) => track,
            Choice::Playlist(variant) => {
                let (base, _, text) = network
                    .manifest(&variant.url, Arc::new(AtomicBool::new(false)))
                    .await?;
                let hls::Playlist::Media(mut track) = hls::parse(&base, &text)? else {
                    return Err(super::unsupported(
                        "Nested HLS master playlists require another explicit inspection.",
                    ));
                };
                track.label = variant.facts.label;
                track.bandwidth = variant.facts.bandwidth;
                track.width = variant.facts.width;
                track.height = variant.facts.height;
                track.notices.extend(variant.facts.notices);
                track
            }
            Choice::Discovered(url) => {
                let choices = inspect_source(&network, &url).await?;
                match choices.as_slice() {
                    [Choice::Track(track)] => track.clone(),
                    _ => {
                        let formats: Vec<_> = choices
                            .iter()
                            .enumerate()
                            .map(|(i, choice)| choice_view(choice, i))
                            .collect();
                        let id = zeus_app::review::random_id();
                        let mut inspections = self.inspections.lock().map_err(|_| state_error())?;
                        inspections.retain(|_, item| item.created.elapsed() < CACHE_TTL);
                        if inspections.len() >= 16 {
                            return Err(Failure::new(
                                "media.busy",
                                "Too many media inspections are waiting.",
                            ));
                        }
                        inspections.insert(
                            id.clone(),
                            Inspection {
                                created: Instant::now(),
                                network: network.clone(),
                                choices,
                            },
                        );
                        return Ok(
                            json!({"inspection_id": id, "formats": formats, "origins": network.origins(), "needs_selection": true}),
                        );
                    }
                }
            }
        };
        for segment in &track.segments {
            network.check(&segment.url)?;
        }
        let settings = zeus_app::engine_config::load(&self.home)?;
        let id = zeus_app::review::random_id();
        let filename = format!("media-{}.{}", &id[..8], track.container);
        let view = track.view(selection.format_id);
        let fingerprint = track.fingerprint();
        let origins = network.origins();
        {
            let mut plans = self.plans.lock().map_err(|_| state_error())?;
            plans.retain(|_, plan| plan.created.elapsed() < CACHE_TTL);
            if plans.len() >= 16 {
                return Err(Failure::new(
                    "media.busy",
                    "Too many media plans are waiting. Try again after their ten-minute expiry.",
                ));
            }
            plans.insert(
                id.clone(),
                Plan {
                    created: Instant::now(),
                    network,
                    track,
                    roots: settings.write_roots.clone(),
                },
            );
        }
        Ok(
            json!({"plan_id": id, "format": view, "fingerprint": fingerprint, "filename": filename,
            "origins": origins, "roots": settings.write_roots, "default_max_bytes": 4u64 * 1024 * 1024 * 1024,
            "expires_in_ms": CACHE_TTL.as_millis()}),
        )
    }

    /// Start exactly the selected plan, or explicitly resume a matching private
    /// checkpoint. No arbitrary path, credential or executable is accepted.
    pub async fn start(self: &Arc<Self>, body: &Value) -> Result<Value> {
        let request: StartRequest = serde_json::from_value(body.clone())?;
        if request.max_bytes == 0
            || request.max_bytes > MAX_BYTES
            || !(1..=8).contains(&request.connections)
            || !(16..=128).contains(&request.idempotency_key.len())
            || !request.idempotency_key.is_ascii()
        {
            return Err(Failure::invalid(
                "Choose one to eight connections, a byte ceiling up to 100 GiB, and a bounded idempotency key.",
            ));
        }
        let (network, track, reviewed_roots) = {
            let plans = self.plans.lock().map_err(|_| state_error())?;
            let plan = plans
                .get(&request.plan_id)
                .filter(|plan| plan.created.elapsed() < CACHE_TTL)
                .ok_or_else(|| {
                    Failure::new(
                        "media.review_expired",
                        "Inspect and select the media again before starting.",
                    )
                })?;
            (plan.network.clone(), plan.track.clone(), plan.roots.clone())
        };
        if !valid_filename(&request.filename, Some(&track.container)) {
            return Err(Failure::invalid(
                "Use a safe media filename with the selected container's extension, without folders or a leading dot.",
            ));
        }
        let settings = zeus_app::engine_config::load(&self.home)?;
        if settings.write_roots != reviewed_roots {
            return Err(Failure::new(
                "media.review_changed",
                "The configured destination folders changed after this review. Select the media format again.",
            ));
        }
        let configured = settings
            .write_roots
            .get(request.root_index)
            .ok_or_else(|| Failure::invalid("Choose a configured download folder."))?;
        let root = SafeRoot::open(Path::new(configured)).map_err(|_| {
            Failure::new(
                "media.destination",
                "The configured download folder is unavailable.",
            )
        })?;
        let fingerprint = track.fingerprint();
        let task_id = match &request.resume_task_id {
            Some(id) if task_identity(id) => id.clone(),
            Some(_) => return Err(Failure::invalid("The media task identity is invalid.")),
            None => format!(
                "media-{}",
                &hex::encode(Sha256::digest(request.idempotency_key.as_bytes()))[..32]
            ),
        };
        let progress = {
            let mut tasks = self.tasks.lock().map_err(|_| state_error())?;
            if let Some(existing) = tasks.get(&task_id) {
                let checkpoint = existing.snapshot()?;
                if checkpoint.kind != "acquisition"
                    || checkpoint.fingerprint != fingerprint
                    || checkpoint.filename != request.filename
                    || checkpoint.root != root.path()
                    || checkpoint.root_index != request.root_index
                {
                    return Err(Failure::new(
                        "media.idempotency_conflict",
                        "This task belongs to a different reviewed track or destination.",
                    ));
                }
                if matches!(
                    checkpoint.state.as_str(),
                    "downloading" | "assembling" | "converting" | "complete"
                ) {
                    if checkpoint.max_bytes != request.max_bytes
                        || checkpoint.connections != request.connections
                    {
                        return Err(Failure::new(
                            "media.idempotency_conflict",
                            "This task was started with a different byte ceiling or connection limit.",
                        ));
                    }
                    return task_view(existing);
                }
                if request.resume_task_id.is_none() {
                    return Err(Failure::new(
                        "media.explicit_resume_required",
                        "Explicitly choose this interrupted task when resuming it.",
                    ));
                }
            } else if request.resume_task_id.is_some() {
                return Err(Failure::new(
                    "media.task_missing",
                    "The media task cannot be resumed because its checkpoint is unavailable.",
                ));
            }
            if tasks
                .values()
                .filter(|task| {
                    task.snapshot().is_ok_and(|state| {
                        matches!(
                            state.state.as_str(),
                            "downloading" | "assembling" | "converting"
                        )
                    })
                })
                .count()
                >= 2
            {
                return Err(Failure::new(
                    "media.busy",
                    "At most two media acquisitions can run together.",
                ));
            }
            let checkpoint = if let Some(existing) = tasks.get(&task_id) {
                existing.snapshot()?
            } else {
                require_unused_staging(&self.home, &task_id)?;
                if tasks.len() >= MAX_TASKS {
                    return Err(Failure::new(
                        "media.history_full",
                        "Archive completed media tasks before starting another acquisition.",
                    ));
                }
                let target = root
                    .join_checked(Path::new(&request.filename))
                    .map_err(|_| Failure::invalid("The output name is invalid."))?;
                if RealFs
                    .metadata(&target)
                    .map_err(|_| state_error())?
                    .is_some()
                {
                    return Err(Failure::new(
                        "media.destination_exists",
                        "The output already exists. Choose another media filename.",
                    ));
                }
                Checkpoint {
                    task_id: task_id.clone(),
                    fingerprint,
                    label: track.label.clone(),
                    root_index: request.root_index,
                    root: root.path().to_path_buf(),
                    filename: request.filename,
                    state: "reviewed".into(),
                    kind: "acquisition".into(),
                    source_task_ids: Vec::new(),
                    helper_sha256: None,
                    max_bytes: request.max_bytes,
                    connections: request.connections,
                    timeout_ms: None,
                    assembly_started: false,
                    segment_count: track.segments.len(),
                    receipts: BTreeMap::new(),
                    network_bytes: 0,
                    output_bytes: None,
                    sha256: None,
                    error_code: None,
                    error_message: None,
                }
            };
            let mut checkpoint = checkpoint;
            if checkpoint.network_bytes >= request.max_bytes {
                return Err(Failure::invalid(
                    "The reviewed byte ceiling must exceed bytes already received by this task.",
                ));
            }
            checkpoint.state = "downloading".into();
            checkpoint.max_bytes = request.max_bytes;
            checkpoint.connections = request.connections;
            checkpoint.error_code = None;
            checkpoint.error_message = None;
            let progress = Arc::new(Progress::new(checkpoint));
            tasks.insert(task_id.clone(), progress.clone());
            progress
        };
        let dir = self.home.app_dir().join("media").join(&task_id);
        let prepared = RealFs
            .create_dir_all(&dir)
            .map_err(|_| {
                Failure::new(
                    "media.state",
                    "The private media folder could not be created.",
                )
            })
            .and_then(|()| transfer::save(&dir, &progress));
        if let Err(error) = prepared {
            if let Ok(mut checkpoint) = progress.checkpoint.lock() {
                checkpoint.state = "failed".into();
                checkpoint.error_code = Some(error.code.clone());
                checkpoint.error_message = Some(error.message.clone());
            }
            return Err(error);
        }
        let worker_progress = progress.clone();
        let handle = tokio::spawn(async move {
            let result = transfer::execute(
                network,
                track,
                dir.clone(),
                worker_progress.clone(),
                request.max_bytes,
                request.connections,
            )
            .await;
            if let Err(error) = result {
                if let Ok(mut checkpoint) = worker_progress.checkpoint.lock() {
                    checkpoint.state = if error.code == "media.canceled" {
                        "canceled"
                    } else {
                        "failed"
                    }
                    .into();
                    checkpoint.error_code = Some(error.code);
                    checkpoint.error_message = Some(error.message);
                }
                let _ = transfer::save(&dir, &worker_progress);
            }
        });
        self.handles
            .lock()
            .map_err(|_| state_error())?
            .insert(task_id, handle);
        task_view(&progress)
    }

    /// Read only the operator-owned helper pin. This route cannot install,
    /// enable, replace or trust any executable.
    pub fn helper(&self) -> Value {
        match runner::Pin::load(&self.home.app_dir()) {
            Ok(pin) => pin.view(),
            Err(error) => {
                json!({"configured": false, "error_code": error.code, "message": error.message})
            }
        }
    }

    /// Preview fixed stream-copy muxing of completed media task IDs. Input
    /// filenames and digests are daemon-independent local facts, never commands.
    pub fn mux_preview(&self, body: &Value) -> Result<Value> {
        let request: MuxRequest = serde_json::from_value(body.clone())?;
        if request.task_ids.is_empty()
            || request.task_ids.len() > 2
            || request.task_ids.iter().any(|id| !task_identity(id))
            || (request.task_ids.len() == 2 && request.task_ids[0] == request.task_ids[1])
            || !matches!(request.container.as_str(), "mp4" | "mkv")
        {
            return Err(Failure::invalid(
                "Choose one completed media task, or distinct video/audio tasks, and MP4 or MKV output.",
            ));
        }
        let helper = runner::Pin::load(&self.home.app_dir())?;
        let settings = zeus_app::engine_config::load(&self.home)?;
        let tasks = self.tasks.lock().map_err(|_| state_error())?;
        let inputs = request
            .task_ids
            .iter()
            .map(|id| {
                let task = tasks.get(id).ok_or_else(|| {
                    Failure::new("media.task_missing", "An input media task was not found.")
                })?;
                let checkpoint = task.snapshot()?;
                if checkpoint.state != "complete"
                    || checkpoint.output_bytes.is_none()
                    || checkpoint.sha256.is_none()
                {
                    return Err(Failure::new(
                        "media.mux_input_incomplete",
                        "Every mux input must be a complete, verified media task.",
                    ));
                }
                mux::validate_input(&checkpoint.filename)?;
                let configured =
                    settings
                        .write_roots
                        .get(checkpoint.root_index)
                        .ok_or_else(|| {
                            Failure::new(
                                "media.destination",
                                "An input's folder is no longer configured.",
                            )
                        })?;
                let root = SafeRoot::open(Path::new(configured)).map_err(|_| {
                    Failure::new("media.destination", "An input's folder is unavailable.")
                })?;
                if root.path() != checkpoint.root {
                    return Err(Failure::new(
                        "media.destination",
                        "An input's configured folder changed.",
                    ));
                }
                Ok(checkpoint)
            })
            .collect::<Result<Vec<_>>>()?;
        drop(tasks);
        let input_bytes = inputs
            .iter()
            .try_fold(0u64, |total, input| {
                total.checked_add(input.output_bytes.unwrap_or(0))
            })
            .filter(|total| *total <= mux::INPUT_BYTES)
            .ok_or_else(|| {
                Failure::new(
                    "media.mux_input_limit",
                    "Mux inputs exceed the 100 GiB staging ceiling.",
                )
            })?;
        let identities = inputs.iter().map(|input| json!({"task_id": input.task_id, "filename": input.filename, "bytes": input.output_bytes, "sha256": input.sha256})).collect::<Vec<_>>();
        let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(
            &json!({"profile": "zeus-media-stream-copy-v1", "inputs": identities, "container": request.container, "helper_sha256": helper.sha256}),
        )?));
        let plan_id = zeus_app::review::random_id();
        let filename = format!("media-mux-{}.{}", &plan_id[..8], request.container);
        let helper_view = helper.view();
        {
            let mut plans = self.mux_plans.lock().map_err(|_| state_error())?;
            plans.retain(|_, plan| plan.created.elapsed() < CACHE_TTL);
            if plans.len() >= 16 {
                return Err(Failure::new(
                    "media.busy",
                    "Too many mux reviews are waiting.",
                ));
            }
            plans.insert(
                plan_id.clone(),
                MuxPlan {
                    created: Instant::now(),
                    inputs,
                    container: request.container.clone(),
                    fingerprint: fingerprint.clone(),
                    helper,
                    roots: settings.write_roots.clone(),
                },
            );
        }
        Ok(
            json!({"plan_id": plan_id, "fingerprint": fingerprint, "filename": filename, "container": request.container,
            "inputs": identities, "input_bytes": input_bytes, "roots": settings.write_roots, "helper": helper_view,
            "notices": ["Stream copy only: incompatible codecs fail without publishing an output.", "Inputs are independently copied and SHA-256 verified; originals are preserved.", "FFmpeg receives only fixed local-file arguments. Its process job is not an OS filesystem or network sandbox.", "MP4 is written as fragmented MP4 so output can be hard-capped through a pipe."],
            "default_max_bytes": 4u64 * 1024 * 1024 * 1024, "default_timeout_ms": 300_000, "expires_in_ms": CACHE_TTL.as_millis()}),
        )
    }

    /// Start an explicitly reviewed local mux job. The helper pin must still
    /// match the review, and the frontend cannot supply executable arguments.
    pub fn mux_start(self: &Arc<Self>, body: &Value) -> Result<Value> {
        let request: MuxStart = serde_json::from_value(body.clone())?;
        if request.max_bytes == 0
            || request.max_bytes > MAX_BYTES
            || !(1_000..=3_600_000).contains(&request.timeout_ms)
            || !(16..=128).contains(&request.idempotency_key.len())
            || !request.idempotency_key.is_ascii()
        {
            return Err(Failure::invalid(
                "Choose a positive output ceiling up to 100 GiB, a one-second to one-hour deadline, and an idempotency key.",
            ));
        }
        let plan = self
            .mux_plans
            .lock()
            .map_err(|_| state_error())?
            .get(&request.plan_id)
            .filter(|plan| plan.created.elapsed() < CACHE_TTL)
            .cloned()
            .ok_or_else(|| {
                Failure::new(
                    "media.review_expired",
                    "Preview the mux operation again before starting.",
                )
            })?;
        if !valid_filename(&request.filename, Some(&plan.container)) {
            return Err(Failure::invalid(
                "Use a safe filename with the selected output container's extension.",
            ));
        }
        let helper = runner::Pin::load(&self.home.app_dir())?;
        if helper.sha256 != plan.helper.sha256 || helper.executable != plan.helper.executable {
            return Err(Failure::new(
                "media.helper_changed",
                "The operator's helper pin changed after this review.",
            ));
        }
        let settings = zeus_app::engine_config::load(&self.home)?;
        if settings.write_roots != plan.roots {
            return Err(Failure::new(
                "media.review_changed",
                "The configured destination folders changed after the mux review. Preview it again.",
            ));
        }
        let configured = settings
            .write_roots
            .get(request.root_index)
            .ok_or_else(|| Failure::invalid("Choose a configured download folder."))?;
        let root = SafeRoot::open(Path::new(configured)).map_err(|_| {
            Failure::new(
                "media.destination",
                "The configured output folder is unavailable.",
            )
        })?;
        let task_id = format!(
            "media-{}",
            &hex::encode(Sha256::digest(request.idempotency_key.as_bytes()))[..32]
        );
        let progress = {
            let mut tasks = self.tasks.lock().map_err(|_| state_error())?;
            if let Some(task) = tasks.get(&task_id) {
                let prior = task.snapshot()?;
                if prior.kind != "mux"
                    || prior.fingerprint != plan.fingerprint
                    || prior.filename != request.filename
                    || prior.root != root.path()
                    || prior.max_bytes != request.max_bytes
                    || prior.timeout_ms != Some(request.timeout_ms)
                {
                    return Err(Failure::new(
                        "media.idempotency_conflict",
                        "This idempotency key belongs to another media operation.",
                    ));
                }
                return task_view(task);
            }
            require_unused_staging(&self.home, &task_id)?;
            if tasks.len() >= MAX_TASKS {
                return Err(Failure::new(
                    "media.history_full",
                    "Archive inactive media tasks before starting another conversion.",
                ));
            }
            if tasks
                .values()
                .filter(|task| {
                    task.snapshot().is_ok_and(|state| {
                        matches!(
                            state.state.as_str(),
                            "downloading" | "assembling" | "converting"
                        )
                    })
                })
                .count()
                >= 2
            {
                return Err(Failure::new(
                    "media.busy",
                    "At most two media tasks can run together.",
                ));
            }
            let target = root
                .join_checked(Path::new(&request.filename))
                .map_err(|_| Failure::invalid("The output name is invalid."))?;
            if RealFs
                .metadata(&target)
                .map_err(|_| state_error())?
                .is_some()
            {
                return Err(Failure::new(
                    "media.destination_exists",
                    "The converted output already exists.",
                ));
            }
            let checkpoint = Checkpoint {
                task_id: task_id.clone(),
                fingerprint: plan.fingerprint,
                label: "Local media stream-copy mux".into(),
                root_index: request.root_index,
                root: root.path().to_path_buf(),
                filename: request.filename,
                state: "converting".into(),
                kind: "mux".into(),
                source_task_ids: plan
                    .inputs
                    .iter()
                    .map(|input| input.task_id.clone())
                    .collect(),
                helper_sha256: Some(helper.sha256.clone()),
                max_bytes: request.max_bytes,
                connections: 1,
                timeout_ms: Some(request.timeout_ms),
                assembly_started: false,
                segment_count: plan.inputs.len(),
                receipts: BTreeMap::new(),
                network_bytes: 0,
                output_bytes: None,
                sha256: None,
                error_code: None,
                error_message: None,
            };
            let progress = Arc::new(Progress::new(checkpoint));
            tasks.insert(task_id.clone(), progress.clone());
            progress
        };
        let directory = self.home.app_dir().join("media").join(&task_id);
        let prepared = RealFs
            .create_dir_all(&directory)
            .map_err(|_| state_error())
            .and_then(|()| transfer::save(&directory, &progress));
        if let Err(error) = prepared {
            if let Ok(mut checkpoint) = progress.checkpoint.lock() {
                checkpoint.state = "failed".into();
                checkpoint.error_code = Some(error.code.clone());
                checkpoint.error_message = Some(error.message.clone());
            }
            return Err(error);
        }
        let worker_progress = progress.clone();
        let handle = tokio::spawn(async move {
            if let Err(error) = mux::execute(
                helper,
                plan.inputs,
                plan.container,
                directory.clone(),
                worker_progress.clone(),
                Duration::from_millis(request.timeout_ms),
            )
            .await
            {
                if let Ok(mut checkpoint) = worker_progress.checkpoint.lock() {
                    checkpoint.state = if error.code == "media.canceled" {
                        "canceled"
                    } else {
                        "failed"
                    }
                    .into();
                    checkpoint.error_code = Some(error.code);
                    checkpoint.error_message = Some(error.message);
                }
                let _ = transfer::save(&directory, &worker_progress);
            }
        });
        self.handles
            .lock()
            .map_err(|_| state_error())?
            .insert(task_id, handle);
        task_view(&progress)
    }

    /// Redacted task progress, including interrupted tasks from previous sessions.
    pub fn list(&self) -> Result<Value> {
        let tasks = self.tasks.lock().map_err(|_| state_error())?;
        let items = tasks
            .values()
            .map(|task| task_view(task))
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"items": items, "unreadable_checkpoints": self.unreadable_checkpoints}))
    }

    /// Stop an acquisition without deleting durable verified segments.
    pub fn cancel(&self, body: &Value) -> Result<Value> {
        let id = body.get("task_id").and_then(Value::as_str).unwrap_or("");
        let tasks = self.tasks.lock().map_err(|_| state_error())?;
        let task = tasks
            .get(id)
            .ok_or_else(|| Failure::new("media.task_missing", "The media task was not found."))?;
        task.cancel.store(true, Ordering::Release);
        task_view(task)
    }

    /// Recover a completely retained acquisition without using its source URL.
    /// The current configured root must still match the original destination.
    pub fn recover(self: &Arc<Self>, body: &Value) -> Result<Value> {
        let id = body
            .get("task_id")
            .and_then(Value::as_str)
            .filter(|id| task_identity(id))
            .ok_or_else(|| Failure::invalid("The media task identity is invalid."))?;
        let progress = {
            let mut tasks = self.tasks.lock().map_err(|_| state_error())?;
            let task = tasks.get(id).ok_or_else(|| {
                Failure::new("media.task_missing", "The media task was not found.")
            })?;
            let mut checkpoint = task.snapshot()?;
            if matches!(
                checkpoint.state.as_str(),
                "downloading" | "assembling" | "converting"
            ) {
                return Err(Failure::new(
                    "media.task_active",
                    "This media task is already active.",
                ));
            }
            if !recoverable(&checkpoint) {
                return Err(Failure::new(
                    "media.recovery_incomplete",
                    "This task needs a fresh source inspection before it can resume.",
                ));
            }
            let settings = zeus_app::engine_config::load(&self.home)?;
            let configured = settings
                .write_roots
                .get(checkpoint.root_index)
                .ok_or_else(|| {
                    Failure::new(
                        "media.destination",
                        "The original download folder is no longer configured.",
                    )
                })?;
            let root = SafeRoot::open(Path::new(configured)).map_err(|_| {
                Failure::new(
                    "media.destination",
                    "The original download folder is unavailable.",
                )
            })?;
            if root.path() != checkpoint.root {
                return Err(Failure::new(
                    "media.destination",
                    "The configured download folder changed. Recovery refused to use another folder.",
                ));
            }
            if tasks
                .values()
                .filter(|task| {
                    task.snapshot().is_ok_and(|state| {
                        matches!(
                            state.state.as_str(),
                            "downloading" | "assembling" | "converting"
                        )
                    })
                })
                .count()
                >= 2
            {
                return Err(Failure::new(
                    "media.busy",
                    "At most two media tasks can run together.",
                ));
            }
            checkpoint.state = "assembling".into();
            let progress = Arc::new(Progress::new(checkpoint));
            tasks.insert(id.to_owned(), progress.clone());
            progress
        };
        let directory = self.home.app_dir().join("media").join(id);
        let worker_progress = progress.clone();
        let converter = progress.snapshot()?.kind == "mux";
        let handle = tokio::spawn(async move {
            let result = if converter {
                mux::recover(directory.clone(), worker_progress.clone()).await
            } else {
                transfer::recover(directory.clone(), worker_progress.clone()).await
            };
            if let Err(error) = result
                && let Ok(mut checkpoint) = worker_progress.checkpoint.lock()
            {
                checkpoint.state = "failed".into();
                checkpoint.error_code = Some(error.code);
                checkpoint.error_message = Some(error.message);
            }
            let _ = transfer::save(&directory, &worker_progress);
        });
        self.handles
            .lock()
            .map_err(|_| state_error())?
            .insert(id.to_owned(), handle);
        task_view(&progress)
    }

    /// Remove only an inactive task's private segment cache and receipt, never
    /// the completed media output. This bounds disk use without a file delete authority.
    pub fn archive(&self, body: &Value) -> Result<Value> {
        let id = body
            .get("task_id")
            .and_then(Value::as_str)
            .filter(|id| task_identity(id))
            .ok_or_else(|| Failure::invalid("The media task identity is invalid."))?;
        let mut tasks = self.tasks.lock().map_err(|_| state_error())?;
        let task = tasks
            .get(id)
            .ok_or_else(|| Failure::new("media.task_missing", "The media task was not found."))?;
        if matches!(
            task.snapshot()?.state.as_str(),
            "downloading" | "assembling" | "converting"
        ) {
            return Err(Failure::new(
                "media.task_active",
                "Stop the media task before archiving its segment cache.",
            ));
        }
        let dir = self.home.app_dir().join("media").join(id);
        let root = SafeRoot::open(&self.home.app_dir()).map_err(|_| state_error())?;
        root.check_existing(&dir).map_err(|_| state_error())?;
        let checkpoint = task.snapshot()?;
        if checkpoint.kind == "mux" {
            for index in 0..checkpoint.segment_count {
                let input = dir.join(format!("segment-{index:04}.bin"));
                if RealFs
                    .metadata(&input)
                    .map_err(|_| state_error())?
                    .is_some_and(|meta| meta.is_regular_file)
                {
                    RealFs
                        .set_readonly(&input, false)
                        .map_err(|_| state_error())?;
                }
            }
        }
        std::fs::remove_dir_all(&dir)?;
        tasks.remove(id);
        self.handles.lock().map_err(|_| state_error())?.remove(id);
        Ok(json!({"ok": true, "output_preserved": true}))
    }

    /// Stop all workers and wait for their writes to drain before the app exits.
    pub async fn shutdown(&self) {
        if let Ok(tasks) = self.tasks.lock() {
            for task in tasks.values() {
                task.cancel.store(true, Ordering::Release);
            }
        }
        let handles = self
            .handles
            .lock()
            .map(|mut handles| handles.drain().map(|(_, h)| h).collect::<Vec<_>>())
            .unwrap_or_default();
        for handle in handles {
            let _ = handle.await;
        }
    }
}

/// A task projection without source addresses, ETags or per-segment private state.
pub fn task_view(progress: &Progress) -> Result<Value> {
    let checkpoint = progress.snapshot()?;
    let published_bytes = if checkpoint.state == "complete" {
        checkpoint.output_bytes
    } else {
        None
    };
    let published_sha256 = if checkpoint.state == "complete" {
        checkpoint.sha256.clone()
    } else {
        None
    };
    let verified_bytes = checkpoint
        .receipts
        .values()
        .try_fold(0u64, |total, receipt| total.checked_add(receipt.bytes))
        .ok_or_else(|| {
            Failure::new(
                "media.state_corrupt",
                "Stored media byte totals overflow their bounds.",
            )
        })?;
    Ok(json!({
        "task_id": checkpoint.task_id, "fingerprint": checkpoint.fingerprint, "label": checkpoint.label,
        "state": checkpoint.state, "filename": checkpoint.filename, "root_index": checkpoint.root_index,
        "kind": checkpoint.kind,
        "recovery_available": recoverable(&checkpoint),
        "source_task_ids": checkpoint.source_task_ids, "helper_sha256": checkpoint.helper_sha256,
        "segment_count": checkpoint.segment_count, "completed_segments": checkpoint.receipts.len(),
        "verified_bytes": verified_bytes, "received_bytes": checkpoint.network_bytes,
        "output_bytes": published_bytes, "sha256": published_sha256,
        "error_code": checkpoint.error_code, "error_message": checkpoint.error_message,
    }))
}
