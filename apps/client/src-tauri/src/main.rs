//! The Zeus client's desktop shell. The page carries everything: the interface and its backend client, which talks
//! over HTTPS to the hosted pre-alpha, over loopback HTTP to your own zeusd through zeus-web-bridge, or to the offline
//! sample data, as chosen in Settings, Backend (the CSP in tauri.conf.json allows exactly those). The shell opens one
//! frameless window on the bundled pages, refuses navigation anywhere else, and grants the page only the window
//! controls of its own title bar. No daemon connection, no file, shell, HTTP or opener access of its own.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Whether the main window may navigate to a URL: only the bundled pages (the custom protocol on each platform, and
/// the development server in debug builds).
fn local_navigation(scheme: &str, host: Option<&str>, port: Option<u16>) -> bool {
    match (scheme, host) {
        ("tauri", Some("localhost")) | ("http" | "https", Some("tauri.localhost")) => true,
        ("http", Some("localhost")) => cfg!(debug_assertions) && port == Some(5174),
        _ => false,
    }
}

fn main() {
    let result = tauri::Builder::default()
        .setup(|app| {
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
                .on_navigation(|url| local_navigation(url.scheme(), url.host_str(), url.port()))
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!());
    if let Err(e) = result {
        eprintln!("zeus: the window could not start: {e}");
        std::process::exit(1);
    }
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
