//! The site grabber's walk: a bounded, polite, same-site crawl that lists what would be fetched before anything is.
//!
//! Rules it keeps, in the order a reader would check them:
//! * http and https only, no user name or password in any address;
//! * `robots.txt` of every host is read first and honoured (Disallow/Allow by longest match, `Crawl-delay`), as the
//!   user agent `Zeus`; a robots.txt that cannot be read because of a server error means "nothing is allowed";
//! * requests to one host are spaced: at least the configured delay (default one second, never below 250 ms, which is
//!   four requests a second) and never closer than the site's `Crawl-delay`;
//! * pages are fetched only while the depth, page, file and byte limits allow, only HTML is read (at most 512 KiB a
//!   page), nothing is executed, and a page already read is never read again (a loop is not followed);
//! * the walk stays on the start host unless the person allows more, and a redirect that leaves the allowed hosts is
//!   refused, never followed silently;
//! * a file's size comes from a `HEAD` request (or a one-byte ranged `GET` when the server refuses `HEAD`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE};
use reqwest::{Client, Method, Response, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::html::{self, Source};
use crate::robots::{Robots, any_match};

/// The most pages one walk reads.
pub const MAX_PAGES: u32 = 200;
/// The most hosts one walk talks to when it may leave the start host.
const MAX_HOSTS: usize = 5;
/// The least delay between requests to one host (four a second).
pub const MIN_DELAY_MS: u64 = 250;
/// The default delay between requests to one host.
pub const DEFAULT_DELAY_MS: u64 = 1000;
const PAGE_BYTES: usize = 512 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const WALK_BUDGET: Duration = Duration::from_secs(600);
const MAX_REDIRECTS: usize = 5;

/// The client's `SiteRequest`, plus optional pacing and a cancellation id.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SiteRequest {
    /// The page the walk starts from.
    #[serde(rename = "startUrl")]
    pub start_url: String,
    /// Links to follow from the start page (0 reads only the start page).
    pub depth: u32,
    /// Stay on the start host.
    #[serde(rename = "sameHostOnly")]
    pub same_host_only: bool,
    /// Extensions to take; empty takes every type.
    pub types: Vec<String>,
    /// Paths to include (globs or substrings); empty includes all.
    pub include: Vec<String>,
    /// Paths to exclude.
    pub exclude: Vec<String>,
    /// Most files to list.
    #[serde(rename = "maxFiles")]
    pub max_files: u32,
    /// Most MiB to list.
    #[serde(rename = "maxMiB")]
    pub max_mib: u64,
    /// Milliseconds between requests to one host (at least 250; the site's Crawl-delay may raise it).
    #[serde(rename = "delayMs")]
    pub delay_ms: Option<u64>,
    /// Most pages to read (at most 200).
    #[serde(rename = "maxPages")]
    pub max_pages: Option<u32>,
    /// Identifies this walk so it can be cancelled.
    #[serde(rename = "walkId")]
    pub walk_id: Option<String>,
}

impl Default for SiteRequest {
    fn default() -> Self {
        Self {
            start_url: String::new(),
            depth: 2,
            same_host_only: true,
            types: Vec::new(),
            include: Vec::new(),
            exclude: Vec::new(),
            max_files: 50,
            max_mib: 500,
            delay_ms: None,
            max_pages: None,
            walk_id: None,
        }
    }
}

/// A page the walk read.
#[derive(Debug, Clone, Serialize)]
pub struct PageRead {
    /// Its address.
    pub url: String,
    /// Links from the start page.
    pub depth: u32,
}

/// A file the walk would fetch.
#[derive(Debug, Clone, Serialize)]
pub struct FileFound {
    /// The address, with its query string when the link had one.
    pub url: String,
    /// The name it would be saved under.
    pub name: String,
    /// Bytes, 0 when the server did not say.
    pub size: u64,
    /// Depth of the page it was found on.
    pub depth: u32,
    /// The page it was found on.
    pub from: String,
    /// The host it is served from.
    pub host: String,
}

/// How many links one rule kept out.
#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    /// The rule in plain words.
    pub reason: String,
    /// How many addresses.
    pub count: u32,
}

/// What robots.txt said about the start host.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RobotsFacts {
    /// A robots.txt was found and read.
    pub found: bool,
    /// Rules that apply to Zeus.
    pub rules: usize,
    /// The site's Crawl-delay in milliseconds, if any.
    pub crawl_delay_ms: Option<u64>,
}

/// The walk's answer (the client's `SitePreview`, extended).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SitePreview {
    /// Whether the walk ran.
    pub ok: bool,
    /// Why it did not, in plain words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The start host.
    pub host: String,
    /// Pages read.
    pub pages: Vec<PageRead>,
    /// Files that would be fetched.
    pub files: Vec<FileFound>,
    /// What was left out, and why.
    pub skipped: Vec<Skipped>,
    /// Bytes of the files listed.
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    /// What robots.txt of the start host said.
    pub robots: RobotsFacts,
    /// HTTP requests made.
    pub requests: u32,
    /// Files whose size the server did not give.
    #[serde(rename = "unknownSizes")]
    pub unknown_sizes: u32,
    /// Milliseconds the walk took.
    #[serde(rename = "elapsedMs")]
    pub elapsed_ms: u64,
}

impl SitePreview {
    fn fail(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Self::default()
        }
    }
}

/// Words a person reads for each way a link is left out.
mod why {
    pub const ROBOTS: &str = "blocked by robots.txt";
    pub const DEPTH: &str = "deeper than the depth limit";
    pub const OTHER_HOST: &str = "link to another host";
    pub const FILE_OTHER_HOST: &str = "file on another host";
    pub const TYPE: &str = "file type not selected";
    pub const INCLUDE: &str = "not matched by an include rule";
    pub const EXCLUDE_FILE: &str = "file excluded by a rule";
    pub const EXCLUDE_PAGE: &str = "page excluded by a rule";
    pub const DUPLICATE: &str = "duplicate";
    pub const FILE_LIMIT: &str = "over the file limit";
    pub const SIZE_LIMIT: &str = "over the size limit";
    pub const PAGE_LIMIT: &str = "over the page limit";
    pub const NO_ANSWER: &str = "page did not answer";
    pub const NOT_HTML: &str = "not an HTML page";
    pub const REDIRECT_AWAY: &str = "redirect to another host";
    pub const REDIRECTS: &str = "too many redirects";
    pub const IS_A_PAGE: &str = "a web page, not a file";
    pub const TIME: &str = "time limit of the walk reached";
}

const PAGE_EXTS: &[&str] = &[
    "", "html", "htm", "xhtml", "php", "asp", "aspx", "jsp", "shtml", "cgi",
];

/// The extension of the last path segment, lower case (`tar.gz` gives `gz`).
pub fn extension(path: &str) -> String {
    let seg = path.rsplit('/').next().unwrap_or("");
    match seg.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && ext.len() <= 10
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            ext.to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

fn file_name(url: &Url) -> String {
    let seg = url.path().rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    let decoded = percent_decode(seg);
    if decoded.is_empty() {
        "index".into()
    } else {
        decoded
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

struct HostState {
    robots: Option<Robots>,
    last: Option<Instant>,
}

enum Fetch {
    Got(Response, Url),
    Refused(&'static str),
    Failed,
}

/// One walk.
pub struct Crawler {
    client: Client,
    cancel: Arc<AtomicBool>,
    min_delay: Duration,
    start_host: String,
    same_host_only: bool,
    hosts: HashMap<String, HostState>,
    requests: u32,
    began: Instant,
}

fn host_key(u: &Url) -> String {
    format!("{}://{}", u.scheme(), u.host_str().unwrap_or(""))
        + &u.port().map(|p| format!(":{p}")).unwrap_or_default()
}

impl Crawler {
    /// A crawler that stops when `cancel` is set.
    pub fn new(req: &SiteRequest, start: &Url, cancel: Arc<AtomicBool>) -> Result<Self, String> {
        let client = Client::builder()
            .user_agent(format!(
                "Zeus/{} (+site grabber)",
                env!("CARGO_PKG_VERSION")
            ))
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(Duration::from_secs(8))
            .build()
            .map_err(|_| "The walk could not start: no HTTP client.".to_owned())?;
        Ok(Self {
            client,
            cancel,
            min_delay: Duration::from_millis(
                req.delay_ms.unwrap_or(DEFAULT_DELAY_MS).max(MIN_DELAY_MS),
            ),
            start_host: start.host_str().unwrap_or("").to_ascii_lowercase(),
            same_host_only: req.same_host_only,
            hosts: HashMap::new(),
            requests: 0,
            began: Instant::now(),
        })
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn host_allowed(&self, u: &Url) -> bool {
        let host = u.host_str().unwrap_or("").to_ascii_lowercase();
        if self.same_host_only {
            return host == self.start_host;
        }
        host == self.start_host
            || self.hosts.len() < MAX_HOSTS
            || self.hosts.contains_key(&host_key(u))
    }

    /// Wait until a request to `u`'s host is allowed by the delay, then mark the time.
    async fn pace(&mut self, u: &Url) {
        let key = host_key(u);
        let delay = {
            let st = self.hosts.entry(key.clone()).or_insert(HostState {
                robots: None,
                last: None,
            });
            let site = st
                .robots
                .as_ref()
                .and_then(|r| r.crawl_delay)
                .unwrap_or_default();
            self.min_delay.max(site)
        };
        loop {
            let wait = self
                .hosts
                .get(&key)
                .and_then(|s| s.last)
                .map(|t| delay.saturating_sub(t.elapsed()))
                .unwrap_or_default();
            if wait.is_zero() || self.cancelled() {
                break;
            }
            tokio::time::sleep(wait.min(Duration::from_millis(100))).await;
        }
        if let Some(st) = self.hosts.get_mut(&key) {
            st.last = Some(Instant::now());
        }
        self.requests += 1;
    }

    /// One request with manual, checked redirects (same allowed hosts, robots honoured on every hop).
    async fn fetch(
        &mut self,
        method: Method,
        url: &Url,
        accept: Option<&str>,
        range0: bool,
    ) -> Fetch {
        let mut current = url.clone();
        for hop in 0..=MAX_REDIRECTS {
            if self.cancelled() {
                return Fetch::Failed;
            }
            if hop > 0 {
                if !self.host_allowed(&current) {
                    return Fetch::Refused(why::REDIRECT_AWAY);
                }
                if !self.robots_allow(&current).await {
                    return Fetch::Refused(why::ROBOTS);
                }
            }
            self.pace(&current).await;
            let mut rb = self.client.request(method.clone(), current.as_str());
            if let Some(a) = accept {
                rb = rb.header(ACCEPT, a);
            }
            if range0 {
                rb = rb.header(RANGE, "bytes=0-0");
            }
            let resp = match rb.send().await {
                Ok(r) => r,
                Err(_) => return Fetch::Failed,
            };
            if resp.status().is_redirection() && resp.status() != StatusCode::NOT_MODIFIED {
                let Some(next) = resp
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|l| current.join(l).ok())
                else {
                    return Fetch::Failed;
                };
                if !matches!(next.scheme(), "http" | "https")
                    || !next.username().is_empty()
                    || next.password().is_some()
                {
                    return Fetch::Refused(why::REDIRECT_AWAY);
                }
                current = next;
                continue;
            }
            return Fetch::Got(resp, current);
        }
        Fetch::Refused(why::REDIRECTS)
    }

    /// The robots rules of `u`'s host, fetched once.
    async fn robots_allow(&mut self, u: &Url) -> bool {
        let key = host_key(u);
        if self
            .hosts
            .get(&key)
            .and_then(|s| s.robots.as_ref())
            .is_none()
        {
            let robots = self.read_robots(u).await;
            let st = self.hosts.entry(key.clone()).or_insert(HostState {
                robots: None,
                last: None,
            });
            st.robots = Some(robots);
        }
        let path = match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_owned(),
        };
        self.hosts
            .get(&key)
            .and_then(|s| s.robots.as_ref())
            .is_none_or(|r| r.allowed(&path))
    }

    async fn read_robots(&mut self, u: &Url) -> Robots {
        let Ok(url) = u.join("/robots.txt") else {
            return Robots::allow_all();
        };
        // robots.txt itself is fetched with the same pacing; its own redirects stay on the host.
        let mut current = url;
        for _ in 0..=MAX_REDIRECTS {
            self.pace(&current).await;
            let resp = match self.client.get(current.as_str()).send().await {
                Ok(r) => r,
                Err(_) => return Robots::deny_everything(),
            };
            let status = resp.status();
            if status.is_redirection() {
                let next = resp
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|l| current.join(l).ok());
                match next {
                    Some(n) if n.host_str() == u.host_str() => {
                        current = n;
                        continue;
                    }
                    _ => return Robots::allow_all(),
                }
            }
            if status.is_success() {
                let text = read_capped(resp, PAGE_BYTES).await.unwrap_or_default();
                return Robots::parse(&text);
            }
            // 4xx: no robots.txt, everything allowed. 5xx: the site is not answering sensibly, allow nothing.
            return if status.is_server_error() {
                Robots::deny_everything()
            } else {
                Robots::allow_all()
            };
        }
        Robots::allow_all()
    }

    /// The size of a file and whether it is really a web page: `HEAD`, or a one-byte ranged `GET` when `HEAD` is
    /// refused by the server (many servers answer `HEAD` with 403, 405 or 501).
    async fn file_size(&mut self, url: &Url) -> Result<(Option<u64>, bool), &'static str> {
        for (method, range0) in [(Method::HEAD, false), (Method::GET, true)] {
            match self.fetch(method, url, None, range0).await {
                Fetch::Got(resp, _) => {
                    let status = resp.status();
                    if !status.is_success() {
                        if !range0 && status != StatusCode::NOT_FOUND && status != StatusCode::GONE
                        {
                            continue;
                        }
                        return Err(why::NO_ANSWER);
                    }
                    let html = resp
                        .headers()
                        .get(CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|t| t.to_ascii_lowercase().contains("text/html"));
                    let from_range = resp
                        .headers()
                        .get(CONTENT_RANGE)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.rsplit('/').next())
                        .and_then(|t| t.parse::<u64>().ok());
                    let total = from_range.or_else(|| {
                        if status == StatusCode::PARTIAL_CONTENT {
                            None
                        } else {
                            resp.headers()
                                .get(CONTENT_LENGTH)
                                .and_then(|v| v.to_str().ok())
                                .and_then(|v| v.parse().ok())
                        }
                    });
                    return Ok((total, html));
                }
                Fetch::Refused(r) => return Err(r),
                Fetch::Failed => {
                    if range0 {
                        return Err(why::NO_ANSWER);
                    }
                }
            }
        }
        Err(why::NO_ANSWER)
    }
}

async fn read_capped(mut resp: Response, cap: usize) -> Option<String> {
    let mut buf: Vec<u8> = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        buf.extend_from_slice(&chunk);
        if buf.len() >= cap {
            buf.truncate(cap);
            break;
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[derive(Default)]
struct Skips {
    counts: Vec<(String, u32)>,
    seen: HashSet<(String, String)>,
}

impl Skips {
    fn add(&mut self, reason: &str, key: &str) {
        if !self.seen.insert((reason.to_owned(), key.to_owned())) {
            return;
        }
        match self.counts.iter_mut().find(|(r, _)| r == reason) {
            Some((_, n)) => *n += 1,
            None => self.counts.push((reason.to_owned(), 1)),
        }
    }
}

/// Validate the start address of a request.
pub fn parse_start(raw: &str) -> Result<Url, String> {
    let u = Url::parse(raw.trim()).map_err(|_| "The start page is not an address.".to_owned())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("The start page must be an http or https address.".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("No user name or password in the address. Logged-in pages go through the Zeus extension.".into());
    }
    if u.host_str().is_none_or(str::is_empty) {
        return Err("The start page has no host.".into());
    }
    Ok(u)
}

/// Walk the site. Never panics; every failure is an `ok: false` preview with a sentence.
pub async fn walk(req: &SiteRequest, cancel: Arc<AtomicBool>) -> SitePreview {
    let start = match parse_start(&req.start_url) {
        Ok(u) => u,
        Err(e) => return SitePreview::fail(e),
    };
    let mut crawler = match Crawler::new(req, &start, cancel.clone()) {
        Ok(c) => c,
        Err(e) => return SitePreview::fail(e),
    };
    let depth_max = req.depth.min(4);
    let max_files = req.max_files.clamp(1, 500) as usize;
    let max_bytes = req.max_mib.clamp(1, 102_400) * 1024 * 1024;
    let max_pages = req.max_pages.unwrap_or(MAX_PAGES).clamp(1, MAX_PAGES) as usize;
    let types: Vec<String> = req
        .types
        .iter()
        .map(|t| t.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let has_include = req.include.iter().any(|p| !p.trim().is_empty());

    let mut out = SitePreview {
        ok: true,
        host: start.host_str().unwrap_or("").to_owned(),
        ..SitePreview::default()
    };
    let mut skips = Skips::default();
    let mut queue: VecDeque<(Url, u32)> = VecDeque::new();
    let mut seen_pages: HashSet<String> = HashSet::new();
    let mut seen_files: HashSet<String> = HashSet::new();

    let key = |u: &Url| {
        let mut k = u.clone();
        k.set_fragment(None);
        k.to_string()
    };
    queue.push_back((start.clone(), 0));
    seen_pages.insert(key(&start));

    // The start host's robots.txt comes first, so its facts are known even when the start page is refused.
    let start_allowed = crawler.robots_allow(&start).await;
    if let Some(r) = crawler
        .hosts
        .get(&host_key(&start))
        .and_then(|s| s.robots.as_ref())
    {
        out.robots = RobotsFacts {
            found: r.present || r.deny_all,
            rules: r.rule_count,
            crawl_delay_ms: r.crawl_delay.map(|d| d.as_millis() as u64),
        };
    }
    if !start_allowed {
        out.ok = false;
        out.error =
            Some("The site's robots.txt does not allow Zeus to read the start page.".into());
        out.requests = crawler.requests;
        return out;
    }

    while let Some((page, depth)) = queue.pop_front() {
        if crawler.cancelled() {
            return SitePreview::fail("The walk was cancelled.");
        }
        if crawler.began.elapsed() > WALK_BUDGET {
            skips.add(why::TIME, "walk");
            break;
        }
        if out.pages.len() >= max_pages {
            skips.add(why::PAGE_LIMIT, page.as_str());
            for (rest, _) in &queue {
                skips.add(why::PAGE_LIMIT, rest.as_str());
            }
            break;
        }
        if !crawler.robots_allow(&page).await {
            skips.add(why::ROBOTS, page.as_str());
            continue;
        }
        let (resp, final_url) = match crawler
            .fetch(
                Method::GET,
                &page,
                Some("text/html,application/xhtml+xml;q=0.9,*/*;q=0.5"),
                false,
            )
            .await
        {
            Fetch::Got(r, u) => (r, u),
            Fetch::Refused(r) => {
                skips.add(r, page.as_str());
                continue;
            }
            Fetch::Failed => {
                if crawler.cancelled() {
                    return SitePreview::fail("The walk was cancelled.");
                }
                skips.add(why::NO_ANSWER, page.as_str());
                continue;
            }
        };
        if !resp.status().is_success() {
            skips.add(why::NO_ANSWER, page.as_str());
            continue;
        }
        let is_html = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| {
                let t = t.to_ascii_lowercase();
                t.contains("text/html") || t.contains("application/xhtml")
            });
        if !is_html {
            skips.add(why::NOT_HTML, page.as_str());
            continue;
        }
        let body = read_capped(resp, PAGE_BYTES).await.unwrap_or_default();
        out.pages.push(PageRead {
            url: final_url.to_string(),
            depth,
        });
        let (links, base) = html::extract(&body);
        let base_url = base
            .and_then(|b| final_url.join(&b).ok())
            .unwrap_or_else(|| final_url.clone());

        for link in links {
            let Ok(mut target) = base_url.join(&link.href) else {
                continue;
            };
            if !matches!(target.scheme(), "http" | "https")
                || !target.username().is_empty()
                || target.password().is_some()
            {
                continue;
            }
            target.set_fragment(None);
            let ext = extension(target.path());
            let is_page = link.source != Source::Media
                && (link.source == Source::Frame || PAGE_EXTS.contains(&ext.as_str()));
            if is_page {
                if !crawler.host_allowed(&target) {
                    skips.add(why::OTHER_HOST, target.as_str());
                    continue;
                }
                if any_match(&req.exclude, target.path()) {
                    skips.add(why::EXCLUDE_PAGE, target.as_str());
                    continue;
                }
                if seen_pages.contains(&key(&target)) {
                    continue;
                }
                if depth + 1 > depth_max {
                    skips.add(why::DEPTH, target.as_str());
                    continue;
                }
                if !crawler.robots_allow(&target).await {
                    skips.add(why::ROBOTS, target.as_str());
                    continue;
                }
                seen_pages.insert(key(&target));
                queue.push_back((target, depth + 1));
                continue;
            }
            // A file.
            if !crawler.host_allowed(&target) {
                skips.add(why::FILE_OTHER_HOST, target.as_str());
                continue;
            }
            if !types.is_empty() && !types.contains(&ext) {
                skips.add(why::TYPE, target.as_str());
                continue;
            }
            if has_include && !any_match(&req.include, target.path()) {
                skips.add(why::INCLUDE, target.as_str());
                continue;
            }
            if any_match(&req.exclude, target.path()) {
                skips.add(why::EXCLUDE_FILE, target.as_str());
                continue;
            }
            if !seen_files.insert(key(&target)) {
                skips.add(why::DUPLICATE, target.as_str());
                continue;
            }
            if out.files.len() >= max_files {
                skips.add(why::FILE_LIMIT, target.as_str());
                continue;
            }
            if !crawler.robots_allow(&target).await {
                skips.add(why::ROBOTS, target.as_str());
                continue;
            }
            if crawler.cancelled() {
                return SitePreview::fail("The walk was cancelled.");
            }
            let size = match crawler.file_size(&target).await {
                Ok((_, true)) => {
                    skips.add(why::IS_A_PAGE, target.as_str());
                    continue;
                }
                Ok((size, false)) => size,
                Err(r) => {
                    skips.add(r, target.as_str());
                    continue;
                }
            };
            let bytes = size.unwrap_or(0);
            if size.is_none() {
                out.unknown_sizes += 1;
            }
            if out.total_bytes + bytes > max_bytes {
                skips.add(why::SIZE_LIMIT, target.as_str());
                continue;
            }
            out.total_bytes += bytes;
            out.files.push(FileFound {
                url: target.to_string(),
                name: file_name(&target),
                size: bytes,
                depth,
                from: final_url.to_string(),
                host: target.host_str().unwrap_or("").to_owned(),
            });
        }
    }
    out.skipped = skips
        .counts
        .into_iter()
        .map(|(reason, count)| Skipped { reason, count })
        .collect();
    out.requests = crawler.requests;
    out.elapsed_ms = crawler.began.elapsed().as_millis() as u64;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_and_names() {
        assert_eq!(extension("/a/b.tar.gz"), "gz");
        assert_eq!(extension("/a/b/"), "");
        assert_eq!(extension("/a/.hidden"), "");
        assert_eq!(extension("/a/Report.PDF"), "pdf");
        assert_eq!(
            file_name(&Url::parse("http://x/a/Quarterly%20Report.pdf?x=1").unwrap()),
            "Quarterly Report.pdf"
        );
        assert_eq!(file_name(&Url::parse("http://x/").unwrap()), "index");
    }

    #[test]
    fn start_addresses_are_checked() {
        assert!(parse_start("https://example.org/a").is_ok());
        assert!(parse_start("ftp://example.org/a").is_err());
        assert!(
            parse_start("https://u:p@example.org/a")
                .unwrap_err()
                .contains("user name")
        );
        assert!(parse_start("nonsense").is_err());
    }
}
