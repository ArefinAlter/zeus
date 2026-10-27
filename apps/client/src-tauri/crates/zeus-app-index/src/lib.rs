//! The Zeus app's file index (scan, watcher, search data, preview, file operations).
//!
//! The index covers the download folders and any folders the person adds, kept current by the file system watcher and
//! saved to a snapshot so the next start needs no scan. The page searches it with the Everything-style syntax in its own
//! code (the entries are small and the page already holds them); this crate keeps the entries true and serves the
//! preview of one file. File operations here are the person's clicks, inside download and explicitly managed folders, and they never
//! touch a file the daemon quarantined.

#![forbid(unsafe_code)]

pub mod browse;
pub mod config;
pub mod facts;
pub mod ops;
pub mod paths;
pub mod preview;
pub mod scan;
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

use crate::config::{IndexConfig, norm};
use crate::service::{Cmd, Service};

/// The module registered by `zeus-app-ext`.
pub struct Module {
    svc: Arc<Service>,
    transfers: transfers::Transfers,
}

impl Module {
    /// Build the module (inside a Tokio runtime) for `app`; the scan starts at once in the background.
    pub fn new(app: &Arc<App>) -> Arc<Self> {
        Arc::new(Self {
            svc: Service::start(app),
            transfers: transfers::Transfers::default(),
        })
    }
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
    let resolved = paths::resolve_inside(&path, &svc.read_roots())?;
    let shown = paths::shown(&resolved);
    let table = svc.facts.table(app).await;
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
    let mut out = tokio::task::spawn_blocking(move || preview::build(&p, &s))
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
    svc.facts.table(app).await.ok_or_else(|| {
        Failure::new(
            "engine.unreachable",
            "The Zeus engine must be running to check what is quarantined before changing a file.",
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
        if let Some(t) = svc.facts.table(app).await
            && facts::quarantined_under(&t, &shown)
        {
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

async fn rename_route(svc: &Arc<Service>, app: &Arc<App>, body: &Value) -> Result<Value> {
    let facts = guarded_facts(svc, app).await?;
    let r = ops::rename(
        str_of(body, "path"),
        str_of(body, "name"),
        &svc.write_roots(),
        &facts,
    )?;
    touched(
        svc,
        &[
            r["from"].as_str().unwrap_or_default(),
            r["path"].as_str().unwrap_or_default(),
        ],
    );
    Ok(r)
}

async fn move_route(svc: &Arc<Service>, app: &Arc<App>, body: &Value) -> Result<Value> {
    let facts = guarded_facts(svc, app).await?;
    let r = ops::move_to(
        str_of(body, "path"),
        str_of(body, "dir"),
        &svc.write_roots(),
        &facts,
    )?;
    touched(
        svc,
        &[
            r["from"].as_str().unwrap_or_default(),
            r["path"].as_str().unwrap_or_default(),
        ],
    );
    Ok(r)
}

async fn delete_route(svc: &Arc<Service>, app: &Arc<App>, body: &Value) -> Result<Value> {
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
    let r = ops::delete(&list, permanent, &svc.write_roots(), &facts)?;
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
                    let since = body.get("since").and_then(Value::as_u64).unwrap_or(0);
                    let delta = svc.index.lock().map(|i| i.delta(since)).ok();
                    Some(match delta {
                        Some(d) => serde_json::to_value(d)
                            .map(|v| with_status(svc, v))
                            .map_err(Failure::from),
                        None => Err(Failure::new("app.io", "The index is busy.")),
                    })
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
                    let roots = svc.read_roots();
                    let limit = body
                        .get("limit")
                        .and_then(Value::as_u64)
                        .unwrap_or(500)
                        .min(500) as usize;
                    let facts = svc.facts.table(app).await;
                    Some(
                        tokio::task::spawn_blocking(move || {
                            let mut page = browse::list(&path, &after, limit, &roots)?;
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
                "GET /native/v1/files/copies" => Some(Ok(self.transfers.list())),
                "POST /native/v1/files/copy/start" => {
                    let facts = match guarded_facts(svc, app).await {
                        Ok(f) => f,
                        Err(e) => return Some(Err(e)),
                    };
                    Some(self.transfers.start(
                        str_of(body, "path").to_owned(),
                        str_of(body, "dir").to_owned(),
                        svc.read_roots(),
                        svc.write_roots(),
                        facts,
                    ))
                }
                "POST /native/v1/files/copy/status" | "POST /native/v1/files/copy/cancel" => Some(
                    self.transfers
                        .get(str_of(body, "id"), route.ends_with("/cancel")),
                ),
                "POST /native/v1/files/copy" => {
                    let from = str_of(body, "path").to_owned();
                    let to = str_of(body, "dir").to_owned();
                    let reads = svc.read_roots();
                    let writes = svc.write_roots();
                    let facts = match guarded_facts(svc, app).await {
                        Ok(f) => f,
                        Err(e) => return Some(Err(e)),
                    };
                    let result = tokio::task::spawn_blocking(move || {
                        ops::copy_to(&from, &to, &reads, &writes, &facts)
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
                    let roots = svc.write_roots();
                    let result = tokio::task::spawn_blocking(move || {
                        ops::create_folder(&parent, &name, &roots)
                    })
                    .await;
                    svc.rescan(None);
                    Some(result.unwrap_or_else(|_| {
                        Err(Failure::new("app.io", "The folder task stopped."))
                    }))
                }
                "POST /native/v1/files/preview" => Some(preview_route(svc, app, body).await),
                "POST /native/v1/files/open" => Some(open_route(svc, app, body).await),
                "POST /native/v1/files/rename" => Some(rename_route(svc, app, body).await),
                "POST /native/v1/files/move" => Some(move_route(svc, app, body).await),
                "POST /native/v1/files/delete" => Some(delete_route(svc, app, body).await),
                _ => None,
            }
        })
    }

    fn shutdown<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.transfers.cancel_all();
            let (tx, rx) = oneshot::channel();
            self.svc.send(Cmd::Stop(tx));
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx).await;
        })
    }
}
