//! The index service: one task owns scanning, the watcher and the snapshot; the routes talk to it through a channel and
//! read the shared index under its lock.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use zeus_app::home::AppHome;

use crate::config::{Excluder, IndexConfig, RootCfg, index_dir, norm};
use crate::facts::FactsCache;
use crate::scan::{self, Progress, display};
use crate::store::Index;

/// Commands the task handles.
pub enum Cmd {
    /// Rescan every included folder.
    RescanAll,
    /// Rescan one folder.
    RescanRoot(String),
    /// The configuration or the download folders changed.
    Reconfigure,
    /// The watcher saw these paths change.
    Fs(Vec<PathBuf>),
    /// A file operation changed these paths: refresh them now.
    Touch(Vec<PathBuf>),
    /// Write the snapshot, then answer.
    Stop(oneshot::Sender<()>),
}

#[derive(Debug, Default, Clone)]
struct StatusInner {
    scanning: bool,
    scanning_root: String,
    roots_total: usize,
    roots_done: usize,
    verifying: bool,
    watcher: bool,
}

/// The shared state of the service.
pub struct Service {
    home: AppHome,
    /// The configuration, with the download folders merged in.
    pub cfg: Mutex<IndexConfig>,
    /// The index itself.
    pub index: Arc<Mutex<Index>>,
    status: Mutex<StatusInner>,
    progress: Progress,
    tx: mpsc::UnboundedSender<Cmd>,
    cancel: AtomicBool,
    scan_seq: AtomicU32,
    dirty: AtomicBool,
    /// What zeusd knows about files.
    pub facts: FactsCache,
    root_scans: Mutex<HashMap<String, (Instant, i64)>>,
    /// The folders the watcher is set on right now.
    watched: Mutex<Vec<String>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl Service {
    /// Create the service and start its task. Inside a Tokio runtime.
    pub fn start(app: &Arc<zeus_app::App>) -> Arc<Self> {
        let home = app.home.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        let download = download_roots(&home);
        let cfg =
            IndexConfig::load(&index_dir(&home).join("index.json")).with_download_roots(&download);
        let roots: Vec<_> = cfg
            .roots
            .iter()
            .filter(|r| r.include)
            .map(|r| r.path.clone())
            .collect();
        if let Ok(bytes) = serde_json::to_vec(&roots) {
            let scopes = home.state_dir().join("cloud-local-roots.json");
            if zeus_app::engine_config::write_atomic(&scopes, &bytes).is_err() {
                let _ = std::fs::remove_file(&scopes);
            }
        }
        let loaded = Index::load(&index_dir(&home).join("index.bin"));
        let had_snapshot = loaded.is_some();
        let svc = Arc::new(Self {
            home,
            cfg: Mutex::new(cfg),
            index: Arc::new(Mutex::new(loaded.unwrap_or_default())),
            status: Mutex::new(StatusInner {
                verifying: had_snapshot,
                ..StatusInner::default()
            }),
            progress: Progress::default(),
            tx,
            cancel: AtomicBool::new(false),
            scan_seq: AtomicU32::new(1),
            dirty: AtomicBool::new(false),
            facts: FactsCache::default(),
            root_scans: Mutex::new(HashMap::new()),
            watched: Mutex::new(Vec::new()),
        });
        let task = svc.clone();
        tokio::spawn(async move { task.run(rx).await });
        svc
    }

    /// Ask the task to do something.
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// Ask for a rescan and stop a scan in progress from blocking it.
    pub fn rescan(&self, root: Option<String>) {
        self.cancel.store(true, Ordering::SeqCst);
        self.send(match root {
            Some(r) => Cmd::RescanRoot(r),
            None => Cmd::RescanAll,
        });
    }

    /// The folders that are indexed (included ones).
    pub fn included(&self) -> Vec<RootCfg> {
        self.cfg
            .lock()
            .map(|c| c.roots.iter().filter(|r| r.include).cloned().collect())
            .unwrap_or_default()
    }

    /// Download folders plus the folders explicitly enabled for management.
    pub fn write_roots(&self) -> Vec<String> {
        let mut roots = download_roots(&self.home);
        roots.extend(
            self.included()
                .into_iter()
                .filter(|r| r.manage_files)
                .map(|r| r.path),
        );
        roots
    }

    /// Folders the app may read for previews: every included folder.
    pub fn read_roots(&self) -> Vec<String> {
        self.included().into_iter().map(|r| r.path).collect()
    }

    /// Replace the configuration (from the page), save it, and apply it.
    pub fn set_config(&self, new: IndexConfig) -> zeus_app::failure::Result<IndexConfig> {
        new.validate()?;
        let merged = new.with_download_roots(&download_roots(&self.home));
        merged.save(&index_dir(&self.home).join("index.json"))?;
        // Only this native human settings surface writes the upload read scopes.
        let roots: Vec<_> = merged
            .roots
            .iter()
            .filter(|r| r.include)
            .map(|r| r.path.clone())
            .collect();
        let scopes = self.home.state_dir().join("cloud-local-roots.json");
        if let Err(error) =
            zeus_app::engine_config::write_atomic(&scopes, &serde_json::to_vec(&roots)?)
        {
            // A failed replacement must not keep a revoked grant effective.
            let _ = std::fs::remove_file(&scopes);
            return Err(error.into());
        }
        if let Ok(mut c) = self.cfg.lock() {
            *c = merged.clone();
        }
        self.cancel.store(true, Ordering::SeqCst);
        self.send(Cmd::Reconfigure);
        Ok(merged)
    }

    /// The status the routes report.
    pub fn status(&self) -> Value {
        let st = self.status.lock().map(|s| s.clone()).unwrap_or_default();
        let (entries, generation, last_scan, capped) = self
            .index
            .lock()
            .map(|i| (i.entries.len(), i.generation, i.last_scan, i.capped))
            .unwrap_or((0, 0, None, false));
        let per_root = self.cfg.lock().map(|c| c.roots.clone()).unwrap_or_default();
        let roots: Vec<Value> = per_root
            .iter()
            .map(|r| {
                let count = self.index.lock().map(|i| i.under(&r.path).0).unwrap_or(0);
                let scanned = self.root_scans.lock().ok().and_then(|m| m.get(&norm(&r.path)).map(|x| x.1));
                json!({
                    "path": r.path, "include": r.include, "download": r.download, "monitor_changes": r.monitor_changes,
                    "rescan_every_hours": r.rescan_every_hours, "entries": count, "last_scan": scanned,
                    "exists": Path::new(&r.path).is_dir(), "manage_files": r.manage_files,
                })
            })
            .collect();
        let per = if st.roots_total == 0 {
            0.0
        } else {
            (st.roots_done as f64 + self.progress.fraction()) / st.roots_total as f64
        };
        json!({
            "state": if st.scanning { "scanning" } else { "current" },
            "progress": if st.scanning { per.min(0.99) } else { 1.0 },
            "scanning": st.scanning_root,
            "verifying": st.verifying,
            "entries": entries,
            "generation": generation,
            "last_scan": last_scan,
            "watcher": st.watcher,
            "capped": capped,
            "cap": crate::store::CAP,
            "roots": roots,
        })
    }

    fn set_status(&self, f: impl FnOnce(&mut StatusInner)) {
        if let Ok(mut s) = self.status.lock() {
            f(&mut s);
        }
    }

    fn snapshot_path(&self) -> PathBuf {
        index_dir(&self.home).join("index.bin")
    }

    fn save_snapshot(&self) {
        let bytes = self.index.lock().ok().map(|i| i.snapshot());
        if let Some(b) = bytes {
            let _ = std::fs::create_dir_all(index_dir(&self.home));
            let _ = zeus_app::engine_config::write_atomic(&self.snapshot_path(), &b);
        }
        self.dirty.store(false, Ordering::SeqCst);
    }

    fn excluder(&self) -> Excluder {
        Excluder::new(&self.cfg.lock().map(|c| c.clone()).unwrap_or_default())
    }

    async fn scan_roots(self: &Arc<Self>, only: Option<String>) {
        self.cancel.store(false, Ordering::SeqCst);
        let roots: Vec<RootCfg> = self
            .included()
            .into_iter()
            .filter(|r| only.as_ref().is_none_or(|o| norm(o) == norm(&r.path)))
            .collect();
        self.set_status(|s| {
            s.scanning = true;
            s.roots_total = roots.len();
            s.roots_done = 0;
        });
        for root in &roots {
            if self.cancel.load(Ordering::SeqCst) {
                break;
            }
            self.set_status(|s| s.scanning_root = root.path.clone());
            let svc = self.clone();
            let path = PathBuf::from(&root.path);
            let ex = self.excluder();
            let scan_id = self.scan_seq.fetch_add(1, Ordering::SeqCst);
            let _ = tokio::task::spawn_blocking(move || {
                if path.is_dir() {
                    scan::scan_root(&svc.index, &path, &ex, scan_id, &svc.progress, &svc.cancel);
                }
            })
            .await;
            if !self.cancel.load(Ordering::SeqCst)
                && let Ok(mut m) = self.root_scans.lock()
            {
                m.insert(norm(&root.path), (Instant::now(), now_ms()));
            }
            self.set_status(|s| s.roots_done += 1);
        }
        let completed = !self.cancel.load(Ordering::SeqCst);
        if let Ok(mut ix) = self.index.lock() {
            let paths: Vec<String> = self
                .included()
                .into_iter()
                .map(|r| display(Path::new(&r.path)))
                .collect();
            let g = ix.begin();
            ix.keep_only(g, &paths);
            if completed && only.is_none() {
                ix.last_scan = Some(now_ms());
            }
        }
        self.set_status(|s| {
            s.scanning = false;
            s.verifying = false;
        });
        if completed {
            self.save_snapshot();
        }
    }

    fn start_watcher(&self) -> Option<notify::RecommendedWatcher> {
        let tx = self.tx.clone();
        let mut w =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(ev) => {
                    if ev.need_rescan() {
                        let _ = tx.send(Cmd::RescanAll);
                    } else if !ev.paths.is_empty() {
                        let _ = tx.send(Cmd::Fs(ev.paths));
                    }
                }
                Err(_) => {
                    let _ = tx.send(Cmd::RescanAll);
                }
            })
            .ok()?;
        let mut any = false;
        let mut watched = Vec::new();
        for r in self.included().into_iter().filter(|r| r.monitor_changes) {
            if !Path::new(&r.path).is_dir() {
                continue;
            }
            // Remembered even when the system refuses a watcher (some network shares), so it is not retried forever.
            watched.push(norm(&r.path));
            if w.watch(Path::new(&r.path), RecursiveMode::Recursive)
                .is_ok()
            {
                any = true;
            }
        }
        if let Ok(mut m) = self.watched.lock() {
            *m = watched;
        }
        self.set_status(|s| s.watcher = any);
        any.then_some(w)
    }

    fn apply_paths(&self, paths: HashSet<PathBuf>) {
        let ex = self.excluder();
        let scan_id = self.scan_seq.fetch_add(1, Ordering::SeqCst);
        let included = self.included();
        let roots: Vec<(String, PathBuf)> = included
            .iter()
            .map(|r| (norm(&r.path), PathBuf::from(&r.path)))
            .collect();
        // A parent first, so a new folder is scanned once. A path in a skipped folder is dropped here.
        let mut list: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| {
                let k = norm(&display(p));
                roots.iter().any(|(r, root)| {
                    (k == *r || k.starts_with(&format!("{r}{}", std::path::MAIN_SEPARATOR)))
                        && !ex.skips_below(p, root)
                        && !ex.skips_by_attributes_below(p, root)
                })
            })
            .collect();
        list.sort();
        for p in list {
            scan::refresh_path(&self.index, &p, &ex, scan_id);
        }
        self.dirty.store(true, Ordering::SeqCst);
    }

    async fn run(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        // The snapshot is served at once; a scan then verifies it.
        self.send(Cmd::RescanAll);
        let mut watcher = self.start_watcher();
        let mut pending: HashSet<PathBuf> = HashSet::new();
        let mut first: Option<Instant> = None;
        let mut last_event = Instant::now();
        let mut last_save = Instant::now();
        let mut last_roots_check = Instant::now();
        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    match cmd {
                        Cmd::RescanAll => self.scan_roots(None).await,
                        Cmd::RescanRoot(r) => self.scan_roots(Some(r)).await,
                        Cmd::Reconfigure => {
                            drop(watcher.take());
                            watcher = self.start_watcher();
                            self.scan_roots(None).await;
                        }
                        Cmd::Fs(paths) => {
                            let ex = self.excluder();
                            for p in paths {
                                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                if ex.skips(&name, &p) {
                                    continue;
                                }
                                pending.insert(p);
                            }
                            if first.is_none() && !pending.is_empty() {
                                first = Some(Instant::now());
                            }
                            last_event = Instant::now();
                        }
                        Cmd::Touch(paths) => {
                            self.apply_paths(paths.into_iter().collect());
                            self.facts.clear();
                        }
                        Cmd::Stop(done) => {
                            drop(watcher.take());
                            if !pending.is_empty() {
                                let batch = std::mem::take(&mut pending);
                                self.apply_paths(batch);
                            }
                            self.save_snapshot();
                            let _ = done.send(());
                            return;
                        }
                    }
                }
                _ = ticker.tick() => {
                    // The watcher's events are applied when the folder has been quiet for a moment, or after 1.5 s of noise.
                    if let Some(f) = first
                        && (last_event.elapsed() >= Duration::from_millis(300) || f.elapsed() >= Duration::from_millis(1500)) {
                            let batch = std::mem::take(&mut pending);
                            first = None;
                            let svc = self.clone();
                            let _ = tokio::task::spawn_blocking(move || svc.apply_paths(batch)).await;
                    }
                    if self.dirty.load(Ordering::SeqCst) && last_save.elapsed() >= Duration::from_secs(10) {
                        let svc = self.clone();
                        let _ = tokio::task::spawn_blocking(move || svc.save_snapshot()).await;
                        last_save = Instant::now();
                    }
                    if last_roots_check.elapsed() >= Duration::from_secs(5) {
                        last_roots_check = Instant::now();
                        // The download folders can change in Settings; follow them.
                        let download = download_roots(&self.home);
                        let changed = self.cfg.lock().map(|c| {
                            let have: Vec<String> = c.roots.iter().filter(|r| r.download).map(|r| norm(&r.path)).collect();
                            let want: Vec<String> = download.iter().map(|d| norm(d)).collect();
                            have != want
                        }).unwrap_or(false);
                        // A folder that did not exist when the watcher was set up (the engine creates the download
                        // folder at start) gets its watcher and a scan as soon as it is there.
                        let want_watch: Vec<String> = self.included().into_iter().filter(|r| r.monitor_changes && Path::new(&r.path).is_dir()).map(|r| norm(&r.path)).collect();
                        let have_watch = self.watched.lock().map(|m| m.clone()).unwrap_or_default();
                        if !changed && want_watch.iter().any(|w| !have_watch.contains(w)) {
                            self.send(Cmd::Reconfigure);
                        }
                        if changed {
                            if let Ok(mut c) = self.cfg.lock() {
                                *c = c.clone().with_download_roots(&download);
                                let _ = c.save(&index_dir(&self.home).join("index.json"));
                            }
                            self.send(Cmd::Reconfigure);
                        }
                        // Periodic rescans.
                        let due: Vec<String> = self.included().into_iter().filter(|r| {
                            r.rescan_every_hours.is_some_and(|h| {
                                self.root_scans.lock().ok().and_then(|m| m.get(&norm(&r.path)).map(|x| x.0.elapsed() >= Duration::from_secs(u64::from(h) * 3600))).unwrap_or(false)
                            })
                        }).map(|r| r.path).collect();
                        for r in due {
                            self.send(Cmd::RescanRoot(r));
                        }
                    }
                }
            }
        }
    }
}

/// The download folders: the engine's write roots.
pub fn download_roots(home: &AppHome) -> Vec<String> {
    zeus_app::engine_config::load(home)
        .map(|s| {
            s.write_roots
                .iter()
                .map(|r| display(Path::new(r)))
                .collect()
        })
        .unwrap_or_default()
}
