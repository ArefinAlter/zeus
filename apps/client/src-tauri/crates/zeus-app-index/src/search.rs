//! Bounded native search over the scoped metadata index. The frontend supplies its parsed query;
//! Rust compiles expressions once and retains only the requested page, never the whole result set.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::time::Instant;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use zeus_app::failure::{Failure, Result};

use crate::config::norm;
use crate::store::{Index, Row};

/// A parsed search term. Regexes use Rust's bounded, linear-time engine.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Term {
    /// Text in a filename or path.
    Text {
        /// Text to find.
        value: String,
        /// Search the full path instead of its basename.
        path: bool,
        /// Preserve case while comparing text.
        match_case: bool,
        /// Require ASCII alphanumeric word boundaries.
        whole_word: bool,
        /// Invert this term.
        neg: bool,
    },
    /// Wildcards converted to anchored regex by the shared frontend parser.
    Wild {
        /// Anchored regex source generated from a wildcard.
        pattern: String,
        /// Search the full path.
        path: bool,
        /// Match case.
        match_case: bool,
        /// Invert this term.
        neg: bool,
    },
    /// A regular expression without backreferences or lookaround.
    Regex {
        /// Regular expression source.
        pattern: String,
        /// Search the full path.
        path: bool,
        /// Match case.
        match_case: bool,
        /// Invert this term.
        neg: bool,
    },
    /// One of the exact file extensions.
    Ext {
        /// Extension list without leading dots.
        exts: Vec<String>,
        /// Invert this term.
        neg: bool,
    },
    /// Inclusive byte range.
    Size {
        /// Minimum file length.
        min: u64,
        /// Maximum file length.
        max: u64,
        /// Invert this term.
        neg: bool,
    },
    /// Half-open modification-time range in epoch milliseconds.
    Dm {
        /// Inclusive lower bound.
        from: i64,
        /// Exclusive upper bound.
        to: i64,
        /// Invert this term.
        neg: bool,
    },
    /// Files or directories.
    Type {
        /// Match directories when true, otherwise regular files.
        folder: bool,
        /// Invert this term.
        neg: bool,
    },
    /// Host provenance is explicitly unavailable to the metadata-only native index.
    Host {
        /// Requested host name.
        value: String,
        /// Invert this term.
        neg: bool,
    },
}

/// A paged, generation-bound query. Groups are ANDed; each group's terms are ORed.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Request {
    /// Parsed terms, at most 64 in total.
    pub groups: Vec<Vec<Term>>,
    /// Optional extension/category filter.
    pub filter: String,
    /// Name, path, size or modified.
    pub sort: String,
    /// Reverse the selected ordering.
    pub descending: bool,
    /// Page offset, at most 10,000.
    pub offset: usize,
    /// At most 500 results per page.
    pub limit: usize,
    /// Required beyond the first page; a changed generation asks the client to restart.
    pub generation: Option<u64>,
}

/// One bounded page and complete-match metadata.
#[derive(Debug, Serialize)]
pub struct Page {
    /// Exact index generation used by this page.
    pub generation: u64,
    /// Bounded visible rows.
    pub files: Vec<Row>,
    /// Number of matches across this index generation.
    pub total: usize,
    /// Bytes of matching regular files.
    pub total_bytes: u64,
    /// Next offset; no next page when absent.
    pub next: Option<usize>,
    /// Native query execution time.
    pub elapsed_ms: u64,
}

struct Compiled {
    term: Term,
    regex: Option<Regex>,
    folded: Option<String>,
    mask: u128,
}

fn compile(term: &Term) -> Result<Compiled> {
    let regex = match term {
        Term::Regex {
            pattern,
            match_case,
            ..
        }
        | Term::Wild {
            pattern,
            match_case,
            ..
        } => {
            if pattern.len() > 2048 {
                return Err(Failure::invalid(
                    "A search expression has at most 2,048 bytes.",
                ));
            }
            Some(RegexBuilder::new(pattern).case_insensitive(!*match_case)
                .size_limit(2 * 1024 * 1024).dfa_size_limit(2 * 1024 * 1024).build()
                .map_err(|_| Failure::invalid("This expression is not supported by native search; lookaround and backreferences are unavailable."))?)
        }
        Term::Text { value, .. } if value.len() > 2048 => {
            return Err(Failure::invalid("A search term has at most 2,048 bytes."));
        }
        Term::Ext { exts, .. } if exts.len() > 64 || exts.iter().any(|x| x.len() > 32) => {
            return Err(Failure::invalid("Use at most 64 extensions of 32 bytes."));
        }
        Term::Host { value, neg } => {
            let _ = (value, neg);
            return Err(Failure::new(
                "app.search_unsupported",
                "host: needs download provenance. Search Downloads by site instead.",
            ));
        }
        _ => None,
    };
    Ok(Compiled {
        term: term.clone(),
        regex,
        folded: if let Term::Text { value, .. } = term {
            Some(value.to_lowercase())
        } else {
            None
        },
        mask: if let Term::Text {
            value, match_case, ..
        } = term
        {
            if *match_case && !value.is_ascii() {
                0
            } else {
                crate::store::trigram_mask(value)
            }
        } else {
            0
        },
    })
}

fn word_match(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(at, value)| {
        let word = |c: char| c.is_ascii_alphanumeric();
        hay[..at].chars().next_back().is_none_or(|c| !word(c))
            && hay[at + value.len()..]
                .chars()
                .next()
                .is_none_or(|c| !word(c))
    })
}

fn hit(t: &Compiled, path: &str, name: &str, entry: &crate::store::Entry) -> bool {
    let (yes, neg) = match &t.term {
        Term::Text {
            value,
            path: full,
            match_case,
            whole_word,
            neg,
        } => {
            let available = if *full {
                entry.path_mask
            } else {
                entry.name_mask
            };
            if available & t.mask != t.mask {
                return *neg;
            }
            let text = if *full { path } else { name };
            if *match_case {
                (
                    if *whole_word {
                        word_match(text, value)
                    } else {
                        text.contains(value)
                    },
                    *neg,
                )
            } else {
                let hay = text.to_lowercase();
                let needle = t.folded.as_deref().unwrap_or("");
                (
                    if *whole_word {
                        word_match(&hay, needle)
                    } else {
                        hay.contains(needle)
                    },
                    *neg,
                )
            }
        }
        Term::Regex {
            path: full, neg, ..
        }
        | Term::Wild {
            path: full, neg, ..
        } => (
            t.regex
                .as_ref()
                .is_some_and(|r| r.is_match(if *full { path } else { name })),
            *neg,
        ),
        Term::Ext { exts, neg } => (
            !entry.dir
                && name
                    .rsplit_once('.')
                    .is_some_and(|(_, ext)| exts.iter().any(|x| x.eq_ignore_ascii_case(ext))),
            *neg,
        ),
        Term::Size { min, max, neg } => {
            (!entry.dir && entry.size >= *min && entry.size <= *max, *neg)
        }
        Term::Dm { from, to, neg } => (entry.mtime >= *from && entry.mtime < *to, *neg),
        Term::Type { folder, neg } => (entry.dir == *folder, *neg),
        Term::Host { .. } => (false, false),
    };
    yes != neg
}

fn category(filter: &str, name: &str, directory: bool) -> bool {
    if filter.is_empty() || filter == "everything" {
        return true;
    }
    if filter == "folder" {
        return directory;
    }
    if directory {
        return false;
    }
    let ext = name.rsplit_once('.').map_or("", |(_, x)| x);
    let extensions: &[&str] = match filter {
        "audio" => &["mp3", "flac", "wav", "ogg", "opus", "m4a", "aac", "wma"],
        "compressed" => &[
            "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "iso", "cab",
        ],
        "document" => &[
            "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "txt", "md", "epub",
            "csv", "json", "rtf",
        ],
        "executable" => &[
            "exe", "msi", "msix", "bat", "cmd", "ps1", "sh", "jar", "apk", "appimage", "dmg",
            "pkg", "deb", "rpm",
        ],
        "picture" => &[
            "png", "jpg", "jpeg", "gif", "webp", "svg", "tif", "tiff", "heic", "raw", "bmp",
        ],
        "video" => &["mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "ts"],
        _ => &[],
    };
    extensions.iter().any(|x| x.eq_ignore_ascii_case(ext))
}

/// Whether a displayed indexed path remains inside a currently included scope.
pub fn in_scope(path: &str, roots: &[String]) -> bool {
    let key = norm(path);
    roots.iter().any(|root| {
        key == *root
            || key
                .strip_prefix(root)
                .is_some_and(|tail| tail.starts_with(std::path::MAIN_SEPARATOR))
    })
}

#[derive(Eq, PartialEq)]
struct Candidate<'a> {
    key: Key,
    path: &'a str,
    entry: &'a crate::store::Entry,
    descending: bool,
}
#[derive(Eq, PartialEq, Ord, PartialOrd)]
enum Key {
    Text(String),
    Size(u64),
    Modified(i64),
}
impl Ord for Candidate<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        let cmp = self
            .key
            .cmp(&other.key)
            .then_with(|| self.path.cmp(other.path));
        if self.descending { cmp.reverse() } else { cmp }
    }
}
impl PartialOrd for Candidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Execute a generation-bound search with O(page offset + limit) retained candidates.
pub fn query(index: &Index, request: &Request, roots: &[String]) -> Result<Page> {
    let start = Instant::now();
    let count = request.groups.iter().map(Vec::len).sum::<usize>();
    if count > 64
        || request.groups.len() > 64
        || request.groups.iter().any(Vec::is_empty)
        || request.offset > 10_000
    {
        return Err(Failure::invalid(
            "Use at most 64 search terms and page through at most 10,000 matches; narrow the search for more.",
        ));
    }
    if request.offset > 0 && request.generation != Some(index.generation) {
        return Err(Failure::new(
            "app.index_changed",
            "Files changed while paging. Refresh this search.",
        ));
    }
    if !["", "name", "path", "size", "modified"].contains(&request.sort.as_str())
        || ![
            "",
            "everything",
            "folder",
            "audio",
            "compressed",
            "document",
            "executable",
            "picture",
            "video",
        ]
        .contains(&request.filter.as_str())
    {
        return Err(Failure::invalid("Unknown file-search ordering or filter."));
    }
    let groups: Vec<Vec<Compiled>> = request
        .groups
        .iter()
        .map(|group| group.iter().map(compile).collect())
        .collect::<Result<_>>()?;
    let roots: Vec<_> = roots.iter().map(|x| norm(x)).collect();
    let limit = if request.limit == 0 {
        500
    } else {
        request.limit.min(500)
    };
    let keep = request.offset + limit;
    let mut best = BinaryHeap::with_capacity(keep + 1);
    let mut total = 0;
    let mut total_bytes = 0u64;
    for (path, entry) in &index.entries {
        if !in_scope(path, &roots) {
            continue;
        }
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if !category(&request.filter, name, entry.dir)
            || !groups
                .iter()
                .all(|group| group.iter().any(|term| hit(term, path, name, entry)))
        {
            continue;
        }
        total += 1;
        if !entry.dir {
            total_bytes = total_bytes.saturating_add(entry.size);
        }
        let key = match request.sort.as_str() {
            "size" => Key::Size(entry.size),
            "modified" => Key::Modified(entry.mtime),
            "path" => Key::Text(path.to_lowercase()),
            _ => Key::Text(name.to_lowercase()),
        };
        let candidate = Candidate {
            key,
            path,
            entry,
            descending: request.descending,
        };
        if best.len() < keep {
            best.push(candidate);
        } else if best.peek().is_some_and(|last| candidate < *last) {
            best.pop();
            best.push(candidate);
        }
    }
    let mut matches = best.into_sorted_vec();
    let skip = request.offset.min(matches.len());
    let files = matches
        .drain(skip..)
        .map(|c| Row {
            path: c.path.to_owned(),
            size: c.entry.size,
            mtime: c.entry.mtime,
            is_dir: c.entry.dir,
        })
        .collect();
    Ok(Page {
        generation: index.generation,
        files,
        total,
        total_bytes,
        next: (request.offset + limit < total && request.offset + limit <= 10_000)
            .then_some(request.offset + limit),
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn index() -> (Index, Vec<String>) {
        let mut index = Index::default();
        let gen_ = index.begin();
        for (path, size) in [
            ("/allowed/report.PDF", 900),
            ("/allowed/report-old.txt", 100),
            ("/allowed/movie.mp4", 2000),
            ("/revoked/report.pdf", 800),
        ] {
            index.upsert(
                gen_,
                &path.replace('/', std::path::MAIN_SEPARATOR_STR),
                size,
                100,
                false,
                1,
            );
        }
        (
            index,
            vec!["/allowed".replace('/', std::path::MAIN_SEPARATOR_STR)],
        )
    }
    #[test]
    fn scopes_boolean_groups_regex_and_metadata_agree() {
        let (index, roots) = index();
        let request: Request = serde_json::from_value(serde_json::json!({"groups":[[{"kind":"ext","exts":["pdf","mp4"],"neg":false}],[{"kind":"size","min":500,"max":1500,"neg":false}]],"limit":1})).unwrap();
        let page = query(&index, &request, &roots).unwrap();
        assert_eq!(page.total, 1);
        assert!(page.files[0].path.ends_with("report.PDF"));
        let bad: Request = serde_json::from_value(serde_json::json!({"groups":[[{"kind":"regex","pattern":"(?=secret)","path":false,"match_case":false,"neg":false}]]})).unwrap();
        assert!(query(&index, &bad, &roots).is_err());
    }
    #[test]
    fn paging_is_bounded_ordered_and_rejects_a_changed_generation() {
        let (mut index, roots) = index();
        let first = Request {
            sort: "size".into(),
            descending: true,
            limit: 1,
            ..Request::default()
        };
        let p = query(&index, &first, &roots).unwrap();
        assert_eq!(p.total, 3);
        assert_eq!(p.files[0].size, 2000);
        assert_eq!(p.next, Some(1));
        let next = Request {
            offset: 1,
            generation: Some(p.generation),
            ..first
        };
        assert_eq!(query(&index, &next, &roots).unwrap().files[0].size, 900);
        index.begin();
        assert_eq!(
            query(&index, &next, &roots).unwrap_err().code,
            "app.index_changed"
        );
    }
    #[test]
    fn large_index_query_retains_only_a_page() {
        let mut index = Index::default();
        let g = index.begin();
        for n in 0..100_000 {
            index.upsert(
                g,
                &format!(
                    "{}{:06}.bin",
                    "/allowed/".replace('/', std::path::MAIN_SEPARATOR_STR),
                    n
                ),
                n,
                10,
                false,
                1,
            );
        }
        let roots = vec!["/allowed".replace('/', std::path::MAIN_SEPARATOR_STR)];
        let p = query(
            &index,
            &Request {
                sort: "size".into(),
                descending: true,
                limit: 25,
                ..Request::default()
            },
            &roots,
        )
        .unwrap();
        assert_eq!(p.total, 100_000);
        assert_eq!(p.files.len(), 25);
        assert_eq!(p.files[0].size, 99_999);
        eprintln!(
            "scoped_search entries={} page={} elapsed_ms={}",
            p.total,
            p.files.len(),
            p.elapsed_ms
        );
    }
    #[test]
    fn trigram_pruning_never_changes_unicode_case_path_or_negation_results() {
        let mut index = Index::default();
        let g = index.begin();
        let prefix = "/allowed/".replace('/', std::path::MAIN_SEPARATOR_STR);
        for name in [
            "Résumé-資料.txt",
            "REPORT-2026.txt",
            "report.txt",
            "unrelated.bin",
        ] {
            index.upsert(g, &format!("{prefix}{name}"), 1, 10, false, 1);
        }
        let roots = vec!["/allowed".replace('/', std::path::MAIN_SEPARATOR_STR)];
        for (value, neg, count) in [
            ("résumé", false, 1),
            ("REPORT", false, 2),
            ("資料", false, 1),
            ("report", true, 2),
            ("not present", false, 0),
            ("not present", true, 4),
        ] {
            let req = Request {
                groups: vec![vec![Term::Text {
                    value: value.into(),
                    path: false,
                    match_case: false,
                    whole_word: false,
                    neg,
                }]],
                ..Request::default()
            };
            assert_eq!(
                query(&index, &req, &roots).unwrap().total,
                count,
                "{value} neg={neg}"
            );
        }
    }

    #[test]
    fn exact_case_unicode_is_not_pruned_by_contextual_lowercasing() {
        let mut ix = Index::default();
        let g = ix.begin();
        ix.upsert(
            g,
            &"/allowed/ΑΣΣΑ.txt".replace('/', std::path::MAIN_SEPARATOR_STR),
            1,
            0,
            false,
            1,
        );
        let request = Request {
            groups: vec![vec![Term::Text {
                value: "ΑΣΣ".into(),
                path: false,
                match_case: true,
                whole_word: false,
                neg: false,
            }]],
            ..Request::default()
        };
        assert_eq!(
            query(
                &ix,
                &request,
                &["/allowed".replace('/', std::path::MAIN_SEPARATOR_STR)]
            )
            .unwrap()
            .total,
            1
        );
    }
}
