//! Generates the Tauri context. The page has one app command, `backend_call`, plus the window controls of its own
//! title bar; the capability file names exactly those.

fn main() {
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(&["backend_call"]));
    if let Err(e) = tauri_build::try_build(attributes) {
        // A build script reports through Cargo's diagnostics; this is not runtime logging.
        eprintln!("zeus-client: invalid Tauri configuration: {e:#}");
        std::process::exit(1);
    }
}
