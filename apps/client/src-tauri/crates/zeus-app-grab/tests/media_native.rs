//! Explicit opt-in acceptance with a verified, isolated FFmpeg installation.
//! Ordinary tests do not acquire helpers or read these operator-supplied paths.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeus_app::home::AppHome;
use zeus_app_grab::media::service::Service;

async fn handle(State(directory): State<Arc<PathBuf>>, request: Request) -> Response {
    let (name, mime) = match request.uri().path() {
        "/video.mp4" => ("video.mp4", "video/mp4"),
        "/audio.m4a" => ("audio.m4a", "audio/mp4"),
        "/combined.ts" => ("combined.ts", "video/mp2t"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let bytes = std::fs::read(directory.join(name)).unwrap();
    let headers = [
        (header::CONTENT_TYPE, mime.to_owned()),
        (header::CONTENT_LENGTH, bytes.len().to_string()),
        (
            header::ETAG,
            format!("\"{}\"", hex::encode(Sha256::digest(&bytes))),
        ),
    ];
    if request.method() == Method::HEAD {
        headers.into_response()
    } else {
        (headers, bytes).into_response()
    }
}

async fn done(service: &Service, id: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let items = service.list().unwrap();
            let task = items["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|task| &task["task_id"] == id)
                .unwrap()
                .clone();
            if !matches!(
                task["state"].as_str().unwrap(),
                "downloading" | "assembling" | "converting"
            ) {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

async fn acquire(service: &Arc<Service>, base: &str, filename: &str, key: &str) -> Value {
    let inspection = service
        .inspect(&json!({"url": format!("{base}/{filename}")}))
        .await
        .unwrap();
    let plan = service
        .select(&json!({"inspection_id": inspection["inspection_id"], "format_id": "0"}))
        .await
        .unwrap();
    let task = service
        .start(
            &json!({"plan_id": plan["plan_id"], "root_index": 0, "filename": filename,
        "max_bytes": 10 * 1024 * 1024, "connections": 2, "idempotency_key": key}),
        )
        .await
        .unwrap();
    let task = done(service, &task["task_id"]).await;
    assert_eq!(task["state"], "complete", "{task}");
    task
}

fn command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command.env_clear().stdin(Stdio::null());
    for key in [
        "SystemRoot",
        "SYSTEMROOT",
        "WINDIR",
        "TEMP",
        "TMP",
        "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

fn verify_media(helper: &Path, file: &Path) {
    let probe = helper.with_file_name(format!("ffprobe{}", std::env::consts::EXE_SUFFIX));
    let output = command(&probe)
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-show_entries",
            "stream=codec_type,codec_name,extradata_size,sample_rate",
            "-of",
            "json",
        ])
        .arg(file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let info: Value = serde_json::from_slice(&output.stdout).unwrap();
    let streams = info["streams"].as_array().unwrap();
    assert!(streams.iter().any(|stream| stream["codec_type"] == "video"));
    assert!(streams.iter().any(|stream| stream["codec_type"] == "audio"));
    assert!(streams.iter().any(|stream| {
        stream["codec_name"] == "aac"
            && stream["extradata_size"]
                .as_u64()
                .is_some_and(|size| size >= 2)
            && stream["sample_rate"] == "48000"
    }));
    let output = command(helper)
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "file",
            "-i",
        ])
        .arg(file)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "decoding failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[ignore = "requires explicitly verified ZEUS_MEDIA_TEST_HELPER and synthetic ZEUS_MEDIA_TEST_FIXTURES"]
async fn real_ffmpeg_mux_repackage_decode_caps_and_pin_changes() {
    let helper = PathBuf::from(
        std::env::var_os("ZEUS_MEDIA_TEST_HELPER").expect("explicit isolated helper path"),
    );
    let fixtures = PathBuf::from(
        std::env::var_os("ZEUS_MEDIA_TEST_FIXTURES").expect("explicit synthetic fixture folder"),
    );
    assert!(helper.is_absolute() && fixtures.is_absolute());
    let temporary = tempfile::TempDir::new().unwrap();
    let home = AppHome::at(temporary.path().join("profile"));
    home.create().unwrap();
    zeus_app::engine_config::save(&home, &zeus_app::engine_config::load(&home).unwrap()).unwrap();
    let hash = hex::encode(Sha256::digest(std::fs::read(&helper).unwrap()));
    let config = home.app_dir().join("media-helper.json");
    std::fs::write(
        &config,
        serde_json::to_vec(
            &json!({"version": 1, "enabled": true, "executable": helper, "sha256": hash}),
        )
        .unwrap(),
    )
    .unwrap();
    let service = Service::new(home.clone());
    assert_eq!(service.helper()["configured"], true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .fallback(handle)
        .with_state(Arc::new(fixtures));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let video = acquire(&service, &base, "video.mp4", "actual-video-request-001").await;
    let audio = acquire(&service, &base, "audio.m4a", "actual-audio-request-001").await;
    let ts = acquire(&service, &base, "combined.ts", "actual-ts-request-00001").await;
    for (input_ids, container, filename, key) in [
        (
            json!([video["task_id"], audio["task_id"]]),
            "mp4",
            "pair.mp4",
            "actual-pair-request-0001",
        ),
        (
            json!([ts["task_id"]]),
            "mp4",
            "repackaged.mp4",
            "actual-ts-mux-request-01",
        ),
        (
            json!([ts["task_id"]]),
            "mkv",
            "repackaged.mkv",
            "actual-mkv-request-0001",
        ),
    ] {
        let review = service
            .mux_preview(&json!({"task_ids": input_ids, "container": container}))
            .unwrap();
        let request = json!({"plan_id": review["plan_id"], "root_index": 0, "filename": filename,
            "max_bytes": 10 * 1024 * 1024, "timeout_ms": 30_000, "idempotency_key": key});
        let task = service.mux_start(&request).unwrap();
        let duplicate = service.mux_start(&request).unwrap();
        assert_eq!(duplicate["task_id"], task["task_id"]);
        let mut changed = request.clone();
        changed["timeout_ms"] = json!(31_000);
        assert_eq!(
            service.mux_start(&changed).unwrap_err().code,
            "media.idempotency_conflict"
        );
        let task = done(&service, &task["task_id"]).await;
        assert_eq!(task["state"], "complete", "{task}");
        assert_eq!(task["helper_sha256"], hash);
        let path = home.default_download_root().join(filename);
        assert_eq!(
            task["sha256"],
            hex::encode(Sha256::digest(std::fs::read(&path).unwrap()))
        );
        verify_media(&helper, &path);
        if filename == "pair.mp4" {
            let checkpoint_path = home
                .app_dir()
                .join("media")
                .join(task["task_id"].as_str().unwrap())
                .join("checkpoint.json");
            let mut checkpoint: Value =
                serde_json::from_slice(&std::fs::read(&checkpoint_path).unwrap()).unwrap();
            checkpoint["state"] = json!("converting");
            std::fs::write(&checkpoint_path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
            let disabled = config.with_extension("disabled");
            std::fs::rename(&config, &disabled).unwrap();
            let recovery = Service::new(home.clone());
            assert_eq!(recovery.helper()["configured"], false);
            assert_eq!(
                recovery.list().unwrap()["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["task_id"] == task["task_id"])
                    .unwrap()["recovery_available"],
                true
            );
            recovery
                .recover(&json!({"task_id": task["task_id"]}))
                .unwrap();
            let recovered = done(&recovery, &task["task_id"]).await;
            assert_eq!(recovered["state"], "complete", "{recovered}");
            assert_eq!(recovered["sha256"], task["sha256"]);
            recovery.shutdown().await;
            std::fs::rename(disabled, &config).unwrap();
        }
        service
            .archive(&json!({"task_id": task["task_id"]}))
            .unwrap();
        assert!(path.exists());
    }
    let review = service
        .mux_preview(&json!({"task_ids": [video["task_id"], audio["task_id"]], "container": "mp4"}))
        .unwrap();
    let capped = service.mux_start(&json!({"plan_id": review["plan_id"], "root_index": 0, "filename": "capped.mp4", "max_bytes": 128, "timeout_ms": 30_000, "idempotency_key": "actual-cap-request-001"})).unwrap();
    let capped = done(&service, &capped["task_id"]).await;
    assert_eq!(capped["error_code"], "media.output_limit");
    assert!(!home.default_download_root().join("capped.mp4").exists());
    let mut pin: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    pin["sha256"] = json!("0".repeat(64));
    std::fs::write(&config, serde_json::to_vec(&pin).unwrap()).unwrap();
    let refusal = service.mux_start(&json!({"plan_id": review["plan_id"], "root_index": 0, "filename": "changed.mp4", "max_bytes": 1024 * 1024, "timeout_ms": 30_000, "idempotency_key": "actual-changed-request-1"})).unwrap_err();
    assert_eq!(refusal.code, "media.helper_changed");
    service.shutdown().await;
    server.abort();
}
