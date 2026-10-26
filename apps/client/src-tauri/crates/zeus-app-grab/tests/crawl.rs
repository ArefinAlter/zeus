//! The site grabber's walk against a local site: robots.txt, crawl delay, depth, filters, loops, other hosts and
//! non-HTML answers. Loopback only.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use zeus_app_grab::crawl::{SitePreview, SiteRequest, walk};

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<(String, String, Instant)>>>);

fn page(links: &[&str]) -> Response {
    let body = links
        .iter()
        .map(|l| format!("<a href=\"{l}\">{l}</a>"))
        .collect::<Vec<_>>()
        .join("\n");
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        format!("<!doctype html><title>t</title>{body}"),
    )
        .into_response()
}

fn file(size: usize, head: bool) -> Response {
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size.to_string()).unwrap(),
    );
    if head {
        (StatusCode::OK, h).into_response()
    } else {
        (StatusCode::OK, h, vec![0u8; size]).into_response()
    }
}

async fn handle(State(log): State<Log>, req: Request) -> Response {
    let path = req.uri().path().to_owned();
    let ua = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    log.0.lock().unwrap().push((
        req.method().to_string(),
        format!("{path}|{ua}"),
        Instant::now(),
    ));
    let head = req.method() == Method::HEAD;
    match path.as_str() {
        "/robots.txt" => "User-agent: *\nDisallow: /private/\nCrawl-delay: 1\n".into_response(),
        "/" => page(&[
            "guide/",
            "/private/secret.pdf",
            "files/a.pdf",
            "files/b.zip",
            "blog/",
            "https://elsewhere.example.org/x.zip",
            "binary",
            "loop/",
            "missing/",
            "files/a.pdf#again",
        ]),
        "/guide/" => page(&["/", "ch1/", "/files/guide.pdf", "/files/notes.txt"]),
        "/guide/ch1/" => page(&["/files/ch1.pdf", "deep/"]),
        "/guide/ch1/deep/" => page(&["/files/deep.zip"]),
        "/loop/" => page(&["/loop/", "/", "/loop/?again=1"]),
        "/blog/" => page(&["/files/blog.zip"]),
        "/binary" => ([(header::CONTENT_TYPE, "application/pdf")], vec![0u8; 10]).into_response(),
        "/missing/" => StatusCode::NOT_FOUND.into_response(),
        p if p.starts_with("/files/") => file(1000 + p.len() * 10, head),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve() -> (String, Log) {
    let log = Log::default();
    let app = Router::new().fallback(handle).with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), log)
}

fn request(base: &str) -> SiteRequest {
    SiteRequest {
        start_url: format!("{base}/"),
        depth: 2,
        delay_ms: Some(250),
        max_files: 50,
        max_mib: 100,
        ..SiteRequest::default()
    }
}

fn names(p: &SitePreview) -> Vec<String> {
    let mut n: Vec<String> = p.files.iter().map(|f| f.name.clone()).collect();
    n.sort();
    n
}

fn skipped(p: &SitePreview, reason: &str) -> u32 {
    p.skipped
        .iter()
        .find(|s| s.reason == reason)
        .map_or(0, |s| s.count)
}

#[tokio::test]
async fn walks_the_site_politely_and_lists_exactly_the_allowed_files() {
    let (base, log) = serve().await;
    let started = Instant::now();
    let preview = walk(&request(&base), Arc::new(AtomicBool::new(false))).await;
    assert!(preview.ok, "{:?}", preview.error);
    // Depth 2: /, /guide/, /guide/ch1/, /blog/, /loop/ are read; /guide/ch1/deep/ is deeper than the limit.
    let pages: Vec<&str> = preview
        .pages
        .iter()
        .map(|p| p.url.trim_start_matches(&base))
        .collect();
    assert!(
        pages.contains(&"/")
            && pages.contains(&"/guide/")
            && pages.contains(&"/guide/ch1/")
            && pages.contains(&"/blog/")
            && pages.contains(&"/loop/"),
        "{pages:?}"
    );
    assert!(!pages.contains(&"/guide/ch1/deep/"));
    assert_eq!(
        names(&preview),
        [
            "a.pdf",
            "b.zip",
            "blog.zip",
            "ch1.pdf",
            "guide.pdf",
            "notes.txt"
        ]
    );
    assert!(preview.total_bytes > 0 && preview.files.iter().all(|f| f.size > 0));
    // The reasons it left things out.
    assert_eq!(
        skipped(&preview, "blocked by robots.txt"),
        1,
        "{:?}",
        preview.skipped
    );
    assert_eq!(skipped(&preview, "deeper than the depth limit"), 1);
    assert_eq!(skipped(&preview, "file on another host"), 1);
    assert_eq!(skipped(&preview, "not an HTML page"), 1);
    assert_eq!(skipped(&preview, "page did not answer"), 1);
    assert_eq!(
        skipped(&preview, "duplicate"),
        1,
        "the #fragment link is the same file"
    );
    assert!(
        preview.robots.found
            && preview.robots.crawl_delay_ms == Some(1000)
            && preview.robots.rules == 1
    );
    // Never the private path, never the other host, and requests spaced by the site's Crawl-delay.
    let seen = log.0.lock().unwrap().clone();
    assert!(
        seen.iter().all(|(_, p, _)| !p.starts_with("/private/")),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .all(|(_, p, _)| p.contains("Zeus/") && p.contains("site grabber")),
        "user agent"
    );
    assert!(
        seen[0].1.starts_with("/robots.txt"),
        "robots.txt is read first"
    );
    for w in seen.windows(2) {
        let gap = w[1].2.duration_since(w[0].2);
        assert!(
            gap >= Duration::from_millis(950),
            "requests {} and {} were {gap:?} apart",
            w[0].1,
            w[1].1
        );
    }
    assert!(started.elapsed() >= Duration::from_secs(seen.len() as u64 - 1));
    assert_eq!(preview.requests as usize, seen.len());
}

#[tokio::test]
async fn filters_depth_and_limits_are_honoured() {
    let (base, log) = serve().await;
    let mut req = request(&base);
    req.depth = 0;
    req.types = vec!["pdf".into()];
    let p = walk(&req, Arc::new(AtomicBool::new(false))).await;
    assert!(p.ok);
    assert_eq!(names(&p), ["a.pdf"]);
    assert_eq!(p.pages.len(), 1);
    assert!(skipped(&p, "file type not selected") >= 1);

    let mut req = request(&base);
    req.depth = 1;
    req.exclude = vec!["*/blog/*".into(), "notes".into()];
    req.include = vec!["/files/*".into()];
    req.max_files = 2;
    let p = walk(&req, Arc::new(AtomicBool::new(false))).await;
    assert!(p.ok);
    assert_eq!(p.files.len(), 2);
    assert!(skipped(&p, "over the file limit") >= 1);
    assert!(
        p.files
            .iter()
            .all(|f| !f.url.contains("blog") && !f.url.contains("notes"))
    );
    assert!(
        !log.0
            .lock()
            .unwrap()
            .iter()
            .any(|(_, p, _)| p.starts_with("/blog/")),
        "an excluded page is never read"
    );
}

#[tokio::test]
async fn bad_start_addresses_and_cancellation() {
    let (base, _) = serve().await;
    let mut req = request(&base);
    req.start_url = "ftp://example.org/".into();
    assert!(!walk(&req, Arc::new(AtomicBool::new(false))).await.ok);
    req.start_url = "http://user:pw@127.0.0.1/".into();
    let p = walk(&req, Arc::new(AtomicBool::new(false))).await;
    assert!(!p.ok && p.error.unwrap().contains("user name"));
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let req = request(&base);
    let task = tokio::spawn(async move { walk(&req, c2).await });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    let p = task.await.unwrap();
    assert!(!p.ok && p.error.unwrap().contains("cancelled"));
}

#[tokio::test]
async fn a_site_whose_robots_forbids_everything_is_refused() {
    async fn deny(req: Request) -> Response {
        if req.uri().path() == "/robots.txt" {
            "User-agent: Zeus\nDisallow: /\n".into_response()
        } else {
            page(&["a.pdf"])
        }
    }
    let app = Router::new().fallback(deny);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let p = walk(
        &request(&format!("http://{addr}")),
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    assert!(!p.ok);
    assert!(p.error.unwrap().contains("robots.txt"));
}
