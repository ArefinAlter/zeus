//! `zeus-app-http`: the app engine behind a loopback HTTP harness, for driving the interface in a browser.
//!
//! `ZEUS_APP_HOME` (an absolute path) is required so a test run never touches a real profile. Prints one JSON line
//! with the address and the bearer token, then runs until stopped; the engine is stopped cleanly on Ctrl-C.

use std::net::SocketAddr;

use zeus_app::App;
use zeus_app::home::AppHome;
use zeus_app::supervisor::bin_dir;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("ZEUS_APP_HOME").is_none() {
        eprintln!("zeus-app-http: set ZEUS_APP_HOME to a scratch folder");
        std::process::exit(2);
    }
    let home = AppHome::resolve(None).map_err(|e| e.to_string())?;
    let app = App::start(home, bin_dir().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let mut token = [0u8; 16];
    rand::fill(&mut token[..]);
    let token = hex::encode(token);
    let port: u16 = std::env::var("ZEUS_HARNESS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(0);
    let origins: Vec<String> = std::env::var("ZEUS_HARNESS_ORIGINS")
        .unwrap_or_else(|_| "http://localhost:5174,http://127.0.0.1:5174,http://localhost:4173,http://127.0.0.1:4173".into())
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    let addr = zeus_app::http::serve(app.clone(), SocketAddr::from(([127, 0, 0, 1], port)), token.clone(), origins).await?;
    println!("{{\"url\":\"http://{addr}\",\"token\":\"{token}\"}}");
    tokio::signal::ctrl_c().await?;
    app.shutdown().await;
    Ok(())
}
