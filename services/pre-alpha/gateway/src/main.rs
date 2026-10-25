//! `zeus-web-bridge`: an HTTP bridge to zeusd for the Zeus pre-alpha client (ADR 0010: the daemon keeps one local
//! listener; HTTP surfaces are separate bridge processes).
//!
//! The bridge offers a closed set of typed operations, the ones the restricted desktop client offers: job pages,
//! link preview and enqueue of a reviewed batch, pause/resume/cancel/retry, files under the download root, artifact
//! integrity facts, the approvals this identity may see and workflow reports. It never decides an approval, changes a
//! grant, reveals an original address or writes a file.
//!
//! Configuration is by environment (see `Config::from_env`). In `hosted` mode every visitor gets an anonymous
//! sandbox; in `local` mode the bridge serves one person on loopback with a token from an owner-only file.

#![forbid(unsafe_code)]

mod daemon;
mod sessions;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};
use zeus_types::{ApprovalState, HttpUrl, JobId};

use daemon::{Action, Daemon, Failure, Status, Wanted};
use sessions::{Sessions, now_ms, random_id, well_formed};

const REVIEW_FOR: Duration = Duration::from_secs(600);
const MAX_LINKS: usize = 50;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const COOKIE: &str = "zeus_sid";

/// Hosted (visitor sandboxes, fixture hosts only) or local (one person, loopback, token file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Hosted,
    Local,
}

/// The fixture catalogue: the only hosts a hosted visitor may download from, and the sizes of their files.
#[derive(Debug, Clone, Default, Deserialize)]
struct Catalogue {
    hosts: Vec<String>,
    /// `host/path` to size in bytes.
    #[serde(default)]
    sizes: BTreeMap<String, u64>,
    /// Suggested links shown in the client, passed through as is.
    #[serde(default)]
    samples: Vec<Value>,
}

#[derive(Debug, Clone)]
struct Config {
    mode: Mode,
    listen: SocketAddr,
    state_dir: PathBuf,
    /// The daemon's write root under which session folders live (hosted), or the default root (local, optional).
    root: Option<PathBuf>,
    catalogue: Catalogue,
    origins: Vec<String>,
    quota_files: u32,
    quota_bytes: u64,
    ttl_ms: u64,
    max_sessions: usize,
    secure_cookie: bool,
    token: Option<String>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_num<T: std::str::FromStr>(name: &str, default: T) -> anyhow::Result<T> {
    match env(name) {
        Some(v) => v
            .parse()
            .map_err(|_| anyhow::anyhow!("{name} must be a number")),
        None => Ok(default),
    }
}

impl Config {
    /// `ZEUS_BRIDGE_MODE` (`local` | `hosted`), `ZEUS_BRIDGE_LISTEN`, `ZEUS_BRIDGE_STATE_DIR`, `ZEUS_BRIDGE_ROOT`,
    /// `ZEUS_BRIDGE_CATALOGUE`, `ZEUS_BRIDGE_ORIGINS`, `ZEUS_BRIDGE_QUOTA_FILES`, `ZEUS_BRIDGE_QUOTA_MIB`,
    /// `ZEUS_BRIDGE_TTL_HOURS`, `ZEUS_BRIDGE_MAX_SESSIONS`, `ZEUS_BRIDGE_INSECURE_COOKIE`.
    fn from_env() -> anyhow::Result<Self> {
        let mode = match env("ZEUS_BRIDGE_MODE").as_deref() {
            None | Some("local") => Mode::Local,
            Some("hosted") => Mode::Hosted,
            Some(_) => bail!("ZEUS_BRIDGE_MODE must be local or hosted"),
        };
        let listen: SocketAddr = env("ZEUS_BRIDGE_LISTEN")
            .unwrap_or_else(|| "127.0.0.1:18545".into())
            .parse()
            .context("ZEUS_BRIDGE_LISTEN must be an address such as 127.0.0.1:18545")?;
        if mode == Mode::Local && !listen.ip().is_loopback() {
            bail!("local mode listens on loopback only");
        }
        let state_dir = match env("ZEUS_BRIDGE_STATE_DIR") {
            Some(d) => PathBuf::from(d),
            None => zeus_api::endpoint::ZeusPaths::discover()
                .map_err(|_| {
                    anyhow::anyhow!(
                        "cannot find the Zeus state directory; set ZEUS_BRIDGE_STATE_DIR"
                    )
                })?
                .state_dir
                .join("bridge"),
        };
        std::fs::create_dir_all(&state_dir).context("create the bridge state directory")?;
        let root = env("ZEUS_BRIDGE_ROOT").map(PathBuf::from);
        let catalogue = match env("ZEUS_BRIDGE_CATALOGUE") {
            Some(p) => {
                serde_json::from_slice(&std::fs::read(&p).context("read the fixture catalogue")?)
                    .context("parse the fixture catalogue")?
            }
            None => Catalogue::default(),
        };
        if mode == Mode::Hosted {
            if root.is_none() {
                bail!("hosted mode needs ZEUS_BRIDGE_ROOT, the daemon's write root");
            }
            if catalogue.hosts.is_empty() {
                bail!("hosted mode needs ZEUS_BRIDGE_CATALOGUE with at least one fixture host");
            }
        }
        let origins = env("ZEUS_BRIDGE_ORIGINS")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    "tauri://localhost".into(),
                    "http://tauri.localhost".into(),
                    "https://tauri.localhost".into(),
                ]
            });
        let token = match mode {
            Mode::Hosted => None,
            Mode::Local => Some(local_token(&state_dir)?),
        };
        Ok(Self {
            mode,
            listen,
            state_dir,
            root,
            catalogue,
            origins,
            quota_files: env_num("ZEUS_BRIDGE_QUOTA_FILES", 40)?,
            quota_bytes: env_num::<u64>("ZEUS_BRIDGE_QUOTA_MIB", 1024)? * 1024 * 1024,
            ttl_ms: env_num::<u64>("ZEUS_BRIDGE_TTL_HOURS", 24)? * 3_600_000,
            max_sessions: env_num("ZEUS_BRIDGE_MAX_SESSIONS", 5000)?,
            secure_cookie: env("ZEUS_BRIDGE_INSECURE_COOKIE").is_none(),
            token,
        })
    }
}

/// The local token: read from `bridge.token`, or created owner-only on first start.
fn local_token(dir: &Path) -> anyhow::Result<String> {
    let path = dir.join("bridge.token");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let t = text.trim().to_owned();
        if well_formed(&t) {
            return Ok(t);
        }
    }
    let t = random_id();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options
        .open(&path)
        .context("create bridge.token")?
        .write_all(t.as_bytes())
        .context("write bridge.token")?;
    Ok(t)
}

#[derive(Default)]
struct Cache {
    jobs: Vec<Value>,
    approvals: Vec<Value>,
    artifacts: Vec<Value>,
    runs: Vec<Value>,
    status: Option<Status>,
    failure: Option<Failure>,
    updated_ms: u64,
}

struct Review {
    sid: String,
    created: Instant,
    wanted: Vec<Wanted>,
    bytes: u64,
}

struct App {
    cfg: Config,
    daemon: Daemon,
    sessions: Mutex<Sessions>,
    reviews: Mutex<HashMap<String, Review>>,
    cache: RwLock<Cache>,
}

type Shared = Arc<App>;

// ---------------------------------------------------------------- errors

struct Fail(StatusCode, Failure);

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        let mut r = (self.0, Json(json!({ "error": self.1 }))).into_response();
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        r
    }
}

fn bad(code: &str, message: &str) -> Fail {
    Fail(StatusCode::BAD_REQUEST, Failure::new(code, message))
}

fn from_daemon(f: Failure) -> Fail {
    let status = if f.code.starts_with("bridge.") {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::BAD_GATEWAY
    };
    Fail(status, f)
}

fn ok(v: Value) -> Response {
    let mut r = Json(v).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

// ---------------------------------------------------------------- who is asking

fn presented(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(t) = v.strip_prefix("Bearer ")
    {
        return Some(t.trim().to_owned());
    }
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == COOKIE).then(|| v.to_owned())
        })
}

/// The session this request belongs to: a live hosted session, or `local` with the right token.
async fn session_of(app: &App, headers: &HeaderMap) -> Result<String, Fail> {
    let missing = || {
        Fail(
            StatusCode::UNAUTHORIZED,
            Failure::new(
                "bridge.session_required",
                "Start a session first (POST /api/v1/session).",
            ),
        )
    };
    let given = presented(headers).ok_or_else(missing)?;
    match app.cfg.mode {
        Mode::Local => {
            if app.cfg.token.as_deref() == Some(given.as_str()) {
                Ok("local".into())
            } else {
                Err(Fail(
                    StatusCode::UNAUTHORIZED,
                    Failure::new(
                        "bridge.token_required",
                        "Paste the bridge token from bridge.token into Settings, Backend.",
                    ),
                ))
            }
        }
        Mode::Hosted => {
            if !well_formed(&given) {
                return Err(missing());
            }
            let sessions = app.sessions.lock().await;
            sessions
                .get(&given, app.cfg.ttl_ms)
                .map(|s| s.id.clone())
                .ok_or_else(missing)
        }
    }
}

/// The folder a session downloads into.
fn session_root(app: &App, sid: &str) -> Option<PathBuf> {
    match app.cfg.mode {
        Mode::Hosted => app.cfg.root.as_ref().map(|r| r.join("s").join(sid)),
        Mode::Local => app.cfg.root.clone(),
    }
}

/// The same folder as the daemon reports it (canonical: resolved links, `\?\` on Windows).
fn canonical_root(app: &App, sid: &str) -> Option<PathBuf> {
    session_root(app, sid).map(|p| std::fs::canonicalize(&p).unwrap_or(p))
}

/// `path` relative to `base` with `/` separators, or `None` when it is outside.
fn relative(path: &str, base: Option<&Path>) -> Option<String> {
    let p = Path::new(path);
    let rel = match base {
        Some(b) => p.strip_prefix(b).ok()?,
        None => p,
    };
    Some(
        rel.components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

async fn owned_jobs(app: &App, sid: &str) -> Option<HashSet<String>> {
    if app.cfg.mode == Mode::Local {
        return None;
    }
    let sessions = app.sessions.lock().await;
    Some(
        sessions
            .get(sid, app.cfg.ttl_ms)
            .map(|s| s.jobs.iter().cloned().collect())
            .unwrap_or_default(),
    )
}

fn pick(v: &Value, fields: &[&str]) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for f in fields {
        if let Some(x) = v.get(*f) {
            out.insert((*f).into(), x.clone());
        }
    }
    out
}

// ---------------------------------------------------------------- routes

async fn create_session(State(app): State<Shared>, headers: HeaderMap) -> Result<Response, Fail> {
    let desktop = headers.get("x-zeus-client").and_then(|v| v.to_str().ok()) == Some("desktop");
    let sid = match session_of(&app, &headers).await {
        Ok(sid) => sid,
        Err(_) if app.cfg.mode == Mode::Hosted => {
            let mut sessions = app.sessions.lock().await;
            if sessions.len() >= app.cfg.max_sessions {
                return Err(Fail(
                    StatusCode::SERVICE_UNAVAILABLE,
                    Failure::new(
                        "bridge.capacity",
                        "The hosted pre-alpha is full right now. Try again later.",
                    ),
                ));
            }
            let s = sessions.create();
            drop(sessions);
            if let Some(dir) = session_root(&app, &s.id) {
                std::fs::create_dir_all(&dir).map_err(|_| {
                    Fail(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Failure::new("bridge.storage", "Could not prepare your folder."),
                    )
                })?;
            }
            s.id
        }
        Err(e) => return Err(e),
    };
    let (created, files, bytes, jobs) = {
        let sessions = app.sessions.lock().await;
        sessions
            .get(&sid, app.cfg.ttl_ms)
            .map(|s| (s.created_ms, s.files, s.bytes, s.jobs.len()))
            .unwrap_or((now_ms(), 0, 0, 0))
    };
    let mut body = json!({
        "mode": if app.cfg.mode == Mode::Hosted { "hosted" } else { "local" },
        "bridge_version": env!("CARGO_PKG_VERSION"),
        "created_ms": created,
        "expires_ms": if app.cfg.mode == Mode::Hosted { Some(created + app.cfg.ttl_ms) } else { None },
        "quota": if app.cfg.mode == Mode::Hosted { json!({"files": app.cfg.quota_files, "bytes": app.cfg.quota_bytes}) } else { Value::Null },
        "used": {"files": files, "bytes": bytes, "jobs": jobs},
        "fixture_hosts": app.cfg.catalogue.hosts,
        "root_label": if app.cfg.mode == Mode::Hosted { "Downloads".to_owned() } else { app.cfg.root.as_ref().map(|r| r.display().to_string()).unwrap_or_else(|| "Default download folder".into()) },
    });
    if desktop && app.cfg.mode == Mode::Hosted {
        body["token"] = json!(sid);
    }
    let mut r = ok(body);
    if app.cfg.mode == Mode::Hosted {
        let cookie = format!(
            "{COOKIE}={sid}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax{}",
            app.cfg.ttl_ms / 1000,
            if app.cfg.secure_cookie {
                "; Secure"
            } else {
                ""
            }
        );
        if let Ok(v) = HeaderValue::from_str(&cookie) {
            r.headers_mut().insert(header::SET_COOKIE, v);
        }
    }
    Ok(r)
}

/// For the front proxy's `auth_request`: 204 with the session in `x-zeus-session`, or 401.
async fn auth(State(app): State<Shared>, headers: HeaderMap) -> Response {
    match session_of(&app, &headers).await {
        Ok(sid) => {
            let mut r = StatusCode::NO_CONTENT.into_response();
            if let Ok(v) = HeaderValue::from_str(&sid) {
                r.headers_mut().insert("x-zeus-session", v);
            }
            r
        }
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn status(State(app): State<Shared>, headers: HeaderMap) -> Result<Response, Fail> {
    session_of(&app, &headers).await?;
    let cache = app.cache.read().await;
    Ok(ok(json!({
        "daemon": cache.status,
        "failure": cache.failure,
        "updated_ms": cache.updated_ms,
    })))
}

const JOB_FIELDS: &[&str] = &[
    "id",
    "state",
    "filename",
    "url_redacted",
    "bytes_done",
    "bytes_total",
    "rate_ewma",
    "eta_s",
    "quarantined",
    "source_kind",
    "sha256",
    "created_at",
    "updated_at",
];

async fn jobs(State(app): State<Shared>, headers: HeaderMap) -> Result<Response, Fail> {
    let sid = session_of(&app, &headers).await?;
    let owned = owned_jobs(&app, &sid).await;
    let base = canonical_root(&app, &sid);
    let cache = app.cache.read().await;
    let items: Vec<Value> = cache
        .jobs
        .iter()
        .filter(|j| match (&owned, j.get("id").and_then(Value::as_str)) {
            (Some(set), Some(id)) => set.contains(id),
            (None, _) => true,
            _ => false,
        })
        .map(|j| {
            let mut out = pick(j, JOB_FIELDS);
            if let Some(p) = j
                .get("path")
                .and_then(Value::as_str)
                .and_then(|p| relative(p, base.as_deref()))
            {
                out.insert("path".into(), json!(p));
            }
            if let Some(code) = j.pointer("/last_error/code") {
                out.insert("error_code".into(), code.clone());
            }
            Value::Object(out)
        })
        .collect();
    Ok(ok(
        json!({ "items": items, "updated_ms": cache.updated_ms, "failure": cache.failure }),
    ))
}

#[derive(Deserialize)]
struct PreviewBody {
    urls: Vec<String>,
    #[serde(default)]
    subdir: Option<String>,
}

fn clean_subdir(s: &str) -> Option<PathBuf> {
    let s = s.trim().replace('\\', "/");
    if s.is_empty() {
        return Some(PathBuf::new());
    }
    if s.len() > 200
        || s.chars()
            .any(|c| c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return None;
    }
    let p = PathBuf::from(s.trim_matches('/'));
    let parts: Vec<_> = p.components().collect();
    if parts.len() > 4 || !parts.iter().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(p)
}

async fn preview(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<PreviewBody>,
) -> Result<Response, Fail> {
    let sid = session_of(&app, &headers).await?;
    let limit = || {
        bad(
            "api.invalid_params",
            "Use 1 to 50 unique http or https links, at most 64 KiB in total.",
        )
    };
    if body.urls.is_empty() || body.urls.iter().map(String::len).sum::<usize>() > MAX_INPUT_BYTES {
        return Err(limit());
    }
    let subdir = match body.subdir.as_deref() {
        Some(s) => clean_subdir(s).ok_or_else(|| {
            bad(
                "bridge.bad_folder",
                "Use a plain folder name inside your download folder.",
            )
        })?,
        None => PathBuf::new(),
    };
    let dest_dir = session_root(&app, &sid)
        .map(|r| r.join(&subdir))
        .map(|p| p.display().to_string());
    if let Some(d) = &dest_dir {
        let _ = std::fs::create_dir_all(d);
    }
    let mut seen = HashSet::new();
    let mut wanted = Vec::new();
    let mut refused = Vec::new();
    let mut bytes = 0u64;
    for (index, input) in body.urls.iter().enumerate() {
        let input = input.trim();
        if input.is_empty() || !seen.insert(input.to_owned()) {
            continue;
        }
        let parsed = url::Url::parse(input);
        let reason = match &parsed {
            Err(_) => Some("Not a valid address."),
            Ok(u) if !matches!(u.scheme(), "http" | "https") => {
                Some("Zeus downloads http and https addresses only.")
            }
            Ok(u) if !u.username().is_empty() || u.password().is_some() => {
                Some("No user name or password in the address.")
            }
            Ok(u)
                if app.cfg.mode == Mode::Hosted
                    && !u.host_str().is_some_and(|h| {
                        app.cfg
                            .catalogue
                            .hosts
                            .iter()
                            .any(|x| x.eq_ignore_ascii_case(h))
                    }) =>
            {
                Some(
                    "The hosted pre-alpha downloads only from its sample file hosts, never from the internet. Install Zeus to download from any site.",
                )
            }
            _ => None,
        };
        if let Some(reason) = reason {
            refused.push(json!({"index": index, "reason": reason}));
            continue;
        }
        let u = parsed.map_err(|_| limit())?;
        if app.cfg.mode == Mode::Hosted {
            let key = format!(
                "{}{}",
                u.host_str().unwrap_or_default().to_ascii_lowercase(),
                u.path()
            );
            bytes += app.cfg.catalogue.sizes.get(&key).copied().unwrap_or(0);
        }
        let url = HttpUrl::parse(input).map_err(|_| limit())?;
        wanted.push((
            index,
            Wanted {
                url,
                dest_dir: dest_dir.clone(),
            },
        ));
        if wanted.len() > MAX_LINKS {
            return Err(limit());
        }
    }
    if !refused.is_empty() {
        return Ok(ok(
            json!({"review_id": null, "ready": false, "items": [], "refused": refused}),
        ));
    }
    if wanted.is_empty() {
        return Err(limit());
    }
    if app.cfg.mode == Mode::Hosted {
        let sessions = app.sessions.lock().await;
        if let Some(s) = sessions.get(&sid, app.cfg.ttl_ms) {
            if s.files as usize + wanted.len() > app.cfg.quota_files as usize {
                return Err(Fail(
                    StatusCode::TOO_MANY_REQUESTS,
                    Failure::new(
                        "bridge.quota_files",
                        "This sandbox has reached its file limit for today. Remove nothing: it resets after 24 hours.",
                    ),
                ));
            }
            if s.bytes + bytes > app.cfg.quota_bytes {
                return Err(Fail(
                    StatusCode::TOO_MANY_REQUESTS,
                    Failure::new(
                        "bridge.quota_bytes",
                        "This batch would pass the sandbox's size limit for today.",
                    ),
                ));
            }
        }
    }
    let list: Vec<Wanted> = wanted.iter().map(|(_, w)| w.clone()).collect();
    let result = app
        .daemon
        .enqueue(&list, true, None)
        .await
        .map_err(from_daemon)?;
    let base = canonical_root(&app, &sid);
    let entries = result
        .get("jobs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut ready = true;
    let items: Vec<Value> = entries
        .iter()
        .zip(wanted.iter())
        .map(|(e, (index, _))| match e.get("plan") {
            Some(plan) if !plan.is_null() => json!({
                "index": index,
                "url_redacted": plan.get("url_redacted"),
                "filename": plan.get("resolved_filename"),
                "destination": plan.get("dest_path").and_then(Value::as_str).and_then(|p| relative(p, base.as_deref())),
                "approval_needed": plan.get("approval_needed"),
                "warnings": plan.get("warnings"),
            }),
            _ => {
                ready = false;
                json!({"index": index, "error_code": e.pointer("/error/code")})
            }
        })
        .collect();
    if !ready || items.len() != wanted.len() {
        return Ok(ok(
            json!({"review_id": null, "ready": false, "items": items, "refused": []}),
        ));
    }
    let id = random_id();
    let mut reviews = app.reviews.lock().await;
    reviews.retain(|_, r| r.created.elapsed() <= REVIEW_FOR);
    let mine: Vec<String> = reviews
        .iter()
        .filter(|(_, r)| r.sid == sid)
        .map(|(k, _)| k.clone())
        .collect();
    if mine.len() >= 4
        && let Some(oldest) = mine
            .iter()
            .min_by_key(|k| reviews.get(*k).map(|r| r.created))
    {
        reviews.remove(oldest);
    }
    reviews.insert(
        id.clone(),
        Review {
            sid,
            created: Instant::now(),
            wanted: list,
            bytes,
        },
    );
    Ok(ok(
        json!({"review_id": id, "ready": true, "items": items, "refused": [], "bytes": bytes}),
    ))
}

#[derive(Deserialize)]
struct EnqueueBody {
    review_id: String,
    idempotency_key: String,
}

fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

async fn enqueue(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<EnqueueBody>,
) -> Result<Response, Fail> {
    let sid = session_of(&app, &headers).await?;
    if !valid_key(&body.idempotency_key) {
        return Err(bad(
            "api.invalid_params",
            "The retry key must have 1 to 64 letters, digits, '-', '_' or '.'.",
        ));
    }
    let (wanted, bytes) = {
        let reviews = app.reviews.lock().await;
        let r = reviews
            .get(&body.review_id)
            .filter(|r| r.sid == sid && r.created.elapsed() <= REVIEW_FOR)
            .ok_or_else(|| {
                bad(
                    "bridge.review_expired",
                    "Preview the links again before starting them.",
                )
            })?;
        (r.wanted.clone(), r.bytes)
    };
    if app.cfg.mode == Mode::Hosted {
        let sessions = app.sessions.lock().await;
        if let Some(s) = sessions.get(&sid, app.cfg.ttl_ms)
            && (s.files as usize + wanted.len() > app.cfg.quota_files as usize
                || s.bytes + bytes > app.cfg.quota_bytes)
        {
            return Err(Fail(
                StatusCode::TOO_MANY_REQUESTS,
                Failure::new(
                    "bridge.quota",
                    "This sandbox has reached its limit for today.",
                ),
            ));
        }
    }
    // The key is namespaced by session so two visitors can never replay each other's receipts.
    let key = format!("{}-{}", &sid[..sid.len().min(32)], body.idempotency_key);
    let result = app
        .daemon
        .enqueue(&wanted, false, Some(key))
        .await
        .map_err(from_daemon)?;
    let entries = result
        .get("jobs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ids: Vec<String> = entries
        .iter()
        .filter_map(|e| e.get("job_id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    if app.cfg.mode == Mode::Hosted {
        let mut sessions = app.sessions.lock().await;
        let n = ids.len() as u32;
        sessions.update(&sid, |s| {
            for id in &ids {
                if !s.jobs.contains(id) {
                    s.jobs.push(id.clone());
                }
            }
            s.files += n;
            s.bytes += bytes;
        });
    }
    app.reviews.lock().await.remove(&body.review_id);
    let jobs: Vec<Value> = entries
        .iter()
        .map(|e| json!({"job_id": e.get("job_id"), "state": e.get("state"), "approval_id": e.get("approval_id"), "error_code": e.pointer("/error/code")}))
        .collect();
    // Show the new jobs at once rather than at the next refresh.
    refresh_jobs(&app).await;
    Ok(ok(
        json!({"replayed": result.get("replayed"), "jobs": jobs}),
    ))
}

#[derive(Deserialize)]
struct ControlBody {
    action: Action,
    idempotency_key: String,
}

async fn control(
    State(app): State<Shared>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<ControlBody>,
) -> Result<Response, Fail> {
    let sid = session_of(&app, &headers).await?;
    if !valid_key(&body.idempotency_key) {
        return Err(bad(
            "api.invalid_params",
            "The retry key must have 1 to 64 letters, digits, '-', '_' or '.'.",
        ));
    }
    if let Some(owned) = owned_jobs(&app, &sid).await
        && !owned.contains(&id)
    {
        return Err(Fail(
            StatusCode::NOT_FOUND,
            Failure::new("job.not_found", "No such download in this sandbox."),
        ));
    }
    let job: JobId = id
        .parse()
        .map_err(|_| bad("api.invalid_params", "Select a valid download."))?;
    let key = format!("{}-{}", &sid[..sid.len().min(32)], body.idempotency_key);
    let r = app
        .daemon
        .control(job, body.action, key)
        .await
        .map_err(from_daemon)?;
    refresh_jobs(&app).await;
    Ok(ok(r))
}

async fn files(State(app): State<Shared>, headers: HeaderMap) -> Result<Response, Fail> {
    let sid = session_of(&app, &headers).await?;
    let base = canonical_root(&app, &sid);
    let dir = match app.cfg.mode {
        Mode::Hosted => base.as_ref().map(|b| b.display().to_string()),
        Mode::Local => app.cfg.root.as_ref().map(|b| b.display().to_string()),
    };
    let entries = app.daemon.files(dir, 10).await.map_err(from_daemon)?;
    let items: Vec<Value> = entries
        .iter()
        .filter_map(|f| {
            let path = f.get("path")?.as_str()?;
            let rel = match &base {
                Some(b) => {
                    let full = Path::new(f.get("root")?.as_str()?).join(path);
                    relative(&full.display().to_string(), Some(b))?
                }
                None => path.replace('\\', "/"),
            };
            let mut out = pick(
                f,
                &[
                    "size_bytes",
                    "modified_at",
                    "sha256",
                    "quarantined",
                    "partial",
                    "job_id",
                ],
            );
            out.insert("path".into(), json!(rel));
            Some(Value::Object(out))
        })
        .collect();
    Ok(ok(json!({"items": items})))
}

async fn artifacts(State(app): State<Shared>, headers: HeaderMap) -> Result<Response, Fail> {
    let sid = session_of(&app, &headers).await?;
    let owned = owned_jobs(&app, &sid).await;
    let cache = app.cache.read().await;
    let items: Vec<Value> = cache
        .artifacts
        .iter()
        .filter(
            |a| match (&owned, a.get("job_id").and_then(Value::as_str)) {
                (None, _) => true,
                (Some(set), Some(j)) => set.contains(j),
                _ => false,
            },
        )
        .map(|a| {
            Value::Object(pick(
                a,
                &[
                    "id",
                    "state",
                    "name",
                    "size_bytes",
                    "sha256",
                    "blake3",
                    "media_type",
                    "quarantined",
                    "created_at",
                    "job_id",
                    "run_id",
                ],
            ))
        })
        .collect();
    Ok(ok(json!({"items": items})))
}

async fn approvals(State(app): State<Shared>, headers: HeaderMap) -> Result<Response, Fail> {
    let sid = session_of(&app, &headers).await?;
    let owned = owned_jobs(&app, &sid).await;
    let cache = app.cache.read().await;
    let items: Vec<Value> = cache
        .approvals
        .iter()
        .filter(
            |a| match (&owned, a.get("job_id").and_then(Value::as_str)) {
                (None, _) => true,
                (Some(set), Some(j)) => set.contains(j),
                _ => false,
            },
        )
        .map(|a| {
            Value::Object(pick(
                a,
                &[
                    "id",
                    "kind",
                    "state",
                    "created_at",
                    "expires_at",
                    "decided_at",
                    "request_hash",
                    "allowed_scopes",
                    "reason_codes",
                    "summary",
                    "job_id",
                    "run_id",
                    "origin",
                    "revision",
                ],
            ))
        })
        .collect();
    Ok(ok(json!({
        "items": items,
        "decide": "Approvals are decided only by a person on a Zeus trust surface (the interactive CLI or the extension popup). This bridge can list them, never decide them.",
    })))
}

async fn runs(State(app): State<Shared>, headers: HeaderMap) -> Result<Response, Fail> {
    session_of(&app, &headers).await?;
    let cache = app.cache.read().await;
    let items: Vec<Value> = cache
        .runs
        .iter()
        .map(|r| {
            Value::Object(pick(
                r,
                &[
                    "run_id",
                    "status",
                    "plan_hash",
                    "pages_fetched",
                    "files_enqueued",
                    "files_completed",
                    "files_failed",
                    "bytes_done",
                    "started_at",
                    "finished_at",
                    "report_version",
                    "plan_id",
                    "created_at",
                    "updated_at",
                    "elapsed_ms",
                    "counts",
                    "pause_reason",
                    "possibly_incomplete",
                ],
            ))
        })
        .collect();
    Ok(ok(json!({"items": items})))
}

async fn fixtures(State(app): State<Shared>, headers: HeaderMap) -> Result<Response, Fail> {
    session_of(&app, &headers).await?;
    Ok(ok(
        json!({"hosts": app.cfg.catalogue.hosts, "samples": app.cfg.catalogue.samples}),
    ))
}

async fn health() -> Response {
    ok(json!({"ok": true}))
}

// ---------------------------------------------------------------- CORS for the desktop app

async fn cors(State(app): State<Shared>, req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let allowed = origin
        .as_ref()
        .filter(|o| app.cfg.origins.iter().any(|x| x == *o))
        .cloned();
    if req.method() == Method::OPTIONS {
        let mut r = StatusCode::NO_CONTENT.into_response();
        if let Some(o) = &allowed {
            add_cors(r.headers_mut(), o);
        }
        return r;
    }
    let mut r = next.run(req).await;
    if let Some(o) = &allowed {
        add_cors(r.headers_mut(), o);
    }
    r
}

fn add_cors(h: &mut HeaderMap, origin: &str) {
    if let Ok(v) = HeaderValue::from_str(origin) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
    }
    h.insert(header::VARY, HeaderValue::from_static("Origin"));
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization, content-type, x-zeus-client"),
    );
    h.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
}

// ---------------------------------------------------------------- the refresher

async fn refresh_jobs(app: &App) {
    match app.daemon.jobs(30).await {
        Ok(jobs) => {
            let mut c = app.cache.write().await;
            c.jobs = jobs;
            c.failure = None;
            c.updated_ms = now_ms();
        }
        Err(f) => app.cache.write().await.failure = Some(f),
    }
}

async fn refresh_slow(app: &App) {
    let status = app.daemon.status().await;
    let approvals = app
        .daemon
        .approvals(&[
            ApprovalState::Pending,
            ApprovalState::Approved,
            ApprovalState::Denied,
            ApprovalState::Expired,
        ])
        .await;
    let artifacts = app.daemon.artifacts(5).await;
    let runs = app.daemon.runs().await;
    let mut c = app.cache.write().await;
    match status {
        Ok(s) => c.status = Some(s),
        Err(f) => c.failure = Some(f),
    }
    if let Ok(a) = approvals {
        c.approvals = a;
    }
    if let Ok(a) = artifacts {
        c.artifacts = a;
    }
    if let Ok(r) = runs {
        c.runs = r;
    }
}

async fn refresher(app: Shared) {
    let mut n: u64 = 0;
    loop {
        refresh_jobs(&app).await;
        if n.is_multiple_of(4) {
            refresh_slow(&app).await;
        }
        if n.is_multiple_of(60) && app.cfg.mode == Mode::Hosted {
            app.sessions.lock().await.prune(app.cfg.ttl_ms);
            app.reviews
                .lock()
                .await
                .retain(|_, r| r.created.elapsed() <= REVIEW_FOR);
        }
        n = n.wrapping_add(1);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("zeus-web-bridge {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!(
            "zeus-web-bridge {}\n\nAn HTTP bridge to zeusd for the Zeus client. Local mode (default) listens on 127.0.0.1:18545 and\nrequires the token in <state>/bridge/bridge.token on every request.\n\nEnvironment: ZEUS_BRIDGE_MODE=local|hosted, ZEUS_BRIDGE_LISTEN, ZEUS_BRIDGE_STATE_DIR, ZEUS_BRIDGE_ROOT,\nZEUS_BRIDGE_CATALOGUE, ZEUS_BRIDGE_ORIGINS, ZEUS_BRIDGE_QUOTA_FILES, ZEUS_BRIDGE_QUOTA_MIB, ZEUS_BRIDGE_TTL_HOURS.\nThe daemon installation is found as the zeus CLI finds it (ZEUS_PROFILE, ZEUS_STATE_DIR, ...).",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    let cfg = Config::from_env()?;
    let sessions = Sessions::load(&cfg.state_dir);
    let listen = cfg.listen;
    if cfg.mode == Mode::Local {
        println!(
            "zeus-web-bridge {}: local mode on http://{listen}",
            env!("CARGO_PKG_VERSION")
        );
        println!(
            "token file: {}",
            cfg.state_dir.join("bridge.token").display()
        );
    } else {
        println!(
            "zeus-web-bridge {}: hosted mode on {listen}",
            env!("CARGO_PKG_VERSION")
        );
    }
    let app: Shared = Arc::new(App {
        cfg,
        daemon: Daemon::default(),
        sessions: Mutex::new(sessions),
        reviews: Mutex::new(HashMap::new()),
        cache: RwLock::new(Cache::default()),
    });
    tokio::spawn(refresher(app.clone()));
    let api = Router::new()
        .route("/session", post(create_session))
        .route("/auth", get(auth))
        .route("/status", get(status))
        .route("/jobs", get(jobs))
        .route("/jobs/{id}/control", post(control))
        .route("/links/preview", post(preview))
        .route("/links/enqueue", post(enqueue))
        .route("/files", get(files))
        .route("/artifacts", get(artifacts))
        .route("/approvals", get(approvals))
        .route("/runs", get(runs))
        .route("/fixtures", get(fixtures));
    let router = Router::new()
        .nest("/api/v1", api)
        .route("/healthz", get(health))
        .layer(middleware::from_fn_with_state(app.clone(), cors))
        .layer(DefaultBodyLimit::max(MAX_INPUT_BYTES + 4096))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("listen on {listen}"))?;
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_stay_inside_the_session() {
        assert_eq!(clean_subdir("Documents"), Some(PathBuf::from("Documents")));
        assert_eq!(clean_subdir("a\\b"), Some(PathBuf::from("a/b")));
        assert_eq!(clean_subdir(""), Some(PathBuf::new()));
        assert!(clean_subdir("../x").is_none());
        assert!(clean_subdir("/etc/passwd").is_some_and(|p| p == Path::new("etc/passwd")));
        assert!(clean_subdir("a/../../b").is_none());
        assert!(clean_subdir("C:x").is_none());
    }

    #[test]
    fn paths_outside_the_base_are_dropped() {
        let base = Path::new("/data/downloads/s/abc");
        assert_eq!(
            relative("/data/downloads/s/abc/Documents/a.pdf", Some(base)).as_deref(),
            Some("Documents/a.pdf")
        );
        assert!(relative("/data/downloads/s/other/a.pdf", Some(base)).is_none());
    }

    #[test]
    fn keys_and_cookies() {
        assert!(valid_key("rk-1"));
        assert!(!valid_key("a b"));
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("x=1; zeus_sid=abc"),
        );
        assert_eq!(presented(&h).as_deref(), Some("abc"));
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer tok"),
        );
        assert_eq!(presented(&h).as_deref(), Some("tok"));
    }
}
