//! The index configuration (`app/index.json`): which folders are indexed, what is skipped, and when to rescan.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeus_app::failure::{Failure, Result};

/// One indexed folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCfg {
    /// Absolute folder path, as shown to the person.
    pub path: String,
    /// False hides the folder from search without forgetting it.
    #[serde(default = "yes")]
    pub include: bool,
    /// Follow changes with the file system watcher.
    #[serde(default = "yes")]
    pub monitor_changes: bool,
    /// Rescan every this many hours (`None`: only on request and at start).
    #[serde(default)]
    pub rescan_every_hours: Option<u32>,
    /// A download folder of the engine: it cannot be removed here (change it in Settings, Save to).
    #[serde(default)]
    pub download: bool,
    /// Explicit permission to manage files here, independent of search inclusion.
    #[serde(default)]
    pub manage_files: bool,
}

fn yes() -> bool {
    true
}

/// The whole configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    /// Indexed folders: the download folders plus the ones the person added.
    pub roots: Vec<RootCfg>,
    /// Names and folders to skip: one pattern each, `*` and `?` allowed; a pattern with a path separator matches the
    /// whole path.
    pub exclusions: Vec<String>,
    /// Skip hidden files and folders.
    pub exclude_hidden: bool,
    /// Skip system files and folders (Windows system attribute).
    pub exclude_system: bool,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            exclusions: vec![
                "*.zpart".into(),
                ".zeus".into(),
                ".zeus-partial".into(),
                "node_modules".into(),
                "Thumbs.db".into(),
                "desktop.ini".into(),
            ],
            exclude_hidden: true,
            exclude_system: true,
        }
    }
}

/// Most folders, patterns and pattern length the page may send.
pub const MAX_ROOTS: usize = 16;
const MAX_PATTERNS: usize = 64;
const MAX_PATTERN_LEN: usize = 200;

impl IndexConfig {
    /// Load `path`; a missing or damaged file gives the defaults.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Make the download folders present (and only they flagged as downloads), in front of the added ones.
    pub fn with_download_roots(mut self, download: &[String]) -> Self {
        let same = |a: &str, b: &str| norm(a) == norm(b);
        for r in &mut self.roots {
            r.download = download.iter().any(|d| same(d, &r.path));
        }
        let mut roots: Vec<RootCfg> = Vec::new();
        for d in download {
            match self.roots.iter().find(|r| same(&r.path, d)) {
                Some(r) => roots.push(r.clone()),
                None => roots.push(RootCfg {
                    path: d.clone(),
                    include: true,
                    monitor_changes: true,
                    rescan_every_hours: None,
                    download: true,
                    manage_files: false,
                }),
            }
        }
        for r in &self.roots {
            if !r.download && !roots.iter().any(|x| same(&x.path, &r.path)) {
                roots.push(r.clone());
            }
        }
        self.roots = roots;
        self
    }

    /// Check what the page sent.
    pub fn validate(&self) -> Result<()> {
        if self.roots.len() > MAX_ROOTS {
            return Err(Failure::invalid(format!(
                "Index at most {MAX_ROOTS} folders."
            )));
        }
        for r in &self.roots {
            let p = Path::new(&r.path);
            if !p.is_absolute() {
                return Err(Failure::invalid(format!(
                    "{} is not an absolute folder path.",
                    r.path
                )));
            }
            if !r.download && !p.is_dir() {
                return Err(Failure::invalid(format!(
                    "{} is not a folder that exists on this computer.",
                    r.path
                )));
            }
            if r.rescan_every_hours.is_some_and(|h| h == 0 || h > 24 * 90) {
                return Err(Failure::invalid(
                    "Rescan every 1 to 2160 hours, or leave it empty.",
                ));
            }
        }
        if self.exclusions.len() > MAX_PATTERNS
            || self.exclusions.iter().any(|p| p.len() > MAX_PATTERN_LEN)
        {
            return Err(Failure::invalid(
                "Use at most 64 exclusion patterns of 200 characters.",
            ));
        }
        Ok(())
    }

    /// Save atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        zeus_app::engine_config::write_atomic(path, &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

/// A path as a comparison key: platform separators, no trailing separator, lowercase on Windows.
pub fn norm(p: &str) -> String {
    let s = p.replace(
        if cfg!(windows) { '/' } else { '\\' },
        if cfg!(windows) { "\\" } else { "/" },
    );
    let s = s.trim_end_matches(['\\', '/']).to_owned();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// `*` and `?` wildcard match of the whole `text`, case-insensitive.
pub fn wild(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// What to skip while scanning and watching.
#[derive(Debug, Clone)]
pub struct Excluder {
    names: Vec<String>,
    paths: Vec<String>,
    /// Skip hidden files.
    pub hidden: bool,
    /// Skip system files.
    pub system: bool,
}

impl Excluder {
    /// Build from the configuration.
    pub fn new(cfg: &IndexConfig) -> Self {
        let (paths, names): (Vec<String>, Vec<String>) = cfg
            .exclusions
            .iter()
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .partition(|p| p.contains(['\\', '/']));
        Self {
            names,
            paths: paths.into_iter().map(|p| p.replace('\\', "/")).collect(),
            hidden: cfg.exclude_hidden,
            system: cfg.exclude_system,
        }
    }

    /// True when an entry named `name` at `path` is skipped by a pattern.
    pub fn skips(&self, name: &str, path: &Path) -> bool {
        if name.eq_ignore_ascii_case(".zeus")
            || name.eq_ignore_ascii_case(".zeus-partial")
            || self.names.iter().any(|p| wild(p, name))
        {
            return true;
        }
        if self.paths.is_empty() {
            return false;
        }
        let full = path.display().to_string().replace('\\', "/");
        self.paths.iter().any(|p| {
            if p.contains(['*', '?']) {
                wild(p, &full)
            } else {
                full.to_lowercase().contains(&p.to_lowercase())
            }
        })
    }

    /// True when `path`, or any folder between it and `root`, is skipped by name or pattern. The scanner never enters
    /// a skipped folder; the watcher reports files inside one, and this keeps them out as well.
    pub fn skips_below(&self, path: &Path, root: &Path) -> bool {
        let rel = path.strip_prefix(root).unwrap_or(path);
        let mut walk = root.to_path_buf();
        for c in rel.components() {
            let name = c.as_os_str().to_string_lossy();
            walk.push(c.as_os_str());
            if self.skips(&name, &walk) {
                return true;
            }
        }
        false
    }

    /// True when `path` or a folder between it and `root` is hidden or system and the settings skip those.
    pub fn skips_by_attributes_below(&self, path: &Path, root: &Path) -> bool {
        if !self.hidden && !self.system {
            return false;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        let mut walk = root.to_path_buf();
        for c in rel.components() {
            walk.push(c.as_os_str());
            let name = c.as_os_str().to_string_lossy();
            if let Ok(meta) = std::fs::symlink_metadata(&walk)
                && self.skips_attributes(&name, &meta)
            {
                return true;
            }
        }
        false
    }

    /// Attribute exclusion through retained directories; uncertain ancestor metadata is skipped.
    pub fn skips_by_attributes_pinned(
        &self,
        path: &Path,
        root: &Path,
        roots: &crate::paths::RootCaps,
    ) -> bool {
        if !self.hidden && !self.system {
            return false;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            return true;
        };
        let mut walk = root.to_path_buf();
        for component in relative.components() {
            walk.push(component.as_os_str());
            match roots.os_metadata(&walk) {
                Ok(Some(meta))
                    if self.skips_attributes(&component.as_os_str().to_string_lossy(), &meta) =>
                {
                    return true;
                }
                Err(_) => return true,
                _ => (),
            }
        }
        false
    }

    /// True when the file's attributes mark it hidden or system and the settings skip those.
    pub fn skips_attributes(&self, name: &str, meta: &std::fs::Metadata) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            let _ = name;
            let a = meta.file_attributes();
            (self.hidden && a & 0x2 != 0) || (self.system && a & 0x4 != 0)
        }
        #[cfg(not(windows))]
        {
            let _ = meta;
            self.hidden && name.starts_with('.')
        }
    }
}

/// The directory of the index files for `home`.
pub fn index_dir(home: &zeus_app::home::AppHome) -> PathBuf {
    home.app_dir().join("index")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_match_whole_names_case_insensitively() {
        assert!(wild("*.zpart", "Movie.ZPART"));
        assert!(wild("node_modules", "NODE_MODULES"));
        assert!(wild("a?c*", "abcdef"));
        assert!(!wild("*.zip", "a.zip.bak"));
        assert!(!wild("a?c", "ac"));
        assert!(wild("*", ""));
    }

    #[test]
    fn exclusions_by_name_and_by_path() {
        let cfg = IndexConfig {
            exclusions: vec!["*.tmp".into(), "cache/old".into()],
            ..IndexConfig::default()
        };
        let ex = Excluder::new(&cfg);
        assert!(ex.skips("a.TMP", Path::new("/r/a.TMP")));
        assert!(ex.skips("old", Path::new("/r/cache/old")));
        assert!(!ex.skips("new", Path::new("/r/cache/new")));
    }

    #[test]
    fn files_inside_a_skipped_folder_are_skipped_too() {
        let ex = Excluder::new(&IndexConfig::default());
        let root = Path::new("/r");
        assert!(ex.skips_below(Path::new("/r/node_modules/x/index.js"), root));
        assert!(ex.skips_below(Path::new("/r/.zeus/trash/a.bin"), root));
        assert!(ex.skips_below(Path::new("/r/a/movie.zpart"), root));
        assert!(!ex.skips_below(Path::new("/r/a/b.txt"), root));
        // The root's own name is never judged: a root inside a folder called node_modules still indexes.
        assert!(!ex.skips_below(
            Path::new("/node_modules/r/a.txt"),
            Path::new("/node_modules/r")
        ));
    }

    #[test]
    fn download_roots_come_first_and_cannot_be_dropped() {
        let cfg = IndexConfig {
            roots: vec![RootCfg {
                path: "/extra".into(),
                include: true,
                monitor_changes: false,
                rescan_every_hours: Some(6),
                download: false,
                manage_files: false,
            }],
            ..IndexConfig::default()
        }
        .with_download_roots(&["/dl".to_owned()]);
        assert_eq!(cfg.roots[0].path, "/dl");
        assert!(cfg.roots[0].download);
        assert_eq!(cfg.roots[1].path, "/extra");
        assert_eq!(cfg.roots[1].rescan_every_hours, Some(6));
        // A removed download folder drops out; a normal folder stays.
        let cfg = cfg.with_download_roots(&["/dl2".to_owned()]);
        assert_eq!(
            cfg.roots
                .iter()
                .map(|r| r.path.as_str())
                .collect::<Vec<_>>(),
            ["/dl2", "/dl", "/extra"]
        );
        assert!(!cfg.roots[1].download);
    }
}
