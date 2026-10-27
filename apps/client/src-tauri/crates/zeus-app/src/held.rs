//! Requests the app itself holds: a site-grabber plan or a file move that the person proposed in this window.
//!
//! They are decided in the window, with the rules of spec 07 (the request hash the person saw, an arming delay,
//! the offered scopes, an expiry), and labelled as held by the app. Nothing here is a Zeus daemon approval: the
//! daemon's approvals (a browser login lent to a download, a destructive file operation) are decided only on
//! zeusd's own trust surfaces, the interactive CLI and the extension popup (ADR 0007).

use std::path::PathBuf;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::engine_config::write_atomic;
use crate::failure::{Failure, Result};
use crate::home::AppHome;

/// The arming delay of the approve button.
pub const ARMING_MS: u64 = 750;
const HELD_KINDS: &[&str] = &["workflow_plan", "destructive_op"];
const MAX_HELD: usize = 200;

/// The store of held requests.
pub struct Held {
    path: PathBuf,
    lock: Mutex<()>,
}

/// The canonical JSON the client hashes: sorted keys, integers only.
pub fn canonical(v: &Value) -> Result<String> {
    Ok(match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                n.to_string()
            } else {
                return Err(Failure::new(
                    "app.bad_approval",
                    "Facts must use integers only.",
                ));
            }
        }
        Value::String(s) => serde_json::to_string(s)?,
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(canonical)
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            let parts = keys
                .into_iter()
                .map(|k| {
                    Ok(format!(
                        "{}:{}",
                        serde_json::to_string(k)?,
                        canonical(&o[k])?
                    ))
                })
                .collect::<Result<Vec<String>>>()?;
            format!("{{{}}}", parts.join(","))
        }
    })
}

fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    // `YYYY-MM-DDTHH:MM:SS(.mmm)Z`, which is all the client writes.
    let b = text.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |a: usize, z: usize| text.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, s) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    let ms = if b.get(19) == Some(&b'.') {
        text.get(20..23)?.parse::<i64>().ok()?
    } else {
        0
    };
    // Days from civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let total = ((days * 24 + h) * 60 + mi) * 60 + s;
    u64::try_from(total * 1000 + ms).ok()
}

impl Held {
    /// The store under `home`.
    pub fn new(home: &AppHome) -> Self {
        Self {
            path: home.app_dir().join("approvals.json"),
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Vec<Value> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
            .unwrap_or_default()
    }

    fn write(&self, list: &[Value]) -> Result<()> {
        write_atomic(&self.path, &serde_json::to_vec(list)?)?;
        Ok(())
    }

    /// Held requests, newest first; pending ones past their deadline are marked expired.
    pub async fn list(&self) -> Result<Value> {
        let _guard = self.lock.lock().await;
        let mut list = self.read();
        let now = now_ms();
        let mut changed = false;
        for a in &mut list {
            let pending = a["state"] == "pending";
            let expires = a
                .pointer("/facts/expires_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_ms);
            if pending && expires.is_some_and(|e| e <= now) {
                a["state"] = json!("expired");
                changed = true;
            }
        }
        if changed {
            self.write(&list)?;
        }
        list.reverse();
        Ok(json!({"items": list}))
    }

    /// Store a new pending request after checking its hash against its facts.
    pub async fn create(&self, a: &Value) -> Result<Value> {
        let id = a.get("id").and_then(Value::as_str).unwrap_or_default();
        if !(id.starts_with("ap_")
            && id.len() >= 13
            && id.len() <= 43
            && id[3..].chars().all(|c| c.is_ascii_alphanumeric()))
        {
            return Err(Failure::new("app.bad_approval", "Bad request id."));
        }
        let kind = a
            .pointer("/facts/kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !HELD_KINDS.contains(&kind) {
            return Err(Failure::new(
                "app.bad_approval",
                "This app holds only plan and file-move requests.",
            ));
        }
        let facts = a
            .get("facts")
            .ok_or_else(|| Failure::new("app.bad_approval", "No facts."))?;
        let hash = sha256_hex(&canonical(facts)?);
        if a.get("requestHash").and_then(Value::as_str) != Some(hash.as_str()) {
            return Err(Failure::new(
                "app.bad_approval",
                "The request hash does not match the facts.",
            ));
        }
        let _guard = self.lock.lock().await;
        let mut list = self.read();
        if list
            .iter()
            .any(|x| x["id"] == id && x["state"] != "pending")
        {
            return Err(Failure::new(
                "approval.not_pending",
                "That request was already decided.",
            ));
        }
        if list.len() >= MAX_HELD {
            list.drain(0..list.len() - MAX_HELD + 1);
        }
        let stored = json!({
            "id": id,
            "state": "pending",
            "facts": facts,
            "requestHash": hash,
            "bindCode": a.get("bindCode").and_then(Value::as_str).unwrap_or_default(),
            "jobIds": a.get("jobIds").cloned().unwrap_or_else(|| json!([])),
            "agentSays": a.get("agentSays").cloned().unwrap_or(Value::Null),
            "heldBy": "service",
        });
        list.retain(|x| x["id"] != id);
        list.push(stored.clone());
        self.write(&list)?;
        Ok(stored)
    }

    /// Decide a pending request. The page enforces the same rules before it asks.
    pub async fn decide(&self, id: &str, d: &Value) -> Result<Value> {
        let _guard = self.lock.lock().await;
        let mut list = self.read();
        let Some(a) = list.iter_mut().find(|x| x["id"] == id) else {
            return Err(Failure::new("approval.not_found", "No such request."));
        };
        if a["state"] != "pending" {
            return Err(Failure::new(
                "approval.not_pending",
                format!(
                    "This request is already {}.",
                    a["state"].as_str().unwrap_or("decided")
                ),
            ));
        }
        let expires = a
            .pointer("/facts/expires_at")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_ms);
        if expires.is_some_and(|e| e <= now_ms()) {
            a["state"] = json!("expired");
            let snapshot = list.clone();
            self.write(&snapshot)?;
            return Err(Failure::new("approval.expired", "This request expired."));
        }
        if d.get("seen_hash") != a.get("requestHash") {
            return Err(Failure::new(
                "approval.hash_mismatch",
                "The request changed after you read it. Read the facts again.",
            ));
        }
        let decision = d
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if decision != "approve" && decision != "deny" {
            return Err(Failure::invalid("Approve or deny."));
        }
        if decision == "approve"
            && d.get("armed_ms").and_then(Value::as_u64).unwrap_or(0) < ARMING_MS
        {
            return Err(Failure::new(
                "approval.not_armed",
                "The approve button was pressed before it armed.",
            ));
        }
        let scopes: Vec<String> = a
            .pointer("/facts/allowed_scopes")
            .and_then(Value::as_array)
            .map(|v| {
                v.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let scope = d.get("scope").and_then(Value::as_str);
        if decision == "approve"
            && !scopes.is_empty()
            && !scope.is_some_and(|s| scopes.iter().any(|x| x == s))
        {
            return Err(Failure::new(
                "approval.scope",
                "Choose one of the offered scopes.",
            ));
        }
        a["state"] = json!(if decision == "approve" && scope != Some("never") {
            "approved"
        } else {
            "denied"
        });
        let mut decided =
            json!({"at": now_ms(), "decision": decision, "via": "Zeus app (this window)"});
        if let Some(s) = scope {
            decided["scope"] = json!(s);
        }
        a["decided"] = decided;
        let out = a.clone();
        self.write(&list)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Value {
        json!({"t":"zeus.approval.facts.v1","approval_id":"ap_0123456789ABCDEF","revision":1,"kind":"workflow_plan",
               "allowed_scopes":["workflow","never"],"expires_at":"2099-01-01T00:00:00.000Z","files":[{"name":"a","size":1}]})
    }

    #[test]
    fn rfc3339_parsing_matches_known_instants() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:01.500Z"), Some(1500));
        assert_eq!(
            parse_rfc3339_ms("2000-03-01T00:00:00.000Z"),
            Some(951_868_800_000)
        );
        assert_eq!(parse_rfc3339_ms("nonsense"), None);
    }

    #[test]
    fn canonical_sorts_keys_and_refuses_floats() {
        assert_eq!(
            canonical(&json!({"b":1,"a":[true,null,"x"]})).unwrap(),
            "{\"a\":[true,null,\"x\"],\"b\":1}"
        );
        assert!(canonical(&json!({"a":1.5})).is_err());
    }

    #[tokio::test]
    async fn a_request_is_decided_once_with_the_hash_it_showed() {
        let dir = tempfile::tempdir().unwrap();
        let held = Held::new(&AppHome::at(dir.path().to_path_buf()));
        let f = facts();
        let hash = sha256_hex(&canonical(&f).unwrap());
        let made = held
            .create(&json!({"id":"ap_0123456789ABCDEF","facts":f,"requestHash":hash,"bindCode":"ABC-123"}))
            .await
            .unwrap();
        assert_eq!(made["state"], "pending");
        assert!(
            held.create(&json!({"id":"ap_0123456789ABCDEF","facts":facts(),"requestHash":"bad"}))
                .await
                .is_err()
        );
        let too_early = held
            .decide(
                "ap_0123456789ABCDEF",
                &json!({"decision":"approve","scope":"workflow","seen_hash":hash,"armed_ms":100}),
            )
            .await;
        assert_eq!(too_early.unwrap_err().code, "approval.not_armed");
        let wrong = held
            .decide(
                "ap_0123456789ABCDEF",
                &json!({"decision":"approve","scope":"workflow","seen_hash":"x","armed_ms":900}),
            )
            .await;
        assert_eq!(wrong.unwrap_err().code, "approval.hash_mismatch");
        let done = held
            .decide(
                "ap_0123456789ABCDEF",
                &json!({"decision":"approve","scope":"workflow","seen_hash":hash,"armed_ms":900}),
            )
            .await
            .unwrap();
        assert_eq!(done["state"], "approved");
        let again = held
            .decide(
                "ap_0123456789ABCDEF",
                &json!({"decision":"deny","seen_hash":hash,"armed_ms":900}),
            )
            .await;
        assert_eq!(again.unwrap_err().code, "approval.not_pending");
    }
}
