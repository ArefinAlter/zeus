//! The Zeus app's file index (scan, watcher, search data, preview, file operations).
//!
//! The index covers the download folders and any folders the person adds, kept current by the file system watcher and
//! saved to a snapshot and reconciled on restart. Native search executes bounded parsed queries in Rust
//! and returns generation-bound pages. Optional Everything SDK2 queries are explicit, scoped live pages.
//! This crate also serves previews, size/duplicate inspection and durable native operation receipts. File operations here are the person's clicks, inside download and explicitly managed folders, and they never
//! touch a file the daemon quarantined.

#![forbid(unsafe_code)]

pub mod analytics;
pub mod archive;
pub mod browse;
pub mod compare;
pub mod config;
pub mod everything;
pub mod facts;
pub mod history;
pub mod ops;
pub mod paths;
pub mod preview;
mod read;
pub mod scan;
pub mod search;
pub mod service;
pub mod store;
pub mod transfers;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::oneshot;
use zeus_app::App;
use zeus_app::ext::{Extension, Reply};
use zeus_app::failure::{Failure, Result};

use crate::config::{Excluder, IndexConfig, norm};
use crate::service::{Cmd, Service};

/// The module registered by `zeus-app-ext`.
pub struct Module {
    svc: Arc<Service>,
    transfers: Arc<transfers::Transfers>,
    comparisons: compare::Comparisons,
    history: Arc<history::Journal>,
    archives: Arc<archive::Archives>,
    everything: Arc<everything::Everything>,
}

impl Module {
    /// Build the module (inside a Tokio runtime) for `app`; the scan starts at once in the background.
    pub fn new(app: &Arc<App>) -> Arc<Self> {
        Arc::new(Self {
            svc: Service::start(app),
            comparisons: compare::Comparisons::default(),
            transfers: Arc::new(transfers::Transfers::new(
                config::index_dir(&app.home).join("copies"),
            )),
            everything: Arc::new(everything::Everything::new(
                config::index_dir(&app.home).join("everything"),
            )),
            archives: Arc::new(archive::Archives::new(
                config::index_dir(&app.home).join("archives"),
            )),
            history: Arc::new(history::Journal::new(
                config::index_dir(&app.home).join("operations.json"),
            )),
        })
    }
}

async fn copy_authority(
    svc: &Arc<Service>,
    app: &Arc<App>,
    sources: Vec<String>,
    writable: bool,
) -> zeus_app::failure::Result<transfers::Authority> {
    let revision = svc.scope_generation();
    let facts = guarded_facts(svc, app).await?;
    let reads = svc.file_capabilities(false)?;
    let writes = if writable {
        svc.file_capabilities(true)?
    } else {
        paths::RootCaps::capture(&[])?
    };
    if svc.scope_generation() != revision {
        return Err(Failure::new(
            "app.path_changed",
            "Folder settings changed while acquiring file authority. Try again.",
        ));
    }
    let service = svc.clone();
    let app = app.clone();
    let runtime = tokio::runtime::Handle::current();
    let last = std::sync::Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(4));
    let check = move || -> zeus_app::failure::Result<()> {
        if service.scope_generation() != revision {
            return Err(Failure::new(
                "app.path_changed",
                "Folder settings changed. This copy is paused until an explicit Resume with current scopes.",
            ));
        }
        let mut last = last
            .lock()
            .map_err(|_| Failure::new("app.io", "Copy authorization is unavailable."))?;
        if last.elapsed() >= std::time::Duration::from_secs(3) {
            service.file_capabilities(false)?;
            if writable {
                service.file_capabilities(true)?;
            }
            let facts = runtime.block_on(service.facts.table(&app)).ok_or_else(||Failure::new("app.offline","The engine's quarantine status is unavailable. Copy checkpoints are preserved."))?;
            if sources
                .iter()
                .any(|source| facts::quarantined_under(&facts, source))
            {
                return Err(ops::quarantine_refusal("This source"));
            }
            *last = std::time::Instant::now();
        }
        Ok(())
    };
    Ok(transfers::Authority {
        reads: reads.workspace_caps()?,
        writes: writes.workspace_caps()?,
        path_check: Arc::new(move |path| {
            if facts::quarantined_under(&facts, path) {
                Err(transfers::to_shared(ops::quarantine_refusal("This item")))
            } else {
                Ok(())
            }
        }),
        check: Arc::new(move || check().map_err(transfers::to_shared)),
    })
}

fn str_of<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn with_status(svc: &Service, mut v: Value) -> Value {
    if let (Some(o), Some(s)) = (v.as_object_mut(), svc.status().as_object()) {
        for (k, val) in s {
            o.entry(k.clone()).or_insert_with(|| val.clone());
        }
    }
    v
}

fn config_json(cfg: &IndexConfig) -> Value {
    json!({
        "roots": cfg.roots,
        "exclusions": cfg.exclusions,
        "exclude_hidden": cfg.exclude_hidden,
        "exclude_system": cfg.exclude_system,
    })
}

async fn preview_route(svc: &Arc<Service>, app: &Arc<App>, body: &Value) -> Result<Value> {
    let path = str_of(body, "path").to_owned();
    let roots = svc.file_capabilities(false)?;
    let resolved = roots.path(&path)?;
    let shown = paths::shown(&resolved);
    let table = Some(guarded_facts(svc, app).await?);
    let facts = table.as_ref().and_then(|t| t.get(&norm(&shown)).cloned());
    if table
        .as_ref()
        .is_some_and(|t| facts::quarantined_under(t, &shown))
    {
        return Ok(json!({
            "kind": "refused", "reason": "quarantined", "path": shown,
            "name": resolved.file_name().map(|n| n.to_string_lossy().into_owned()),
            "message": "This file is quarantined. Zeus never opens, previews or runs a quarantined file.",
        }));
    }
    let p = resolved.clone();
    let s = shown.clone();
    let mut out = tokio::task::spawn_blocking(move || preview::build_pinned(&p, &s, &roots))
        .await
        .map_err(|_| Failure::new("app.io", "The preview failed."))?;
    if out["kind"] == "folder" {
        let (entries, bytes) = svc.index.lock().map(|i| i.under(&shown)).unwrap_or((0, 0));
        out["entries"] = json!(entries);
        out["bytes"] = json!(bytes);
    }
    if let Some(f) = &facts {
        out["quarantined"] = json!(f.quarantined);
        out["job_id"] = json!(f.job_id);
        out["sha256"] = json!(f.sha256);
        if let Some(job) = &f.job_id {
            out["blake3"] = json!(facts::blake3_of(app, job).await);
        }
    }
    Ok(out)
}

async fn guarded_facts(
    svc: &Arc<Service>,
    app: &Arc<App>,
) -> Result<Arc<std::collections::HashMap<String, facts::FileFacts>>> {
    svc.facts.clear();
    svc.facts.table(app).await.ok_or_else(|| {
        Failure::new(
            "app.quarantine_unverified",
            "Zeus could not verify the complete quarantine history. Check that the engine is running; histories above 3,000 jobs need the reviewed file-organize workflow.",
        )
    })
}

fn touched(svc: &Service, paths: &[&str]) {
    svc.send(Cmd::Touch(paths.iter().map(PathBuf::from).collect()));
}

async fn open_route(svc: &Arc<Service>, app: &Arc<App>, body: &Value) -> Result<Value> {
    let path = str_of(body, "path");
    let reveal = str_of(body, "action") == "reveal";
    let resolved = paths::resolve_inside(path, &svc.read_roots())?;
    let shown = paths::shown(&resolved);
    if !reveal && resolved.is_file() {
        if zeus_app::osopen::runnable(&shown) {
            return Err(Failure::new(
                "app.runnable",
                "Zeus does not open programs and scripts. Use Show in folder, and decide there.",
            ));
        }
        let table = guarded_facts(svc, app).await?;
        if facts::quarantined_under(&table, &shown) {
            return Err(Failure::new(
                "app.quarantined",
                "Quarantined: Zeus never opens it. Show it in its folder instead.",
            ));
        }
    }
    launch(&resolved, reveal)?;
    Ok(json!({"ok": true}))
}

#[cfg(windows)]
fn launch(p: &std::path::Path, reveal: bool) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("explorer.exe");
    if reveal && p.is_file() {
        cmd.raw_arg(format!("/select,\"{}\"", scan::display(p)));
    } else {
        cmd.raw_arg(format!("\"{}\"", scan::display(p)));
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|_| Failure::new("app.open", "The file manager could not be started."))
}

#[cfg(not(windows))]
fn launch(p: &std::path::Path, reveal: bool) -> Result<()> {
    let target = if reveal && p.is_file() {
        p.parent().unwrap_or(p)
    } else {
        p
    };
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|_| Failure::new("app.open", "No file manager could be started."))
}

async fn relocate_route(
    module: &Module,
    app: &Arc<App>,
    body: &Value,
    rename: bool,
) -> Result<Value> {
    let svc = &module.svc;
    let facts = guarded_facts(svc, app).await?;
    let roots = svc.file_capabilities(true)?;
    let from = roots.path(str_of(body, "path"))?;
    let to = if rename {
        let name = str_of(body, "name");
        paths::check_name(name)?;
        from.parent()
            .ok_or_else(|| Failure::invalid("That path has no parent."))?
            .join(name)
    } else {
        let dest = roots.path(str_of(body, "dir"))?;
        roots.directory(&dest)?;
        dest.join(
            from.file_name()
                .ok_or_else(|| Failure::invalid("That path has no name."))?,
        )
    };
    let journal = module.history.clone();
    let result = tokio::task::spawn_blocking(move || {
        journal.relocate_pinned(&paths::shown(&from), &paths::shown(&to), &roots, &facts)
    })
    .await
    .map_err(|_| Failure::new("app.io", "The operation task stopped; review its receipt."))??;
    touched(
        svc,
        &[
            result["from"].as_str().unwrap_or_default(),
            result["path"].as_str().unwrap_or_default(),
        ],
    );
    Ok(result)
}

async fn delete_route(
    module: &Module,
    svc: &Arc<Service>,
    app: &Arc<App>,
    body: &Value,
) -> Result<Value> {
    let facts = guarded_facts(svc, app).await?;
    let list: Vec<String> = body
        .get("paths")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let permanent = body
        .get("permanent")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let roots = svc.file_capabilities(true)?;
    let journal = module.history.clone();
    let r = tokio::task::spawn_blocking(move || {
        ops::delete_pinned(&list, permanent, &roots, &facts, Some(&journal))
    })
    .await
    .map_err(|_| {
        Failure::new(
            "app.io",
            "The deletion task stopped; inspect operation history.",
        )
    })??;
    let gone: Vec<&str> = r["trashed"]
        .as_array()
        .into_iter()
        .chain(r["deleted"].as_array())
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    touched(svc, &gone);
    Ok(r)
}

impl Extension for Module {
    fn call<'a>(
        &'a self,
        app: &'a Arc<App>,
        route: &'a str,
        body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        Box::pin(async move {
            let svc = &self.svc;
            match route {
                "GET /svc/v1/index" => {
                    let (files, generation, last_scan) = svc
                        .index
                        .lock()
                        .map(|i| (i.all(), i.generation, i.last_scan))
                        .unwrap_or_default();
                    Some(Ok(
                        json!({"generation": generation, "last_scan": last_scan, "files": files}),
                    ))
                }
                "POST /svc/v1/index/rescan" => {
                    let root = body.get("path").and_then(Value::as_str).map(str::to_owned);
                    svc.rescan(root);
                    Some(Ok(
                        json!({"generation": 0, "last_scan": null, "files": [], "status": svc.status()}),
                    ))
                }
                "POST /native/v1/index/changes" => {
                    if body.get("summary_only").and_then(Value::as_bool) == Some(true) {
                        let generation = svc.index.lock().map(|i| i.generation).unwrap_or(0);
                        return Some(Ok(with_status(
                            svc,
                            json!({"generation":generation,"reset":false,"files":[],"upserts":[],"removes":[]}),
                        )));
                    }
                    let since = body.get("since").and_then(Value::as_u64).unwrap_or(0);
                    let delta = svc.index.lock().map(|i| i.delta(since)).ok();
                    Some(match delta {
                        Some(d) => serde_json::to_value(d)
                            .map(|v| with_status(svc, v))
                            .map_err(Failure::from),
                        None => Err(Failure::new("app.io", "The index is busy.")),
                    })
                }
                "POST /native/v1/index/search" => {
                    let request = match serde_json::from_value::<search::Request>(body.clone()) {
                        Ok(r) => r,
                        Err(_) => {
                            return Some(Err(Failure::invalid("Invalid native search request.")));
                        }
                    };
                    let roots = svc.read_roots();
                    let index = svc.index.clone();
                    Some(
                        tokio::task::spawn_blocking(move || {
                            let index = index
                                .lock()
                                .map_err(|_| Failure::new("app.io", "The index is busy."))?;
                            serde_json::to_value(search::query(&index, &request, &roots)?)
                                .map_err(Failure::from)
                        })
                        .await
                        .unwrap_or_else(|_| {
                            Err(Failure::new("app.io", "The search task stopped."))
                        }),
                    )
                }
                "GET /native/v1/index/analytics" => {
                    let roots = svc.read_roots();
                    let index = svc.index.clone();
                    Some(
                        tokio::task::spawn_blocking(move || {
                            let index = index
                                .lock()
                                .map_err(|_| Failure::new("app.io", "The index is busy."))?;
                            Ok(analytics::summarize(&index, &roots))
                        })
                        .await
                        .unwrap_or_else(|_| {
                            Err(Failure::new("app.io", "The size summary task stopped."))
                        }),
                    )
                }
                "POST /native/v1/files/duplicates/verify" => {
                    let paths: Vec<String> = body
                        .get("paths")
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default();
                    let facts = match guarded_facts(svc, app).await {
                        Ok(f) => f,
                        Err(e) => return Some(Err(e)),
                    };
                    let roots = match svc.file_capabilities(false) {
                        Ok(roots) => roots,
                        Err(error) => return Some(Err(error)),
                    };
                    Some(
                        tokio::task::spawn_blocking(move || {
                            analytics::verify_duplicates_pinned(&paths, &roots, &facts)
                        })
                        .await
                        .unwrap_or_else(|_| {
                            Err(Failure::new("app.io", "Duplicate verification stopped."))
                        }),
                    )
                }
                "GET /native/v1/files/archives" => Some(Ok(self.archives.list())),
                "POST /native/v1/files/archive/prepare" => {
                    let request = match serde_json::from_value::<archive::Prepare>(body.clone()) {
                        Ok(r) => r,
                        Err(_) => {
                            return Some(Err(Failure::invalid("Invalid archive preview request.")));
                        }
                    };
                    let facts = match guarded_facts(svc, app).await {
                        Ok(f) => f,
                        Err(e) => return Some(Err(e)),
                    };
                    let reads = match svc.file_capabilities(false) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let writes = match svc.file_capabilities(true) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let manager = self.archives.clone();
                    Some(
                        tokio::task::spawn_blocking(move || {
                            manager.prepare_pinned(&request, &reads, &writes, &facts)
                        })
                        .await
                        .unwrap_or_else(|_| {
                            Err(Failure::new(
                                "app.archive",
                                "The archive preview task stopped.",
                            ))
                        }),
                    )
                }
                "POST /native/v1/files/archive/start" => {
                    let facts = match guarded_facts(svc, app).await {
                        Ok(f) => f,
                        Err(e) => return Some(Err(e)),
                    };
                    let reads = match svc.file_capabilities(false) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let writes = match svc.file_capabilities(true) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    Some(
                        self.archives
                            .start_pinned(str_of(body, "id"), reads, writes, facts),
                    )
                }
                "POST /native/v1/files/archive/reconcile" => {
                    let id = str_of(body, "id").to_owned();
                    let writes = match svc.file_capabilities(true) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let manager = self.archives.clone();
                    let facts = match guarded_facts(svc, app).await {
                        Ok(f) => f,
                        Err(e) => return Some(Err(e)),
                    };
                    Some(
                        tokio::task::spawn_blocking(move || {
                            manager.reconcile_pinned(&id, &writes, &facts)
                        })
                        .await
                        .unwrap_or_else(|_| {
                            Err(Failure::new(
                                "app.archive",
                                "Archive reconciliation stopped; inspect its receipt.",
                            ))
                        }),
                    )
                }
                "POST /native/v1/files/archive/cancel" => {
                    Some(self.archives.cancel(str_of(body, "id")))
                }
                "POST /native/v1/files/archive/dismiss" => {
                    Some(self.archives.dismiss(str_of(body, "id")))
                }
                "POST /native/v1/files/archive/cleanup" => {
                    let id = str_of(body, "id").to_owned();
                    let writes = match svc.file_capabilities(true) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let manager = self.archives.clone();
                    Some(
                        tokio::task::spawn_blocking(move || manager.cleanup_pinned(&id, &writes))
                            .await
                            .unwrap_or_else(|_| {
                                Err(Failure::new(
                                    "app.archive",
                                    "The archive cleanup task stopped.",
                                ))
                            }),
                    )
                }
                "GET /native/v1/files/history" => Some(Ok(self.history.list())),
                "POST /native/v1/files/history/undo"
                | "POST /native/v1/files/history/reconcile" => {
                    let id = str_of(body, "id").to_owned();
                    let roots = match svc.file_capabilities(true) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let journal = self.history.clone();
                    let undo = route.ends_with("/undo");
                    let facts = match guarded_facts(svc, app).await {
                        Ok(f) => f,
                        Err(e) => return Some(Err(e)),
                    };
                    let result = tokio::task::spawn_blocking(move || {
                        if undo {
                            journal.undo_pinned(&id, &roots, &facts)
                        } else {
                            journal.reconcile_pinned(&id, &roots)
                        }
                    })
                    .await
                    .unwrap_or_else(|_| {
                        Err(Failure::new(
                            "app.io",
                            "The operation task stopped; inspect its receipt.",
                        ))
                    });
                    svc.rescan(None);
                    Some(result)
                }
                "GET /native/v1/index/everything/config" => Some(Ok(self.everything.config())),
                "POST /native/v1/index/everything/inspect" => {
                    let path = str_of(body, "path").to_owned();
                    Some(
                        tokio::task::spawn_blocking(move || everything::inspect(&path))
                            .await
                            .unwrap_or_else(|_| {
                                Err(Failure::new("app.io", "Helper inspection stopped."))
                            }),
                    )
                }
                "PUT /native/v1/index/everything/config" => {
                    let settings =
                        match serde_json::from_value::<everything::Settings>(body.clone()) {
                            Ok(s) => s,
                            Err(_) => {
                                return Some(Err(Failure::invalid(
                                    "Invalid Everything integration settings.",
                                )));
                            }
                        };
                    let provider = self.everything.clone();
                    Some(
                        tokio::task::spawn_blocking(move || provider.configure(settings))
                            .await
                            .unwrap_or_else(|_| {
                                Err(Failure::new("app.io", "The helper settings task stopped."))
                            }),
                    )
                }
                "POST /native/v1/index/everything/search" => {
                    let request = match serde_json::from_value::<everything::Request>(body.clone())
                    {
                        Ok(r) => r,
                        Err(_) => {
                            return Some(Err(Failure::invalid(
                                "Invalid Everything search request.",
                            )));
                        }
                    };
                    let reads = match svc.file_capabilities(false) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let exclusions =
                        Excluder::new(&svc.cfg.lock().map(|c| c.clone()).unwrap_or_default());
                    let result = self
                        .everything
                        .query_pinned(&request, &reads, &exclusions)
                        .await;
                    // Native settings can revoke a read scope while the helper is running.
                    if svc
                        .file_capabilities(false)
                        .and_then(|roots| roots.directory(std::path::Path::new(&request.root)))
                        .is_err()
                    {
                        return Some(Err(Failure::new(
                            "app.path_outside",
                            "This read scope was revoked during the query.",
                        )));
                    }
                    Some(result.and_then(|v| serde_json::to_value(v).map_err(Failure::from)))
                }
                "GET /native/v1/index/status" => Some(Ok(svc.status())),
                "GET /native/v1/index/config" => Some(Ok(svc
                    .cfg
                    .lock()
                    .map(|c| config_json(&c))
                    .unwrap_or_default())),
                "PUT /native/v1/index/config" => Some(
                    serde_json::from_value::<IndexConfig>(body.clone())
                        .map_err(Failure::from)
                        .and_then(|c| svc.set_config(c))
                        .map(|c| config_json(&c)),
                ),
                "POST /native/v1/files/list" => {
                    let path = str_of(body, "path").to_owned();
                    let after = str_of(body, "after").to_owned();
                    let roots = match svc.file_capabilities(false) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let limit = body
                        .get("limit")
                        .and_then(Value::as_u64)
                        .unwrap_or(500)
                        .min(500) as usize;
                    let facts = svc.facts.table(app).await;
                    let directories_only = body
                        .get("directories_only")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    Some(
                        tokio::task::spawn_blocking(move || {
                            let mut page = browse::list_filtered_pinned(
                                &path,
                                &after,
                                limit,
                                &roots,
                                directories_only,
                            )?;
                            if let Some(rows) = page["files"].as_array_mut() {
                                for row in rows {
                                    let quarantined = facts.as_ref().is_some_and(|t| {
                                        facts::quarantined_under(
                                            t,
                                            row["path"].as_str().unwrap_or_default(),
                                        )
                                    });
                                    row["quarantined"] = json!(quarantined);
                                }
                            }
                            Ok(page)
                        })
                        .await
                        .unwrap_or_else(|_| {
                            Err(Failure::new("app.io", "The folder could not be read."))
                        }),
                    )
                }
                "POST /native/v1/files/compare/start" => {
                    let request = match serde_json::from_value::<compare::Request>(body.clone()) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e.into())),
                    };
                    let authority = match copy_authority(
                        svc,
                        app,
                        vec![request.left.clone(), request.right.clone()],
                        false,
                    )
                    .await
                    {
                        Ok(a) => a,
                        Err(e) => return Some(Err(e)),
                    };
                    Some(self.comparisons.start(request, authority))
                }
                "POST /native/v1/files/compare/status" | "POST /native/v1/files/compare/cancel" => {
                    Some(
                        self.comparisons
                            .status(str_of(body, "id"), route.ends_with("/cancel")),
                    )
                }
                "POST /native/v1/files/compare/page" => Some(self.comparisons.page(
                    str_of(body, "id"),
                    body["offset"].as_u64().unwrap_or(0).min(usize::MAX as u64) as usize,
                    body["limit"].as_u64().unwrap_or(200).min(500) as usize,
                )),
                "GET /native/v1/files/copies" => Some(Ok(self.transfers.list())),
                "POST /native/v1/files/copy/start" | "POST /native/v1/files/copy/resume" => {
                    let id = str_of(body, "id").to_owned();
                    let existing = if route.ends_with("/resume") {
                        match self.transfers.get(&id, false) {
                            Ok(v) => Some(v),
                            Err(e) => return Some(Err(e)),
                        }
                    } else {
                        None
                    };
                    let from = existing
                        .as_ref()
                        .and_then(|v| v["source"].as_str())
                        .unwrap_or_else(|| str_of(body, "path"))
                        .to_owned();
                    let to = str_of(body, "dir").to_owned();
                    let authority = match copy_authority(svc, app, vec![from.clone()], true).await {
                        Ok(a) => a,
                        Err(e) => return Some(Err(e)),
                    };
                    let transfers = self.transfers.clone();
                    Some(
                        tokio::task::spawn_blocking(move || {
                            if existing.is_some() {
                                transfers.resume(&id, authority)
                            } else {
                                transfers.start(from, to, authority)
                            }
                        })
                        .await
                        .unwrap_or_else(|_| {
                            Err(Failure::new(
                                "app.io",
                                "The copy worker stopped before admission.",
                            ))
                        }),
                    )
                }
                "POST /native/v1/files/copy/status" | "POST /native/v1/files/copy/cancel" => Some(
                    self.transfers
                        .get(str_of(body, "id"), route.ends_with("/cancel")),
                ),
                "POST /native/v1/files/copy" => {
                    let from = str_of(body, "path").to_owned();
                    let to = str_of(body, "dir").to_owned();
                    let reads = match svc.file_capabilities(false) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let writes = match svc.file_capabilities(true) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let facts = match guarded_facts(svc, app).await {
                        Ok(f) => f,
                        Err(e) => return Some(Err(e)),
                    };
                    let result = tokio::task::spawn_blocking(move || {
                        ops::copy_to_pinned(&from, &to, &reads, &writes, &facts)
                    })
                    .await;
                    svc.rescan(None);
                    Some(
                        result.unwrap_or_else(|_| {
                            Err(Failure::new("app.io", "The copy task stopped."))
                        }),
                    )
                }
                "POST /native/v1/files/mkdir" => {
                    let parent = str_of(body, "parent").to_owned();
                    let name = str_of(body, "name").to_owned();
                    let roots = match svc.file_capabilities(true) {
                        Ok(r) => r,
                        Err(e) => return Some(Err(e)),
                    };
                    let result = tokio::task::spawn_blocking(move || {
                        ops::create_folder_pinned(&parent, &name, &roots)
                    })
                    .await;
                    svc.rescan(None);
                    Some(result.unwrap_or_else(|_| {
                        Err(Failure::new("app.io", "The folder task stopped."))
                    }))
                }
                "POST /native/v1/files/preview" => Some(preview_route(svc, app, body).await),
                "POST /native/v1/files/open" => Some(open_route(svc, app, body).await),
                "POST /native/v1/files/rename" => Some(relocate_route(self, app, body, true).await),
                "POST /native/v1/files/move" => Some(relocate_route(self, app, body, false).await),
                "POST /native/v1/files/delete" => Some(delete_route(self, svc, app, body).await),
                _ => None,
            }
        })
    }

    fn shutdown<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.transfers.cancel_all();
            self.comparisons.cancel_all();
            self.archives.cancel_all();
            let (tx, rx) = oneshot::channel();
            self.svc.send(Cmd::Stop(tx));
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx).await;
        })
    }
}
