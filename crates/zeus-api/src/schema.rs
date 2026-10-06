//! The draft JSON Schema of the daemon API, generated from the Rust types.
//!
//! `crates/zeus-api/schemas/daemon-api.v1.draft.schema.json` is the checked-in output. It is the starting point of
//! `docs/spec/schema/daemon-api.v1.schema.json` once spec 05 is written. A test fails when the Rust types and the
//! checked-in file drift apart; regenerate with `UPDATE_SCHEMAS=1 cargo test -p zeus-api schema`.

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use crate::methods::{self, notifications};

/// Build the schema document: `methods` maps each method name to its params and result, `notifications` lists the
/// server-to-client notifications, and `$defs` holds every shared definition (core entities included).
pub fn daemon_api_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let mut method_map = Map::new();
    for (name, params, result) in methods::describe_all(&mut generator) {
        method_map.insert(
            name.to_string(),
            json!({ "params": params, "result": result }),
        );
    }
    let nm_frame = generator.subschema_for::<notifications::NmFrame>();
    let stopping = generator.subschema_for::<notifications::StoppingNotification>();
    let event = generator.subschema_for::<methods::events::EventNotification>();
    let lagged = generator.subschema_for::<methods::events::LaggedNotification>();
    let mut notification_map = Map::new();
    notification_map.insert(
        notifications::EVENTS_EVENT.into(),
        json!({ "params": event }),
    );
    notification_map.insert(
        notifications::EVENTS_LAGGED.into(),
        json!({ "params": lagged }),
    );
    notification_map.insert(
        notifications::DAEMON_STOPPING.into(),
        json!({ "params": stopping }),
    );
    notification_map.insert(
        notifications::NM_FRAME.into(),
        json!({ "params": nm_frame }),
    );
    let defs = generator.take_definitions(true);
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://zeus.invalid/schema/daemon-api.v1.draft",
        "title": "Zeus daemon API v1 (draft generated from crates/zeus-api)",
        "description": "JSON-RPC 2.0 over newline-delimited JSON on a local socket. See docs/ARCHITECTURE.md section 5.",
        "protocol_version": crate::PROTOCOL_VERSION,
        "methods": Value::Object(method_map),
        "notifications": Value::Object(notification_map),
        "$defs": Value::Object(defs),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked_in_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schemas")
            .join("daemon-api.v1.draft.schema.json")
    }

    #[test]
    fn every_method_has_a_params_and_result_schema() {
        let schema = daemon_api_schema();
        let methods = schema["methods"].as_object().unwrap();
        assert_eq!(methods.len(), crate::methods::ALL_NAMES.len());
        for name in crate::methods::ALL_NAMES {
            assert!(
                methods[*name].get("params").is_some() && methods[*name].get("result").is_some(),
                "{name}"
            );
        }
        assert_eq!(schema["notifications"].as_object().unwrap().len(), 4);
    }

    #[test]
    fn the_checked_in_schema_matches_the_types() {
        let generated = serde_json::to_string_pretty(&daemon_api_schema()).unwrap() + "\n";
        let path = checked_in_path();
        if std::env::var_os("UPDATE_SCHEMAS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &generated).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        assert!(
            on_disk == generated,
            "schemas/daemon-api.v1.draft.schema.json is out of date; run `UPDATE_SCHEMAS=1 cargo test -p zeus-api schema`"
        );
    }

    #[test]
    fn the_schema_validates_a_real_request_and_rejects_a_bad_one() {
        let schema = daemon_api_schema();
        let enqueue = &schema["methods"]["jobs.enqueue"]["params"];
        // Wrap the params schema with the shared definitions so $ref resolves.
        let root =
            json!({ "$schema": schema["$schema"], "$defs": schema["$defs"], "allOf": [enqueue] });
        let validator = jsonschema::validator_for(&root).expect("schema compiles");
        assert!(validator.is_valid(&json!({ "jobs": [{ "url": "https://files.example.com/a.zip" }], "idempotency_key": "k" })));
        assert!(!validator.is_valid(&json!({ "jobs": "nope" })));
    }
}
