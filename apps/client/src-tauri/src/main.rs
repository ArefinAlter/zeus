//! The Zeus desktop app.
//!
//! One window on the bundled pages, and the engine behind it: on first launch the app provisions its own Zeus
//! installation in its per-user data folder, starts the bundled `zeusd` and talks to it with its own restricted
//! credential (`zeus-app`). The page reaches the engine through one closed command, `backend_call`, whose route table
//! lives in `zeus-app`. The window may navigate only to the bundled pages, and it has no file, shell, HTTP or opener
//! access of its own.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, State, WindowEvent};
use zeus_app::App;
use zeus_app::failure::Failure;
use zeus_app::home::AppHome;
use zeus_app::shell::{ShellHooks, ShellSettings};
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

/// The windowing side of the shell: the notification-area icon, start at sign-in and system notifications.
struct Hooks {
    app: AppHandle,
    tray: Mutex<Option<tauri::tray::TrayIcon>>,
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

impl ShellHooks for Hooks {
    fn apply(&self, s: &ShellSettings) -> Result<(), String> {
        let mut notes: Vec<String> = Vec::new();
        {
            use tauri_plugin_autostart::ManagerExt;
            let launcher = self.app.autolaunch();
            let result = if s.launch_at_login {
                launcher.enable()
            } else {
                launcher.disable()
            };
            if let Err(e) = result {
                notes.push(format!("Start at sign-in could not be changed ({e})."));
            }
        }
        let want = s.show_tray;
        let app = self.app.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<Option<tauri::tray::TrayIcon>, String>>();
        let have = self.tray.lock().map(|t| t.is_some()).unwrap_or(false);
        let result = self.app.run_on_main_thread(move || {
            if !want || have {
                let _ = tx.send(Ok(None));
                return;
            }
            let built = (|| -> Result<tauri::tray::TrayIcon, String> {
                let show = MenuItem::with_id(&app, "show", "Show Zeus", true, None::<&str>)
                    .map_err(|e| e.to_string())?;
                let quit = MenuItem::with_id(&app, "quit", "Quit Zeus", true, None::<&str>)
                    .map_err(|e| e.to_string())?;
                let menu = Menu::with_items(&app, &[&show, &quit]).map_err(|e| e.to_string())?;
                let mut builder = TrayIconBuilder::with_id("zeus")
                    .tooltip("Zeus")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id().as_ref() {
                        "show" => show_main(app),
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_main(tray.app_handle());
                        }
                    });
                if let Some(icon) = app.default_window_icon() {
                    builder = builder.icon(icon.clone());
                }
                builder.build(&app).map_err(|e| e.to_string())
            })();
            let _ = tx.send(built.map(Some));
        });
        if result.is_ok() {
            match rx.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok(Ok(Some(icon))) => {
                    if let Ok(mut t) = self.tray.lock() {
                        *t = Some(icon);
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => notes.push(format!(
                    "The notification-area icon could not be shown ({e})."
                )),
                Err(_) => notes.push("The notification-area icon did not appear.".into()),
            }
        }
        if !want
            && let Ok(mut t) = self.tray.lock()
            && let Some(icon) = t.take()
        {
            let _ = icon.set_visible(false);
        }
        if notes.is_empty() {
            Ok(())
        } else {
            Err(notes.join(" "))
        }
    }

    fn notify(&self, title: &str, body: &str) {
        use tauri_plugin_notification::NotificationExt;
        let _ = self
            .app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show();
    }

    fn tooltip(&self, text: &str) {
        if let Ok(t) = self.tray.lock()
            && let Some(icon) = t.as_ref()
        {
            let _ = icon.set_tooltip(Some(text));
        }
    }

    fn tray_available(&self) -> bool {
        self.tray.lock().map(|t| t.is_some()).unwrap_or(false) || !cfg!(target_os = "linux")
    }
}

fn start_engine() -> Result<Arc<App>, Failure> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
    let home = AppHome::resolve(exe_dir.as_deref())?;
    let bin = zeus_app::supervisor::stage_if_ephemeral(&home, bin_dir()?)?;
    tauri::async_runtime::block_on(async {
        let app = App::start(home, bin)?;
        zeus_app_ext::install(&app);
        Ok(app)
    })
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
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None::<Vec<&str>>,
        ))
        .plugin(tauri_plugin_notification::init())
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
            let window = tauri::WebviewWindowBuilder::from_config(app, &config)?
                .data_directory(data_dir.clone())
                .on_navigation(|url| local_navigation(url.scheme(), url.host_str(), url.port()))
                .build()?;
            let backend = app.state::<Backend>().0.clone();
            let settings = backend.shell.settings();
            backend.shell.set_hooks(Arc::new(Hooks {
                app: app.handle().clone(),
                tray: Mutex::new(None),
            }));
            if settings.start_minimized {
                if settings.show_tray {
                    let _ = window.hide();
                } else {
                    let _ = window.minimize();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it to the notification area when the person chose that; the engine keeps running.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let backend = window.state::<Backend>();
                let s = backend.0.shell.settings();
                if s.close_to_tray
                    && s.show_tray
                    && backend.0.shell.view()["tray_available"] == true
                {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
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
