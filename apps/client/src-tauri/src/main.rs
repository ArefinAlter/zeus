//! The Zeus desktop app.
//!
//! One window on the bundled pages, and the engine behind it: on first launch the app provisions its own Zeus
//! installation in its per-user data folder, starts the bundled `zeusd` and talks to it with its own restricted
//! credential (`zeus-app`). The page reaches the engine through one closed command, `backend_call`, whose route table
//! lives in `zeus-app`. The window may navigate only to the bundled pages, and it has no file, shell, HTTP or opener
//! access of its own.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use serde_json::Value;
use tauri::{Manager, RunEvent, State};
use zeus_app::App;
use zeus_app::failure::Failure;
use zeus_app::home::AppHome;
use zeus_app::supervisor::bin_dir;

/// Whether the main window may navigate to a URL: only the bundled pages (the custom protocol on each platform, and
/// the development server in debug builds).
fn local_navigation(scheme: &str, host: Option<&str>, port: Option<u16>) -> bool {
    match (scheme, host) {
        ("tauri", Some("localhost")) | ("http" | "https", Some("tauri.localhost")) => true,
        ("http", Some("localhost")) => cfg!(debug_assertions) && port == Some(5174),
        _ => false,
    }
}

/// The engine, shared with every command.
struct Backend(Arc<App>);

/// The one command the page may call: a route of the closed table in `zeus-app`.
#[tauri::command]
async fn backend_call(
    state: State<'_, Backend>,
    route: String,
    body: Option<Value>,
) -> Result<Value, Failure> {
    state.0.call(&route, body.unwrap_or(Value::Null)).await
}

fn start_engine() -> Result<Arc<App>, Failure> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
    let home = AppHome::resolve(exe_dir.as_deref())?;
    let bin = bin_dir()?;
    tauri::async_runtime::block_on(async { App::start(home, bin) })
}

fn main() {
    let app = match start_engine() {
        Ok(app) => app,
        Err(f) => {
            eprintln!("zeus: the engine could not be prepared: {f}");
            std::process::exit(1);
        }
    };
    let data_dir = app.home.webview_dir();
    let built = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .manage(Backend(app))
        .invoke_handler(tauri::generate_handler![backend_call])
        .setup(move |app| {
            let engine = app.state::<Backend>().0.clone();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                engine.quit.notified().await;
                handle.exit(0);
            });
            let config = app
                .config()
                .app
                .windows
                .first()
                .ok_or_else(|| {
                    std::io::Error::other("the bundled main window configuration is missing")
                })?
                .clone();
            tauri::WebviewWindowBuilder::from_config(app, &config)?
                .data_directory(data_dir.clone())
                .on_navigation(|url| local_navigation(url.scheme(), url.host_str(), url.port()))
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!());
    let built = match built {
        Ok(b) => b,
        Err(e) => {
            eprintln!("zeus: the window could not start: {e}");
            std::process::exit(1);
        }
    };
    built.run(|handle, event| {
        if let RunEvent::Exit = event {
            let backend = handle.state::<Backend>();
            tauri::async_runtime::block_on(backend.0.shutdown());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::local_navigation;

    #[test]
    fn only_bundled_pages_are_reachable() {
        assert!(local_navigation("tauri", Some("localhost"), None));
        assert!(local_navigation("https", Some("tauri.localhost"), None));
        assert!(!local_navigation("https", Some("example.org"), None));
        assert!(!local_navigation("file", None, None));
        assert!(!local_navigation("http", Some("localhost"), Some(8080)));
    }
}
