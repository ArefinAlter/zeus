//! Generate the closed desktop command permissions when the system webview is enabled.

fn main() {
    #[cfg(feature = "desktop")]
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "workspace_snapshot",
            "workspace_page",
            "preview_links",
            "enqueue_review",
            "control_job",
        ]),
    ))
    .expect("desktop configuration must be valid");
}
