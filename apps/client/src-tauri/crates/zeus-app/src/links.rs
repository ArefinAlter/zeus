//! The link grabber's online check: one probe per address, through zeusd, a few at a time.
//!
//! zeusd's `jobs.probe` follows redirects, reads `Content-Disposition`, learns the size and whether ranges work, and
//! never raises an approval. The answer says what a download would meet, without creating one.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::daemon::Daemon;
use crate::failure::{Failure, Result};

const MAX_URLS: usize = 50;

async fn probe_one(daemon: &Daemon, url: &str) -> Value {
    let mut last = None;
    for attempt in 0..4 {
        match daemon.raw("jobs.probe", json!({"url": url})).await {
            Ok(view) => return from_probe(url, &view),
            Err(f) if f.code == "api.rate_limited" && attempt < 3 => {
                tokio::time::sleep(Duration::from_millis(350)).await;
                last = Some(f);
            }
            Err(f) => return from_failure(url, &f),
        }
    }
    from_failure(
        url,
        &last.unwrap_or_else(|| Failure::new("api.rate_limited", "zeusd is busy.")),
    )
}

fn from_probe(url: &str, view: &Value) -> Value {
    let probe = view.get("probe").cloned().unwrap_or(Value::Null);
    let status = probe.get("status").and_then(Value::as_u64).unwrap_or(0);
    let requires_session = probe
        .get("requires_session")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let check = if requires_session {
        "needs_login"
    } else if (200..300).contains(&status) {
        "online"
    } else {
        "offline"
    };
    json!({
        "url": url,
        "check": check,
        "status": status,
        "size": probe.get("total_bytes"),
        "name": probe.pointer("/filename/value"),
        "name_source": probe.pointer("/filename/source"),
        "resumable": probe.get("range_support"),
        "content_type": probe.get("content_type"),
        "redirects": probe.get("redirects"),
        "final_url": probe.get("final_url_redacted"),
        "interstitial": probe.get("interstitial_suspected"),
        "approval_needed": view.get("approval_needed"),
    })
}

fn from_failure(url: &str, f: &Failure) -> Value {
    let status = f
        .code
        .strip_prefix("http.status_")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    json!({
        "url": url,
        "check": if f.code == "http.auth_required" { "needs_login" } else { "offline" },
        "status": status,
        "size": Value::Null,
        "error_code": f.code,
        "error_message": f.message,
    })
}

/// Check up to 50 addresses; the answers come back in the order asked.
pub async fn check(daemon: Arc<Daemon>, urls: &[String]) -> Result<Value> {
    if urls.len() > MAX_URLS {
        return Err(Failure::invalid("Check at most 50 links at a time."));
    }
    let gate = Arc::new(Semaphore::new(4));
    let mut tasks = Vec::new();
    for url in urls {
        let (daemon, gate, url) = (daemon.clone(), gate.clone(), url.clone());
        tasks.push(tokio::spawn(async move {
            let _permit = gate.acquire().await;
            let parsed = url::Url::parse(&url);
            if !parsed.as_ref().is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
                return json!({"url": url, "check": "offline", "status": 0, "size": Value::Null, "error_code": "api.invalid_params", "error_message": "Not an http or https address."});
            }
            probe_one(&daemon, &url).await
        }));
    }
    let mut items = Vec::new();
    for t in tasks {
        items.push(
            t.await
                .unwrap_or_else(|_| json!({"check": "offline", "status": 0})),
        );
    }
    Ok(json!({"items": items}))
}
