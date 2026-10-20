//! A desktop daemon client. Credentials and original URLs stay in the Rust process.
#![forbid(unsafe_code)]

pub mod bridge;

/// Start the desktop system webview with the fixed command registry.
#[cfg(feature = "desktop")]
pub fn run() {
    tauri::Builder::default()
        .manage(bridge::Bridge::default())
        .setup(|app| {
            let config = app.config().app.windows.first().ok_or_else(|| {
                std::io::Error::other("the bundled main window configuration is missing")
            })?;
            tauri::WebviewWindowBuilder::from_config(app, config)?
                .on_navigation(|url| local_navigation(url.scheme(), url.host_str()))
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::workspace_snapshot,
            commands::workspace_page,
            commands::preview_links,
            commands::enqueue_review,
            commands::control_job,
        ])
        .run(tauri::generate_context!())
        .expect("the desktop system webview could not start");
}

#[cfg(any(feature = "desktop", test))]
fn local_navigation(scheme: &str, host: Option<&str>) -> bool {
    matches!(
        (scheme, host),
        ("tauri", Some("localhost")) | ("http" | "https", Some("tauri.localhost"))
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn remote_documents_and_file_urls_cannot_navigate_the_main_window() {
        assert!(super::local_navigation("tauri", Some("localhost")));
        assert!(super::local_navigation("http", Some("tauri.localhost")));
        assert!(!super::local_navigation("https", Some("example.org")));
        assert!(!super::local_navigation(
            "https",
            Some("tauri.localhost.example.org")
        ));
        assert!(!super::local_navigation("http", Some("localhost")));
        assert!(!super::local_navigation("file", None));
    }
}

#[cfg(feature = "desktop")]
mod commands {
    use crate::bridge::{Bridge, BridgeError, JobAction, PageView, Section, Snapshot};
    use serde_json::Value;
    use tauri::State;

    #[tauri::command]
    pub async fn workspace_snapshot(state: State<'_, Bridge>) -> Result<Snapshot, BridgeError> {
        state.snapshot().await
    }

    #[tauri::command]
    pub async fn workspace_page(
        state: State<'_, Bridge>,
        section: Section,
        cursor: Option<String>,
    ) -> Result<PageView, BridgeError> {
        state.page(section, cursor).await
    }

    #[tauri::command]
    pub async fn preview_links(
        state: State<'_, Bridge>,
        urls: Vec<String>,
        destination: Option<String>,
        generation: u64,
    ) -> Result<Value, BridgeError> {
        state.preview_links(urls, destination, generation).await
    }

    #[tauri::command]
    pub async fn enqueue_review(
        state: State<'_, Bridge>,
        review_id: u64,
        idempotency_key: String,
        generation: u64,
    ) -> Result<Value, BridgeError> {
        state
            .enqueue_review(review_id, idempotency_key, generation)
            .await
    }

    #[tauri::command]
    pub async fn control_job(
        state: State<'_, Bridge>,
        job_id: String,
        action: JobAction,
        idempotency_key: String,
        generation: u64,
    ) -> Result<Value, BridgeError> {
        state
            .control_job(job_id, action, idempotency_key, generation)
            .await
    }
}
