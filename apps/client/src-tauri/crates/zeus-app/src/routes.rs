//! The closed route table the page may call. Anything else is refused.
//!
//! Routes mirror `zeus-web-bridge` (`/api/v1`) and the hosted pre-alpha service (`/svc/v1`) so the client has one
//! code path for all three backends; `/native/v1` holds what only the desktop app offers.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::App;
use crate::engine_config::{self, EngineSettings};
use crate::failure::{Failure, Result};
use crate::osopen;
use crate::supervisor::Phase;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The sentence shown while the engine is not answering.
fn not_ready(app: &App) -> Failure {
    let s = app.supervisor.status();
    let code = match s.phase {
        Phase::Starting => "engine.starting",
        Phase::Restarting => "engine.restarting",
        Phase::Failed => "engine.failed",
        Phase::Stopped => "engine.stopped",
        Phase::Ready => "engine.unreachable",
    };
    Failure::new(code, s.message)
}

fn ensure_ready(app: &App) -> Result<()> {
    if app.supervisor.status().phase == Phase::Ready {
        Ok(())
    } else {
        Err(not_ready(app))
    }
}

/// Strip the Windows verbatim prefix so paths read as a person writes them.
pub fn display_path(p: &str) -> String {
    p.strip_prefix(r"\\?\").unwrap_or(p).to_owned()
}

fn clean_paths(v: &mut Value) {
    match v {
        Value::String(s) => {
            if s.starts_with(r"\\?\") {
                *s = display_path(s);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(clean_paths),
        Value::Object(o) => o.values_mut().for_each(clean_paths),
        _ => {}
    }
}

fn retry_key() -> String {
    format!("app-{}", crate::review::random_id())
}

/// Dispatch one route.
pub async fn dispatch(app: &Arc<App>, route: &str, body: Value) -> Result<Value> {
    let (method, path) = route
        .split_once(' ')
        .ok_or_else(|| Failure::invalid("Unknown route."))?;
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let mut out = match (method, parts.as_slice()) {
        ("POST", ["api", "v1", "session"]) => session(app).await,
        ("GET", ["api", "v1", "status"]) => status(app).await,
        ("GET", ["api", "v1", "jobs"]) => jobs(app).await,
        ("GET", ["api", "v1", "jobs", id]) => job(app, id).await,
        ("POST", ["api", "v1", "jobs", id, "control"]) => control(app, id, &body).await,
        ("POST", ["api", "v1", "links", "preview"]) => {
            ensure_ready(app)?;
            let settings = engine_config::load(&app.home)?;
            app.reviews.preview(&app.daemon, &settings, &body).await
        }
        ("POST", ["api", "v1", "links", "enqueue"]) => {
            ensure_ready(app)?;
            app.reviews.enqueue(&app.daemon, &body).await
        }
        ("GET", ["api", "v1", "files"]) => files(app).await,
        ("GET", ["api", "v1", "artifacts"]) => {
            ensure_ready(app)?;
            let items = app.daemon.all("artifacts.list", json!({}), 5).await?;
            Ok(json!({"items": items}))
        }
        ("GET", ["api", "v1", "approvals"]) => {
            ensure_ready(app)?;
            let items = app.daemon.all("approvals.list", json!({}), 3).await?;
            Ok(
                json!({"items": items, "decide": "Decide these in the interactive Zeus CLI (zeus approvals) or the browser extension's popup."}),
            )
        }
        ("GET", ["api", "v1", "runs"]) => {
            ensure_ready(app)?;
            let items = app
                .daemon
                .all("workflows.list", json!({}), 2)
                .await
                .unwrap_or_default();
            Ok(json!({"items": items}))
        }
        ("GET", ["api", "v1", "fixtures"]) => Ok(json!({"hosts": [], "samples": []})),

        ("GET", ["svc", "v1", "state"]) => app.state.all().await,
        ("DELETE", ["svc", "v1", "state"]) => app.state.clear().await,
        ("PUT", ["svc", "v1", "state", slice]) => app.state.put(slice, &body).await,
        ("GET", ["svc", "v1", "index"]) | ("POST", ["svc", "v1", "index", "rescan"]) => {
            index(app).await
        }
        ("POST", ["svc", "v1", "links", "check"]) => {
            ensure_ready(app)?;
            let urls: Vec<String> = body
                .get("urls")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|u| u.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            crate::links::check(app.daemon.clone(), &urls).await
        }
        ("GET", ["svc", "v1", "approvals"]) => app.held.list().await,
        ("POST", ["svc", "v1", "approvals"]) => app.held.create(&body).await,
        ("POST", ["svc", "v1", "approvals", id, "decide"]) => app.held.decide(id, &body).await,

        ("GET", ["native", "v1", "engine"]) => engine(app).await,
        ("POST", ["native", "v1", "engine", "restart"]) => {
            app.supervisor.restart();
            Ok(json!({"ok": true}))
        }
        ("GET", ["native", "v1", "engine", "settings"]) => {
            Ok(serde_json::to_value(engine_config::load(&app.home)?)?)
        }
        ("PUT", ["native", "v1", "engine", "settings"]) => {
            let settings: EngineSettings = serde_json::from_value(body)?;
            engine_config::save(&app.home, &settings)?;
            Ok(json!({"ok": true, "restart_needed": true}))
        }
        ("GET", ["native", "v1", "queues"]) => {
            ensure_ready(app)?;
            app.daemon.raw("queues.list", json!({})).await
        }
        (
            "POST",
            [
                "native",
                "v1",
                "queues",
                op @ ("put" | "delete" | "control" | "configure"),
            ],
        ) => {
            ensure_ready(app)?;
            let mut params = if body.is_object() { body } else { json!({}) };
            if params.get("idempotency_key").is_none() {
                params["idempotency_key"] = json!(retry_key());
            }
            app.daemon.raw(&format!("queues.{op}"), params).await
        }
        ("POST", ["native", "v1", "open"]) => {
            let settings = engine_config::load(&app.home)?;
            let path = body.get("path").and_then(Value::as_str).unwrap_or_default();
            let action = match body.get("action").and_then(Value::as_str) {
                Some("reveal") => osopen::Action::Reveal,
                _ => osopen::Action::Open,
            };
            osopen::open(&app.home, &settings, path, action)?;
            Ok(json!({"ok": true}))
        }
        ("POST", ["native", "v1", "finish-action"]) => {
            match body.get("action").and_then(Value::as_str) {
                Some("close_app") => {
                    app.quit.notify_one();
                    Ok(json!({"ok": true}))
                }
                Some("sleep") => {
                    osopen::sleep()?;
                    Ok(json!({"ok": true}))
                }
                _ => Err(Failure::invalid("Unknown action.")),
            }
        }
        _ => Err(Failure::new(
            "app.unknown_route",
            "This client asked for something this build does not offer.",
        )),
    }?;
    clean_paths(&mut out);
    Ok(out)
}

async fn session(app: &Arc<App>) -> Result<Value> {
    let settings = engine_config::load(&app.home)?;
    let root = settings.write_roots.first().cloned().unwrap_or_default();
    Ok(json!({
        "mode": "native",
        "bridge_version": env!("CARGO_PKG_VERSION"),
        "created_ms": app.started_ms,
        "expires_ms": Value::Null,
        "quota": Value::Null,
        "used": {"files": 0, "bytes": 0, "jobs": 0},
        "fixture_hosts": [],
        "root_label": display_path(&root),
        "roots": settings.write_roots.iter().map(|r| display_path(r)).collect::<Vec<_>>(),
        "sep": std::path::MAIN_SEPARATOR.to_string(),
        "platform": std::env::consts::OS,
        "home": app.home.root.display().to_string(),
        "logs": app.home.logs_dir().display().to_string(),
        "contained": app.home.contained,
    }))
}

async fn status(app: &Arc<App>) -> Result<Value> {
    let engine = app.supervisor.status();
    if engine.phase != Phase::Ready {
        let f = not_ready(app);
        return Ok(
            json!({"daemon": Value::Null, "failure": f, "updated_ms": now_ms(), "engine": engine}),
        );
    }
    match app.daemon.raw("daemon.status", json!({})).await {
        Ok(s) => Ok(json!({
            "daemon": {
                "daemon_version": s.pointer("/daemon/version"),
                "capabilities": app.daemon.capabilities().await,
                "permissions": app.daemon.permissions().await,
                "head_seq": s.get("head_seq"),
                "pending_approvals": s.get("pending_approvals"),
                "kill_switch_engaged": s.get("kill_switch_engaged"),
                "journal_ok": s.get("journal_ok"),
                "uptime_ms": s.get("uptime_ms"),
            },
            "failure": Value::Null,
            "updated_ms": now_ms(),
            "engine": engine,
        })),
        Err(f) => Ok(
            json!({"daemon": Value::Null, "failure": f, "updated_ms": now_ms(), "engine": engine}),
        ),
    }
}

async fn jobs(app: &Arc<App>) -> Result<Value> {
    if app.supervisor.status().phase != Phase::Ready {
        return Ok(json!({"items": [], "updated_ms": now_ms(), "failure": not_ready(app)}));
    }
    match app.daemon.all("jobs.list", json!({}), 20).await {
        Ok(items) => Ok(json!({"items": items, "updated_ms": now_ms(), "failure": Value::Null})),
        Err(f) => Ok(json!({"items": [], "updated_ms": now_ms(), "failure": f})),
    }
}

async fn job(app: &Arc<App>, id: &str) -> Result<Value> {
    ensure_ready(app)?;
    app.daemon.raw("jobs.get", json!({"job_id": id})).await
}

async fn control(app: &Arc<App>, id: &str, body: &Value) -> Result<Value> {
    ensure_ready(app)?;
    let action = body
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !["pause", "resume", "cancel", "retry", "reprioritize"].contains(&action) {
        return Err(Failure::invalid("Unknown action."));
    }
    let key = body
        .get("idempotency_key")
        .and_then(Value::as_str)
        .map_or_else(retry_key, |k| format!("app-{k}").chars().take(64).collect());
    let mut params = json!({"job_id": id, "action": action, "idempotency_key": key});
    if let Some(p) = body.get("priority") {
        params["priority"] = p.clone();
    }
    let r = app.daemon.raw("jobs.control", params).await?;
    Ok(json!({
        "replayed": r.get("replayed").and_then(Value::as_bool).unwrap_or(false),
        "job_id": r.pointer("/job/id"),
        "state": r.pointer("/job/state"),
    }))
}

async fn files(app: &Arc<App>) -> Result<Value> {
    ensure_ready(app)?;
    let settings = engine_config::load(&app.home)?;
    let mut items = Vec::new();
    for root in &settings.write_roots {
        let list = app
            .daemon
            .all(
                "files.list",
                json!({"dir": root, "recursive": true, "include_partial": true}),
                20,
            )
            .await;
        match list {
            Ok(mut v) => {
                // zeusd reports each file relative to its root; the page works with absolute paths.
                for item in &mut v {
                    if let Some(rel) = item.get("path").and_then(Value::as_str).map(str::to_owned) {
                        let abs = std::path::Path::new(root)
                            .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
                        item["relpath"] = json!(rel);
                        item["path"] = json!(display_path(&abs.display().to_string()));
                    }
                }
                items.append(&mut v);
            }
            Err(f) if f.code == "api.not_found" || f.code == "api.invalid_params" => {}
            Err(f) => return Err(f),
        }
    }
    Ok(json!({"items": items}))
}

/// The file list the Files page shows: a plain walk of the download folders. (The persistent index with a watcher
/// replaces this in the `zeus-app-index` module.)
async fn index(app: &Arc<App>) -> Result<Value> {
    let settings = engine_config::load(&app.home)?;
    let mut rows = Vec::new();
    for root in &settings.write_roots {
        walk(std::path::Path::new(root), 0, &mut rows);
    }
    Ok(json!({"generation": 1, "last_scan": now_ms(), "files": rows}))
}

fn walk(dir: &std::path::Path, depth: usize, rows: &mut Vec<Value>) {
    if depth > 8 || rows.len() > 50_000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64);
        let p = display_path(&path.display().to_string());
        if meta.is_dir() {
            rows.push(json!({"path": p, "size": 0, "mtime": mtime, "is_dir": true}));
            walk(&path, depth + 1, rows);
        } else if meta.is_file() {
            rows.push(json!({"path": p, "size": meta.len(), "mtime": mtime, "is_dir": false}));
        }
    }
}

async fn engine(app: &Arc<App>) -> Result<Value> {
    let settings = engine_config::load(&app.home)?;
    Ok(json!({
        "status": app.supervisor.status(),
        "home": app.home.root.display().to_string(),
        "logs": app.home.logs_dir().display().to_string(),
        "state": app.home.state_dir().display().to_string(),
        "config": app.home.config_dir().join("config.toml").display().to_string(),
        "bin": app.supervisor.bin_dir().display().to_string(),
        "env": app.home.zeus_env().into_iter().collect::<std::collections::BTreeMap<_, _>>(),
        "settings": settings,
        "app_version": env!("CARGO_PKG_VERSION"),
    }))
}
