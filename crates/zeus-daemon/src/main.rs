//! `zeusd`: the Zeus daemon.
//!
//! Commands: `zeusd init` creates the first-run state (IPC secret and client credentials, idempotent);
//! `zeusd run` starts the daemon (not implemented yet: it needs the hardened listener in `zeus_api::transport` and
//! the engine adapter); `zeusd --version`.

use anyhow::{Context, Result, bail};
use zeus_api::endpoint::ZeusPaths;
use zeus_types::Timestamp;

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("ZEUS_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn main() -> Result<()> {
    init_logging();
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") => {
            println!("zeusd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("init") => {
            let paths = ZeusPaths::discover().context("cannot resolve the state directory")?;
            let report = zeus_daemon::provision::provision(&paths, Timestamp::now())
                .context("provisioning failed")?;
            println!("state directory: {}", paths.state_dir.display());
            println!("ipc secret created: {}", report.created_ipc_secret);
            println!(
                "credentials created: {}",
                if report.created_credentials.is_empty() {
                    "none".into()
                } else {
                    report.created_credentials.join(", ")
                }
            );
            Ok(())
        }
        Some("run") => bail!(
            "zeusd run is not implemented yet: the hardened IPC listener (zeus_api::transport::bind) and the engine adapter are pending"
        ),
        Some(other) => {
            bail!("unknown command `{other}`; try `zeusd init`, `zeusd run` or `zeusd --version`")
        }
        None => bail!("usage: zeusd <init|run|--version>"),
    }
}
