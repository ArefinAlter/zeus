//! Embeds the packaged browser extension (made by `apps/client/scripts/package-extension.mjs`) so the installer carries
//! it and the app can unpack it. When the package has not been built, empty placeholders are embedded and the app says
//! plainly that this build carries no extension.

use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default());
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default())
        .join("..")
        .join("..")
        .join("resources")
        .join("extension");
    println!("cargo:rerun-if-changed={}", dir.display());
    for name in [
        "zeus-extension-chromium.zip",
        "zeus-extension-firefox.zip",
        "extension.json",
    ] {
        let from = dir.join(name);
        println!("cargo:rerun-if-changed={}", from.display());
        let to = out.join(name);
        let ok = from.is_file() && std::fs::copy(&from, &to).is_ok();
        if !ok {
            let empty: &[u8] = if name.ends_with(".json") { b"{}" } else { b"" };
            if let Err(e) = std::fs::write(&to, empty) {
                eprintln!("cannot write the extension placeholder {name}: {e}");
                std::process::exit(1);
            }
        }
    }
}
