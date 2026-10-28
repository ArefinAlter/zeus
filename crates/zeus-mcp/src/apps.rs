//! Read-only MCP App resource. Tool visibility permits apps only on the monitoring tools;
//! host-mediated mutation/approval/credential operations are never requested by this view.
use rmcp::model::{MetaObject, Resource, ResourceContents};
use serde_json::{Value, json};
/// Stable resource identity; the template contains no owner data or credentials.
pub const MONITOR_URI: &str = "ui://zeus/transfer-monitor/v1";
/// Standard MIME type of an MCP App HTML resource.
pub const MIME: &str = "text/html;profile=mcp-app";
/// Self-contained monitor; no runtime network/resource dependencies.
pub const HTML: &str = include_str!("../ui/transfer-monitor.html");
/// Restrictive host-enforced resource policy.
pub fn metadata() -> MetaObject {
    MetaObject(json!({"ui":{"csp":{"connectDomains":[],"resourceDomains":[],"frameDomains":[],"baseUriDomains":[]},"permissions":{},"prefersBorder":true}}).as_object().expect("static UI metadata").clone())
}
/// Discoverable static resource with an explicit sandbox policy.
pub fn resource() -> Resource {
    Resource::new(MONITOR_URI, "zeus-transfer-monitor")
        .with_description("Read-only transfer progress; no approval or mutation controls.")
        .with_mime_type(MIME)
        .with_meta(metadata())
}
/// Return the same static document for every caller; transfer data arrives through ordinary redacted tools.
pub fn contents() -> ResourceContents {
    ResourceContents::text(HTML, MONITOR_URI)
        .with_mime_type(MIME)
        .with_meta(metadata())
}
/// Server-side declaration of the Apps extension; clients without Apps keep the existing text fallback.
pub fn extension() -> serde_json::Map<String, Value> {
    json!({"mimeTypes":[MIME]})
        .as_object()
        .expect("static extension declaration")
        .clone()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inline_script_matches_the_content_security_policy_hash() {
        use base64::Engine;
        let script = HTML
            .split("<script>")
            .nth(1)
            .unwrap()
            .split("</script>")
            .next()
            .unwrap();
        let declared = HTML
            .split("script-src 'sha256-")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap();
        let actual = base64::engine::general_purpose::STANDARD
            .encode(zeus_verify::hash_bytes(script.as_bytes()).sha256.as_bytes());
        assert_eq!(
            actual, declared,
            "update the declared CSP digest after changing the bundled script"
        );
    }
    #[test]
    fn monitor_has_no_network_or_mutation_surface() {
        assert!(HTML.contains("event.source!==window.parent"));
        assert!(HTML.contains("name:'zeus_status'"));
        for forbidden in [
            "innerHTML",
            "fetch(",
            "XMLHttpRequest",
            "localStorage",
            "document.cookie",
            "approvals.decide",
            "zeus_control",
            "openLink",
            "REPLACE_SCRIPT_HASH",
        ] {
            assert!(!HTML.contains(forbidden), "{forbidden}");
        }
        assert!(HTML.contains("connect-src 'none'"));
        assert!(HTML.contains("sha256-"));
        let meta = serde_json::to_value(metadata()).unwrap();
        assert_eq!(meta["ui"]["csp"]["connectDomains"], json!([]));
        let content = serde_json::to_value(contents()).unwrap();
        assert_eq!(content["mimeType"], MIME);
        assert_eq!(content["uri"], MONITOR_URI);
    }
}
