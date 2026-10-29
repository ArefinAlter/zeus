//! Media acceptance against hermetic HTTP origins: actual requests, bytes,
//! receipts, interruption, exact ranges, recovery and fail-closed behavior.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use url::Url;
use zeus_app::home::AppHome;
use zeus_app_grab::media::service::Service;
use zeus_app_grab::media::{dash, hls};

#[derive(Clone, Default)]
struct Fixture {
    requests: Arc<Mutex<Vec<RequestRecord>>>,
    changed: Arc<AtomicBool>,
}

type RequestRecord = (String, String, Option<String>);

fn resource(body: &'static [u8], request: &Request, fixture: &Fixture) -> Response {
    let etag = if fixture.changed.load(Ordering::Acquire) {
        "\"v2\""
    } else {
        "\"v1\""
    };
    if request
        .headers()
        .get(header::IF_MATCH)
        .is_some_and(|value| value != etag)
    {
        return StatusCode::PRECONDITION_FAILED.into_response();
    }
    let mut headers = HeaderMap::from_iter([
        (header::ETAG, HeaderValue::from_static(etag)),
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        ),
    ]);
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    let (status, bytes) = match range {
        Some(range) => {
            let (start, end) = range
                .strip_prefix("bytes=")
                .unwrap()
                .split_once('-')
                .unwrap();
            let (start, end) = (
                start.parse::<usize>().unwrap(),
                end.parse::<usize>().unwrap(),
            );
            if end >= body.len() || end < start {
                return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            }
            headers.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{end}/{}", body.len())).unwrap(),
            );
            (StatusCode::PARTIAL_CONTENT, &body[start..=end])
        }
        None => (StatusCode::OK, body),
    };
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string()).unwrap(),
    );
    if request.method() == Method::HEAD {
        (status, headers).into_response()
    } else {
        (status, headers, bytes).into_response()
    }
}

async fn handle(State(fixture): State<Fixture>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    fixture.requests.lock().unwrap().push((
        request.method().to_string(),
        path.clone(),
        request
            .headers()
            .get(header::RANGE)
            .map(|h| h.to_str().unwrap().to_owned()),
    ));
    let playlist = |body: &'static str| {
        (
            [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
            body,
        )
            .into_response()
    };
    match path.as_str() {
        "/master.m3u8" => playlist("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000,RESOLUTION=1280x720\nfinite.m3u8\n"),
        "/finite.m3u8" => playlist("#EXTM3U\n#EXT-X-MAP:URI=\"blob\",BYTERANGE=\"4@0\"\n#EXTINF:1,\n#EXT-X-BYTERANGE:4@4\nblob\n#EXTINF:1,\n#EXT-X-BYTERANGE:4\nblob\n#EXT-X-ENDLIST\n"),
        "/cancel.m3u8" => playlist("#EXTM3U\n#EXTINF:1,\nfast.ts\n#EXTINF:1,\nslow.ts\n#EXT-X-ENDLIST\n"),
        "/bad-range.m3u8" => playlist("#EXTM3U\n#EXTINF:1,\n#EXT-X-BYTERANGE:4@4\nignore-range\n#EXT-X-ENDLIST\n"),
        "/expired.m3u8" => playlist("#EXTM3U\n#EXTINF:1,\nexpired.ts\n#EXT-X-ENDLIST\n"),
        "/redirect.m3u8" => (StatusCode::FOUND, [(header::LOCATION, "/finite.m3u8")]).into_response(),
        "/foreign.m3u8" => playlist("#EXTM3U\n#EXTINF:1,\nhttps://foreign.invalid/media.ts\n#EXT-X-ENDLIST\n"),
        "/dash.mpd" => ([(header::CONTENT_TYPE, "application/dash+xml")], "<MPD type=\"static\" mediaPresentationDuration=\"PT2S\"><Period><AdaptationSet mimeType=\"video/mp4\"><SegmentTemplate initialization=\"init.mp4\" media=\"part-$Number$.m4s\" duration=\"1\"/><Representation id=\"v\" height=\"720\" bandwidth=\"1000\"/></AdaptationSet></Period></MPD>").into_response(),
        "/page" => ([(header::CONTENT_TYPE, "text/html")], "<html><video src='master.m3u8'></video><script>fake='secret.m3u8'</script></html>").into_response(),
        "/blob" => resource(b"INITAAAABBBB", &request, &fixture),
        "/init.mp4" => resource(b"INIT", &request, &fixture),
        "/part-1.m4s" | "/fast.ts" => resource(b"AAAA", &request, &fixture),
        "/part-2.m4s" => resource(b"BBBB", &request, &fixture),
        "/slow.ts" => { tokio::time::sleep(Duration::from_millis(500)).await; resource(b"BBBB", &request, &fixture) }
        "/direct.mp4" => {
            let mut response = resource(b"DIRECT-MEDIA", &request, &fixture);
            response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
            response
        }
        "/ignore-range" => ([(header::CONTENT_TYPE, "application/octet-stream")], b"INITAAAABBBB".as_slice()).into_response(),
        "/expired.ts" => StatusCode::FORBIDDEN.into_response(),
        "/login.mp4" if request.method() == Method::HEAD => ([(header::CONTENT_TYPE, "video/mp4")], "").into_response(),
        "/login.mp4" => ([(header::CONTENT_TYPE, "Text/Html")], "<html>sign in first</html>").into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve() -> (String, Fixture, tokio::task::JoinHandle<()>) {
    let fixture = Fixture::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(handle).with_state(fixture.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, fixture, handle)
}

fn service() -> (TempDir, AppHome, Arc<Service>) {
    let temporary = TempDir::new().unwrap();
    let home = AppHome::at(temporary.path().join("profile"));
    home.create().unwrap();
    let settings = zeus_app::engine_config::load(&home).unwrap();
    zeus_app::engine_config::save(&home, &settings).unwrap();
    let service = Service::new(home.clone());
    (temporary, home, service)
}

async fn plan(service: &Service, url: &str) -> Value {
    let inspection = service.inspect(&json!({"url": url})).await.unwrap();
    service
        .select(&json!({"inspection_id": inspection["inspection_id"], "format_id": "0"}))
        .await
        .unwrap()
}

fn start_body(plan: &Value, filename: &str, key: &str) -> Value {
    json!({"plan_id": plan["plan_id"], "root_index": 0, "filename": filename,
        "max_bytes": 1024 * 1024, "connections": 2, "idempotency_key": key})
}

async fn wait_task(service: &Service, id: &Value, predicate: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let list = service.list().unwrap();
            let task = list["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|task| &task["task_id"] == id)
                .unwrap()
                .clone();
            if predicate(&task) {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn finished(service: &Service, id: &Value) -> Value {
    wait_task(service, id, |task| {
        !matches!(
            task["state"].as_str().unwrap(),
            "downloading" | "assembling"
        )
    })
    .await
}

#[test]
fn hls_parser_handles_audio_variants_and_rejects_unsafe_or_unbounded_tracks() {
    let base = Url::parse("https://media.invalid/a/master.m3u8").unwrap();
    let master = hls::parse(&base, "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"English, stereo\",URI=\"audio.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=12000,RESOLUTION=1920x1080,AUDIO=\"audio\"\nvideo.m3u8\n").unwrap();
    let hls::Playlist::Master(variants) = master else {
        panic!("master expected")
    };
    assert_eq!(variants.len(), 2);
    assert_eq!(variants[1].facts.height, Some(1080));
    assert!(!variants[1].facts.notices.is_empty());
    for invalid in [
        "#EXTM3U\n#EXTINF:1,\na.ts\n",
        "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:1,\na.ts\n#EXT-X-ENDLIST\n",
        "#EXTM3U\n#EXTINF:1,\n#EXT-X-BYTERANGE:4\na.ts\n#EXT-X-ENDLIST\n",
        "#EXTM3U\n#EXT-X-DISCONTINUITY\n#EXTINF:1,\na.ts\n#EXT-X-ENDLIST\n",
        "#EXTM3U\n#EXTINF:1,\nfile:///etc/passwd\n#EXT-X-ENDLIST\n",
    ] {
        assert!(hls::parse(&base, invalid).is_err());
    }
}

#[test]
fn dash_parser_expands_lists_templates_and_finite_timelines_without_drm_or_xml_entities() {
    let base = Url::parse("https://media.invalid/stream/master.mpd").unwrap();
    let tracks = dash::parse(&base, "<MPD mediaPresentationDuration='PT3S'><Period><AdaptationSet mimeType='video/mp4'><SegmentTemplate initialization='init-$RepresentationID$.mp4' media='$Number%03d$-$Time$.m4s' startNumber='4'><SegmentTimeline><S t='2' d='2' r='1'/></SegmentTimeline></SegmentTemplate><Representation id='hd' bandwidth='1000' height='720'/></AdaptationSet></Period></MPD>").unwrap();
    assert_eq!(tracks[0].segments.len(), 3);
    assert!(tracks[0].segments[0].url.path().ends_with("init-hd.mp4"));
    assert!(tracks[0].segments[1].url.path().ends_with("004-2.m4s"));
    assert!(tracks[0].segments[2].url.path().ends_with("005-4.m4s"));
    let list = dash::parse(&base, "<MPD><Period><AdaptationSet mimeType='audio/mp4'><Representation id='audio'><BaseURL>audio.mp4?a=1&amp;b=2</BaseURL><SegmentList><Initialization range='0-3'/><SegmentURL media='audio.mp4' mediaRange='4-7'/><SegmentURL media='audio.mp4' mediaRange='8-11'/></SegmentList></Representation></AdaptationSet></Period></MPD>").unwrap();
    assert_eq!(list[0].segments.len(), 3);
    assert_eq!(list[0].container, "m4a");
    assert_eq!(list[0].segments[0].range.as_ref().unwrap().end, 4);
    assert_eq!(list[0].segments[0].url.query(), Some("a=1&b=2"));
    for invalid in [
        "<MPD type='dynamic'><Period/></MPD>",
        "<MPD><Period/><Period/></MPD>",
        "<MPD><Period><AdaptationSet><ContentProtection schemeIdUri='x'/></AdaptationSet></Period></MPD>",
        "<!DOCTYPE MPD [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><MPD><Period/></MPD>",
        "<MPD><Period><AdaptationSet><SegmentTemplate media='$Number$.m4s'><SegmentTimeline><S d='1' r='-1'/></SegmentTimeline></SegmentTemplate><Representation id='a'/></AdaptationSet></Period></MPD>",
        "<MPD><Period><AdaptationSet><SegmentTemplate media='$Number%099999d$.m4s'><SegmentTimeline><S d='1'/></SegmentTimeline></SegmentTemplate><Representation id='a'/></AdaptationSet></Period></MPD>",
    ] {
        assert!(dash::parse(&base, invalid).is_err());
    }
}

#[tokio::test]
async fn hls_ranges_and_dash_tracks_execute_and_verify_exact_ordered_output() {
    let (base, fixture, server) = serve().await;
    let (_temporary, home, service) = service();
    for (path, filename, key) in [
        ("/master.m3u8", "hls.mp4", "hls-request-00000001"),
        ("/dash.mpd", "dash.mp4", "dash-request-0000001"),
        ("/redirect.m3u8", "redirect.mp4", "redirect-request-001"),
        ("/direct.mp4", "direct.mp4", "direct-request-0001"),
    ] {
        let plan = plan(&service, &format!("{base}{path}?token=ZEUS-CANARY-MEDIA")).await;
        assert!(
            !serde_json::to_string(&plan)
                .unwrap()
                .contains("ZEUS-CANARY-")
        );
        let body = start_body(&plan, filename, key);
        let task = service.start(&body).await.unwrap();
        let duplicate = service.start(&body).await.unwrap();
        assert_eq!(task["task_id"], duplicate["task_id"]);
        let mut changed = body.clone();
        changed["connections"] = json!(1);
        assert_eq!(
            service.start(&changed).await.unwrap_err().code,
            "media.idempotency_conflict"
        );
        let done = finished(&service, &task["task_id"]).await;
        assert_eq!(done["state"], "complete", "{done}");
        let expected: &[u8] = if path == "/direct.mp4" {
            b"DIRECT-MEDIA"
        } else {
            b"INITAAAABBBB"
        };
        let bytes = std::fs::read(home.default_download_root().join(filename)).unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(done["sha256"], hex::encode(Sha256::digest(expected)));
        let checkpoint = std::fs::read_to_string(
            home.app_dir()
                .join("media")
                .join(task["task_id"].as_str().unwrap())
                .join("checkpoint.json"),
        )
        .unwrap();
        assert!(!checkpoint.contains("ZEUS-CANARY-"));
        service
            .archive(&json!({"task_id": task["task_id"]}))
            .unwrap();
        assert_eq!(
            std::fs::read(home.default_download_root().join(filename)).unwrap(),
            expected
        );
    }
    let log = fixture.requests.lock().unwrap();
    assert!(
        log.iter()
            .any(|(_, path, range)| path == "/blob" && range.as_deref() == Some("bytes=0-3"))
    );
    assert!(
        log.iter()
            .any(|(_, path, range)| path == "/blob" && range.as_deref() == Some("bytes=8-11"))
    );
    assert!(
        !serde_json::to_string(&service.list().unwrap())
            .unwrap()
            .contains("ZEUS-CANARY-")
    );
    server.abort();
}

#[tokio::test]
async fn cancellation_restart_and_strong_validator_resume_keep_only_verified_segments() {
    let (base, fixture, server) = serve().await;
    let (_temporary, home, service) = service();
    let selected = plan(&service, &format!("{base}/cancel.m3u8")).await;
    let task = service
        .start(&start_body(&selected, "resume.ts", "cancel-request-0001"))
        .await
        .unwrap();
    wait_task(&service, &task["task_id"], |task| {
        task["completed_segments"] == 1
    })
    .await;
    service
        .cancel(&json!({"task_id": task["task_id"]}))
        .unwrap();
    let canceled = finished(&service, &task["task_id"]).await;
    assert_eq!(canceled["state"], "canceled");
    assert_eq!(canceled["completed_segments"], 1);
    service.shutdown().await;
    let recovered = Service::new(home.clone());
    let fresh = plan(&recovered, &format!("{base}/cancel.m3u8")).await;
    let mut request = start_body(&fresh, "resume.ts", "resume-request-0001");
    request["resume_task_id"] = task["task_id"].clone();
    recovered.start(&request).await.unwrap();
    let done = finished(&recovered, &task["task_id"]).await;
    assert_eq!(done["state"], "complete", "{done}");
    assert_eq!(
        std::fs::read(home.default_download_root().join("resume.ts")).unwrap(),
        b"AAAABBBB"
    );
    let log = fixture.requests.lock().unwrap();
    assert_eq!(
        log.iter()
            .filter(|(method, path, _)| method == "GET" && path == "/fast.ts")
            .count(),
        1
    );
    assert_eq!(
        log.iter()
            .filter(|(method, path, _)| method == "HEAD" && path == "/fast.ts")
            .count(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn invalid_ranges_expired_sources_scopes_and_collisions_fail_without_outputs() {
    let (base, _, server) = serve().await;
    let (_temporary, home, service) = service();
    for (path, code, key) in [
        (
            "bad-range.m3u8",
            "media.range_ignored",
            "bad-range-request-01",
        ),
        (
            "expired.m3u8",
            "media.auth_or_expired",
            "expired-request-001",
        ),
        (
            "login.mp4",
            "media.unexpected_content",
            "login-request-00001",
        ),
    ] {
        let selected = plan(&service, &format!("{base}/{path}")).await;
        let filename = format!(
            "refused.{}",
            selected["format"]["container"].as_str().unwrap()
        );
        let task = service
            .start(&start_body(&selected, &filename, key))
            .await
            .unwrap();
        let result = finished(&service, &task["task_id"]).await;
        assert_eq!(result["state"], "failed");
        assert_eq!(result["error_code"], code);
        assert!(!home.default_download_root().join(&filename).exists());
    }
    let inspection = service
        .inspect(&json!({"url": format!("{base}/foreign.m3u8")}))
        .await
        .unwrap();
    let refusal = service
        .select(&json!({"inspection_id": inspection["inspection_id"], "format_id": "0"}))
        .await
        .unwrap_err();
    assert_eq!(refusal.code, "media.origin_not_allowed");
    let selected = plan(&service, &format!("{base}/direct.mp4")).await;
    std::fs::write(home.default_download_root().join("exists.mp4"), b"keep me").unwrap();
    let refusal = service
        .start(&start_body(
            &selected,
            "exists.mp4",
            "collision-request-001",
        ))
        .await
        .unwrap_err();
    assert_eq!(refusal.code, "media.destination_exists");
    assert_eq!(
        std::fs::read(home.default_download_root().join("exists.mp4")).unwrap(),
        b"keep me"
    );
    let refusal = service
        .start(&start_body(
            &selected,
            "../escape.mp4",
            "escape-request-0001",
        ))
        .await
        .unwrap_err();
    assert_eq!(refusal.code, "api.invalid_params");
    server.abort();
}

#[tokio::test]
async fn page_discovery_does_not_execute_scripts_and_requires_actual_format_review() {
    let (base, fixture, server) = serve().await;
    let (_temporary, _, service) = service();
    let inspection = service
        .inspect(&json!({"url": format!("{base}/page")}))
        .await
        .unwrap();
    assert_eq!(inspection["formats"].as_array().unwrap().len(), 1);
    let nested = service
        .select(&json!({"inspection_id": inspection["inspection_id"], "format_id": "0"}))
        .await
        .unwrap();
    assert_eq!(nested["needs_selection"], true);
    let selected = service
        .select(&json!({"inspection_id": nested["inspection_id"], "format_id": "0"}))
        .await
        .unwrap();
    assert!(selected.get("plan_id").is_some());
    assert!(
        service.list().unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, path, _)| path.contains("secret"))
    );
    server.abort();
}

#[tokio::test]
async fn complete_receipts_recover_a_rename_checkpoint_crash_without_any_network() {
    let (base, _, server) = serve().await;
    let (_temporary, home, service) = service();
    let selected = plan(&service, &format!("{base}/finite.m3u8")).await;
    let task = service
        .start(&start_body(
            &selected,
            "recovery.mp4",
            "recover-request-001",
        ))
        .await
        .unwrap();
    let done = finished(&service, &task["task_id"]).await;
    assert_eq!(done["state"], "complete");
    service.shutdown().await;
    let path = home
        .app_dir()
        .join("media")
        .join(task["task_id"].as_str().unwrap())
        .join("checkpoint.json");
    let mut checkpoint: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    // Model the durable state immediately before the process died after rename.
    checkpoint["state"] = json!("assembling");
    checkpoint["output_bytes"] = Value::Null;
    checkpoint["sha256"] = Value::Null;
    std::fs::write(&path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    server.abort();
    let recovered = Service::new(home.clone());
    assert_eq!(
        recovered.list().unwrap()["items"][0]["state"],
        "interrupted"
    );
    recovered
        .recover(&json!({"task_id": task["task_id"]}))
        .unwrap();
    let done = finished(&recovered, &task["task_id"]).await;
    assert_eq!(done["state"], "complete", "{done}");
    assert_eq!(
        std::fs::read(home.default_download_root().join("recovery.mp4")).unwrap(),
        b"INITAAAABBBB"
    );
    // A later replacement is not accepted or overwritten by recovery.
    std::fs::write(
        home.default_download_root().join("recovery.mp4"),
        b"different output",
    )
    .unwrap();
    recovered
        .recover(&json!({"task_id": task["task_id"]}))
        .unwrap();
    let refused = finished(&recovered, &task["task_id"]).await;
    assert_eq!(refused["error_code"], "media.destination_exists");
    assert_eq!(
        std::fs::read(home.default_download_root().join("recovery.mp4")).unwrap(),
        b"different output"
    );
}

#[tokio::test]
async fn changed_origin_validators_refuse_resume_and_corrupt_local_segments_are_refetched() {
    let (base, fixture, server) = serve().await;
    let (_temporary, home, service) = service();
    let selected = plan(&service, &format!("{base}/cancel.m3u8")).await;
    let task = service
        .start(&start_body(&selected, "changed.ts", "changed-request-001"))
        .await
        .unwrap();
    wait_task(&service, &task["task_id"], |task| {
        task["completed_segments"] == 1
    })
    .await;
    service
        .cancel(&json!({"task_id": task["task_id"]}))
        .unwrap();
    finished(&service, &task["task_id"]).await;
    fixture.changed.store(true, Ordering::Release);
    let mut resume = start_body(&selected, "changed.ts", "resume-changed-0001");
    resume["resume_task_id"] = task["task_id"].clone();
    service.start(&resume).await.unwrap();
    let refused = finished(&service, &task["task_id"]).await;
    assert_eq!(refused["error_code"], "media.source_changed");
    assert!(!home.default_download_root().join("changed.ts").exists());
    fixture.changed.store(false, Ordering::Release);
    let segment = home
        .app_dir()
        .join("media")
        .join(task["task_id"].as_str().unwrap())
        .join("segment-0000.bin");
    std::fs::write(segment, b"xxxx").unwrap();
    service.start(&resume).await.unwrap();
    let done = finished(&service, &task["task_id"]).await;
    assert_eq!(done["state"], "complete", "{done}");
    assert_eq!(
        std::fs::read(home.default_download_root().join("changed.ts")).unwrap(),
        b"AAAABBBB"
    );
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, path, _)| method == "GET" && path == "/fast.ts")
            .count(),
        2
    );
    server.abort();
}

#[tokio::test]
async fn a_hard_receive_ceiling_prevents_assembly_and_private_corruption_is_reported() {
    let (base, _, server) = serve().await;
    let (_temporary, home, service) = service();
    let selected = plan(&service, &format!("{base}/finite.m3u8")).await;
    let mut request = start_body(&selected, "limited.mp4", "limit-request-0001");
    request["max_bytes"] = json!(11);
    let task = service.start(&request).await.unwrap();
    let refused = finished(&service, &task["task_id"]).await;
    assert_eq!(refused["error_code"], "media.byte_limit");
    assert!(refused["received_bytes"].as_u64().unwrap() <= 11);
    assert!(!home.default_download_root().join("limited.mp4").exists());
    service.shutdown().await;
    let path = home
        .app_dir()
        .join("media")
        .join(task["task_id"].as_str().unwrap())
        .join("checkpoint.json");
    std::fs::write(&path, b"not json").unwrap();
    let recovered = Service::new(home);
    assert_eq!(recovered.list().unwrap()["unreadable_checkpoints"], 1);
    let selected = plan(&recovered, &format!("{base}/finite.m3u8")).await;
    let refusal = recovered
        .start(&start_body(&selected, "limited.mp4", "limit-request-0001"))
        .await
        .unwrap_err();
    assert_eq!(refusal.code, "media.checkpoint_unavailable");
    assert_eq!(std::fs::read(path).unwrap(), b"not json");
    server.abort();
}

#[tokio::test]
async fn a_changed_destination_configuration_invalidates_the_review() {
    let (base, _, server) = serve().await;
    let (_temporary, home, service) = service();
    let selected = plan(&service, &format!("{base}/direct.mp4")).await;
    let mut settings = zeus_app::engine_config::load(&home).unwrap();
    let replacement = home.root.join("replacement-root");
    settings.write_roots = vec![replacement.display().to_string()];
    zeus_app::engine_config::save(&home, &settings).unwrap();
    let refusal = service
        .start(&start_body(
            &selected,
            "review.mp4",
            "root-change-request-01",
        ))
        .await
        .unwrap_err();
    assert_eq!(refusal.code, "media.review_changed");
    assert!(!replacement.join("review.mp4").exists());
    assert!(
        service.list().unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    server.abort();
}
