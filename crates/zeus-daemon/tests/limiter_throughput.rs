//! The speed limiter measured against its limit.
//!
//! A loopback server sends a body as fast as the client will read it; the throttled transport paces what the client
//! sees. Each test reads a body through the transport and checks that the measured throughput is within 10 percent of
//! the configured limit (and that an unlimited or out-of-window transport is not slowed down).

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use chrono::TimeZone;
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeus_api::methods::queues::{LimiterDef, QueueDefaults, Timezone};
use zeus_daemon::limiter::{Limits, Membership, ThrottledTransport};
use zeus_daemon::queues::QueueService;
use zeus_http::{ClientConfig, HttpRequest, HttpTransport, ReqwestTransport};

const KIB: usize = 1024;

/// A server answering every request with `size` bytes of 200 OK.
async fn server(size: usize) -> (url::Url, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!(
        "http://{}/body.bin",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.ends_with(b"\r\n\r\n") && request.len() < 16384 {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
                );
                if stream.write_all(head.as_bytes()).await.is_ok() {
                    let _ = stream.write_all(&vec![7u8; size]).await;
                }
            });
        }
    });
    (url, task)
}

fn transport(limits: &Arc<Limits>, queue: Option<&str>) -> ThrottledTransport {
    let inner: Arc<dyn HttpTransport> =
        Arc::new(ReqwestTransport::new(&ClientConfig::default()).unwrap());
    ThrottledTransport::new(
        inner,
        limits.clone(),
        Membership {
            label: queue.map(str::to_owned),
            fingerprint: "test".into(),
        },
    )
}

/// Read one body through `transport`; returns the bytes and how long it took from the request to the last byte.
async fn read_body(transport: &ThrottledTransport, url: &url::Url) -> (usize, Duration) {
    let started = Instant::now();
    let response = transport
        .send(HttpRequest {
            method: http::Method::GET,
            url: url.clone(),
            headers: http::HeaderMap::new(),
            head_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(10),
        })
        .await
        .unwrap();
    assert_eq!(response.status.as_u16(), 200);
    let mut body = response.body;
    let mut total = 0usize;
    while let Some(chunk) = body.next().await {
        total += chunk.unwrap().len();
    }
    (total, started.elapsed())
}

fn rate(bytes: usize, took: Duration) -> f64 {
    bytes as f64 / took.as_secs_f64()
}

fn within_ten_percent(measured: f64, limit_bytes_per_s: f64, what: &str) {
    let error = (measured - limit_bytes_per_s).abs() / limit_bytes_per_s;
    println!(
        "{what}: measured {:.0} KiB/s against a limit of {:.0} KiB/s ({:+.1} percent)",
        measured / KIB as f64,
        limit_bytes_per_s / KIB as f64,
        (measured - limit_bytes_per_s) / limit_bytes_per_s * 100.0
    );
    assert!(
        error <= 0.10,
        "{what}: {:.0} B/s is {:.1} percent away from the limit {:.0} B/s",
        measured,
        error * 100.0,
        limit_bytes_per_s
    );
}

#[tokio::test]
async fn the_global_limit_holds_within_ten_percent() {
    let (url, _server) = server(2048 * KIB).await;
    let limits = Limits::new();
    let client = transport(&limits, None);
    limits.set_global(Some(1024));
    let (bytes, took) = read_body(&client, &url).await;
    assert_eq!(bytes, 2048 * KIB);
    within_ten_percent(rate(bytes, took), (1024 * KIB) as f64, "global 1024 KiB/s");
}

#[tokio::test]
async fn a_lower_queue_limit_wins_over_the_global_one() {
    let (url, _server) = server(1024 * KIB).await;
    let limits = Limits::new();
    let client = transport(&limits, Some("night"));
    limits.set_global(Some(2048));
    limits.set_queue("night", Some(512));
    let (bytes, took) = read_body(&client, &url).await;
    assert_eq!(bytes, 1024 * KIB);
    within_ten_percent(
        rate(bytes, took),
        (512 * KIB) as f64,
        "queue 512 KiB/s under global 2048",
    );
}

#[tokio::test]
async fn concurrent_downloads_share_one_limit() {
    let (url, _server) = server(512 * KIB).await;
    let limits = Limits::new();
    let a = transport(&limits, None);
    let b = transport(&limits, None);
    // Client construction can fill the intentional 100 ms idle burst. Enable the limiter
    // only after setup so that this one-second measurement does not exclude earned credit.
    limits.set_global(Some(1024));
    let started = Instant::now();
    let ((n1, _), (n2, _)) = tokio::join!(read_body(&a, &url), read_body(&b, &url));
    let took = started.elapsed();
    assert_eq!(n1 + n2, 1024 * KIB);
    within_ten_percent(
        rate(n1 + n2, took),
        (1024 * KIB) as f64,
        "two downloads sharing 1024 KiB/s",
    );
}

#[tokio::test]
async fn outside_its_window_the_limiter_does_not_slow_anything_down() {
    let (url, _server) = server(2048 * KIB).await;
    let limits = Limits::new();
    let at = |h: u32, m: u32| {
        chrono::Utc
            .with_ymd_and_hms(2026, 10, 25, h, m, 0)
            .unwrap()
            .timestamp_millis()
    };
    let clock = Arc::new(AtomicI64::new(at(14, 0)));
    let c = clock.clone();
    let dir = tempfile::tempdir().unwrap();
    let service = QueueService::open_with(
        dir.path(),
        limits.clone(),
        Arc::new(move || c.load(Ordering::SeqCst)),
    )
    .unwrap();
    service
        .configure(
            Some(LimiterDef {
                enabled: true,
                limit_kib_s: 1024,
                schedule_enabled: true,
                from: "15:00".into(),
                to: "16:00".into(),
                days: vec![0, 1, 2, 3, 4, 5, 6],
            }),
            Some(QueueDefaults {
                timezone: Timezone::Utc,
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    // 14:00 is outside the 15:00 to 16:00 window: the body arrives at loopback speed.
    assert_eq!(limits.global_rate(), None);
    let client = transport(&limits, None);
    let (bytes, took) = read_body(&client, &url).await;
    assert_eq!(bytes, 2048 * KIB);
    let free = rate(bytes, took);
    println!("outside the window: {:.0} KiB/s", free / KIB as f64);
    assert!(
        free > 8.0 * (1024 * KIB) as f64,
        "{free} B/s is not unthrottled"
    );

    // At 15:30 the scheduler's next tick applies the limit to the very next request.
    let (url2, _server2) = server(1024 * KIB).await;
    clock.store(at(15, 30), Ordering::SeqCst);
    service.tick().await.unwrap();
    assert_eq!(limits.global_rate(), Some(1024 * 1024));
    let (bytes, took) = read_body(&client, &url2).await;
    within_ten_percent(
        rate(bytes, took),
        (1024 * KIB) as f64,
        "inside the window, 1024 KiB/s",
    );
}
