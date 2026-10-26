//! Link review: a dry-run enqueue the person reads, then exactly that batch, once.
//!
//! The exact inputs (including query strings, which the page never sees again) stay here for ten minutes. A review
//! that expired, or was used, is refused. Every start carries a retry key, so a lost answer can be asked again
//! without creating the downloads twice.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use std::sync::Arc;

use crate::daemon::Daemon;
use crate::engine_config::EngineSettings;
use crate::failure::{Failure, Result};

const REVIEW_FOR: Duration = Duration::from_secs(600);
const MAX_LINKS: usize = 50;
const MAX_INPUT_BYTES: usize = 64 * 1024;

struct Review {
    created: Instant,
    jobs: Vec<Value>,
}

/// The reviews waiting to be started.
#[derive(Default)]
pub struct Reviews {
    held: Mutex<HashMap<String, Review>>,
}

/// 32 lowercase hex characters from the operating system's random source.
pub fn random_id() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes[..]);
    hex::encode(bytes)
}

/// True when `path` lies inside `root` (case-insensitive on Windows, lexical).
pub fn inside(root: &str, path: &str) -> bool {
    let norm = |s: &str| {
        let s = s.replace('\\', "/");
        let s = s.trim_end_matches('/').to_owned();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let (r, p) = (norm(root), norm(path));
    p == r || p.starts_with(&format!("{r}/"))
}

fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// One requested download, as the page sends it.
struct Item {
    index: usize,
    job: Map<String, Value>,
}

fn parse_items(body: &Value, settings: &EngineSettings) -> Result<(Vec<Item>, Vec<Value>)> {
    let default_dir = body
        .get("dir")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let sub = body
                .get("subdir")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .replace('\\', "/");
            let sub = sub.trim_matches('/');
            settings.write_roots.first().map(|r| {
                if sub.is_empty() {
                    r.clone()
                } else {
                    Path::new(r).join(sub).display().to_string()
                }
            })
        });
    let raw: Vec<Value> = match body.get("items").and_then(Value::as_array) {
        Some(items) => items.clone(),
        None => body
            .get("urls")
            .and_then(Value::as_array)
            .map(|u| u.iter().map(|x| json!({"url": x})).collect())
            .unwrap_or_default(),
    };
    let bytes: usize = raw
        .iter()
        .map(|i| i.get("url").and_then(Value::as_str).map_or(0, str::len))
        .sum();
    if raw.is_empty() || bytes > MAX_INPUT_BYTES {
        return Err(Failure::invalid(
            "Use 1 to 50 unique http or https links, at most 64 KiB in total.",
        ));
    }
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut refused = Vec::new();
    for (index, entry) in raw.iter().enumerate() {
        let input = entry
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let dir = entry
            .get("dir")
            .and_then(Value::as_str)
            .filter(|d| !d.is_empty())
            .map(str::to_owned)
            .or_else(|| default_dir.clone());
        let filename = entry
            .get("filename")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if input.is_empty()
            || !seen.insert((input.clone(), dir.clone(), filename.map(str::to_owned)))
        {
            continue;
        }
        let reason = match url::Url::parse(&input) {
            Err(_) => Some("Not a valid address."),
            Ok(u) if !matches!(u.scheme(), "http" | "https") => {
                Some("Zeus downloads http and https addresses only.")
            }
            Ok(u) if !u.username().is_empty() || u.password().is_some() => {
                Some("No user name or password in the address.")
            }
            Ok(_) => None,
        };
        if let Some(reason) = reason {
            refused.push(json!({"index": index, "reason": reason}));
            continue;
        }
        if let Some(d) = &dir {
            if !Path::new(d).is_absolute() || !settings.write_roots.iter().any(|r| inside(r, d)) {
                refused.push(json!({"index": index, "reason": "That folder is outside the download folders Zeus may write to. Add it in Settings, Save to."}));
                continue;
            }
        }
        let mut job = Map::new();
        job.insert("url".into(), json!(input));
        if let Some(d) = dir {
            job.insert("dest_dir".into(), json!(d));
        }
        if let Some(f) = filename {
            job.insert("filename".into(), json!(f));
        }
        for key in [
            "expected_sha256",
            "expected_size",
            "overwrite",
            "priority",
            "max_connections",
            "referrer",
        ] {
            if let Some(v) = entry.get(key).filter(|v| !v.is_null()) {
                job.insert(key.into(), v.clone());
            }
        }
        if let Some(q) = entry
            .get("queue")
            .and_then(Value::as_str)
            .filter(|q| !q.is_empty())
        {
            job.insert("labels".into(), json!({"zeus.queue": q}));
        }
        items.push(Item { index, job });
        if items.len() > MAX_LINKS {
            return Err(Failure::invalid(
                "Use 1 to 50 unique http or https links, at most 64 KiB in total.",
            ));
        }
    }
    Ok((items, refused))
}

impl Reviews {
    /// Resolve a batch without creating anything and keep the exact inputs for ten minutes.
    pub async fn preview(
        &self,
        daemon: &Arc<Daemon>,
        settings: &EngineSettings,
        body: &Value,
    ) -> Result<Value> {
        let (mut items, refused) = parse_items(body, settings)?;
        if !refused.is_empty() {
            return Ok(json!({"review_id": null, "ready": false, "items": [], "refused": refused}));
        }
        if items.is_empty() {
            return Err(Failure::invalid(
                "Use 1 to 50 unique http or https links, at most 64 KiB in total.",
            ));
        }
        // What each address really is: its server-given name, size and whether it can be resumed. A name the server
        // gives (Content-Disposition) is used unless the person chose one, because zeusd fixes the destination name
        // when the job is created.
        let wanted: Vec<String> = items
            .iter()
            .map(|i| {
                i.job
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        let probes = crate::links::check(daemon.clone(), &wanted).await?;
        let probes: Vec<Value> = probes
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (item, probe) in items.iter_mut().zip(probes.iter()) {
            let given = probe
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty());
            let from_server =
                probe.get("name_source").and_then(Value::as_str) == Some("content_disposition");
            if !item.job.contains_key("filename")
                && let Some(name) = given
                && from_server
            {
                item.job.insert("filename".into(), json!(name));
            }
        }
        let jobs: Vec<Value> = items.iter().map(|i| Value::Object(i.job.clone())).collect();
        for item in &items {
            if let Some(d) = item.job.get("dest_dir").and_then(Value::as_str) {
                let _ = std::fs::create_dir_all(d);
            }
        }
        let result = daemon
            .raw("jobs.enqueue", json!({"jobs": jobs, "dry_run": true}))
            .await?;
        let entries = result
            .get("jobs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut ready = true;
        let out: Vec<Value> = entries
            .iter()
            .zip(items.iter())
            .enumerate()
            .map(|(n, (e, item))| match e.get("plan").filter(|p| !p.is_null()) {
                Some(plan) => json!({
                    "index": item.index,
                    "url_redacted": plan.get("url_redacted"),
                    "filename": plan.get("resolved_filename").filter(|v| !v.is_null()).or_else(|| item.job.get("filename")).or_else(|| probes.get(n).and_then(|p| p.get("name"))),
                    "destination": plan.get("dest_path"),
                    "approval_needed": plan.get("approval_needed"),
                    "warnings": plan.get("warnings"),
                    "size": probes.get(n).and_then(|p| p.get("size")),
                    "resumable": probes.get(n).and_then(|p| p.get("resumable")),
                    "content_type": probes.get(n).and_then(|p| p.get("content_type")),
                    "probe_status": probes.get(n).and_then(|p| p.get("status")),
                }),
                None => {
                    ready = false;
                    json!({
                        "index": item.index,
                        "error_code": e.pointer("/error/code"),
                        "error_message": e.pointer("/error/message"),
                    })
                }
            })
            .collect();
        if !ready || out.len() != items.len() {
            return Ok(json!({"review_id": null, "ready": false, "items": out, "refused": []}));
        }
        let id = random_id();
        let mut held = self.held.lock().await;
        held.retain(|_, r| r.created.elapsed() <= REVIEW_FOR);
        if held.len() >= 8
            && let Some(oldest) = held
                .iter()
                .min_by_key(|(_, r)| r.created)
                .map(|(k, _)| k.clone())
        {
            held.remove(&oldest);
        }
        held.insert(
            id.clone(),
            Review {
                created: Instant::now(),
                jobs,
            },
        );
        Ok(json!({"review_id": id, "ready": true, "items": out, "refused": []}))
    }

    /// Start exactly the reviewed batch, once per retry key.
    pub async fn enqueue(&self, daemon: &Daemon, body: &Value) -> Result<Value> {
        let key = body
            .get("idempotency_key")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !valid_key(key) {
            return Err(Failure::invalid(
                "The retry key must have 1 to 64 letters, digits, '-', '_' or '.'.",
            ));
        }
        let review_id = body
            .get("review_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let jobs = {
            let held = self.held.lock().await;
            held.get(review_id)
                .filter(|r| r.created.elapsed() <= REVIEW_FOR)
                .map(|r| r.jobs.clone())
                .ok_or_else(|| {
                    Failure::new(
                        "app.review_expired",
                        "Preview the links again before starting them.",
                    )
                })?
        };
        let key: String = format!("app-{key}").chars().take(64).collect();
        let result = daemon
            .raw(
                "jobs.enqueue",
                json!({"jobs": jobs, "dry_run": false, "idempotency_key": key}),
            )
            .await?;
        let entries = result
            .get("jobs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let out: Vec<Value> = entries
            .iter()
            .map(|e| {
                json!({
                    "job_id": e.get("job_id"),
                    "state": e.get("state"),
                    "approval_id": e.get("approval_id"),
                    "error_code": e.pointer("/error/code"),
                    "error_message": e.pointer("/error/message"),
                })
            })
            .collect();
        if out.iter().all(|e| e["job_id"].is_string()) {
            self.held.lock().await.remove(review_id);
        }
        Ok(
            json!({"replayed": result.get("replayed").and_then(Value::as_bool).unwrap_or(false), "jobs": out}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> EngineSettings {
        EngineSettings {
            write_roots: vec![if cfg!(windows) {
                "C:\\dl".into()
            } else {
                "/dl".into()
            }],
            ..EngineSettings::default()
        }
    }

    #[test]
    fn inside_is_lexical_and_exact_on_the_boundary() {
        assert!(inside("/dl", "/dl/a/b"));
        assert!(inside("/dl/", "/dl"));
        assert!(!inside("/dl", "/dlx/a"));
        assert!(!inside("/dl", "/other"));
    }

    #[test]
    fn items_are_validated_deduplicated_and_labelled() {
        let s = settings();
        let root = s.write_roots[0].clone();
        let body = json!({"items": [
            {"url": "https://example.org/a.zip", "queue": "night", "filename": "b.zip"},
            {"url": "https://example.org/a.zip", "queue": "night", "filename": "b.zip"},
            {"url": "ftp://example.org/x"},
        ]});
        let (items, refused) = parse_items(&body, &s).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].job["labels"]["zeus.queue"], "night");
        assert_eq!(items[0].job["dest_dir"], root);
        assert_eq!(refused.len(), 1);
        let outside = json!({"items": [{"url": "https://example.org/a.zip", "dir": if cfg!(windows) { "D:\\elsewhere" } else { "/elsewhere" }}]});
        assert_eq!(parse_items(&outside, &s).unwrap().1.len(), 1);
        assert!(parse_items(&json!({"urls": []}), &s).is_err());
    }
}
