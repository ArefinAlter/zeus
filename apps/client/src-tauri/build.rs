//! Generates the Tauri context. The shell has no app commands: the capability grants the main window only the window
//! controls of its own title bar.

fn main() {
    if let Err(e) = tauri_build::try_build(tauri_build::Attributes::new()) {
        // A build script reports through Cargo's diagnostics; this is not runtime logging.
        eprintln!("zeus-client: invalid Tauri configuration: {e:#}");
        std::process::exit(1);
    }
}
