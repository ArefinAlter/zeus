//! `zeusd`: the Zeus daemon.
//!
//! Commands: `zeusd init` creates the first-run state (IPC secret and client credentials, idempotent);
//! `zeusd run` starts the persistent daemon; `zeusd trust-clients` reviews an offline client upgrade;
//! `zeusd --version` reports the build.

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

#[tokio::main]
async fn main() -> Result<()> {
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
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(paths.lock_file())
                .context("cannot open daemon lock")?;
            zeus_api::transport::secure_private_file(&paths.lock_file())
                .context("cannot protect daemon lock")?;
            lock.try_lock()
                .context("stop zeusd before initializing trusted clients")?;
            zeus_daemon::trust::TrustedPrograms::initialize_installed(&paths.state_dir).context(
                "trusted client initialization failed; install all package binaries together",
            )?;
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
        Some("run") => {
            let paths = ZeusPaths::discover().context("cannot resolve daemon paths")?;
            zeus_daemon::run::run(paths).await
        }
        Some("trust-clients") => {
            if args.next().is_some() {
                bail!("usage: zeusd trust-clients (no flags or noninteractive override)");
            }
            let paths = ZeusPaths::discover().context("cannot resolve daemon paths")?;
            zeus_daemon::trust::refresh_offline(&paths)
                .context("trusted client review did not complete")
        }
        Some(other) => {
            bail!(
                "unknown command `{other}`; try `zeusd init`, `zeusd run`, `zeusd trust-clients` or `zeusd --version`"
            )
        }
        None => bail!("usage: zeusd <init|run|trust-clients|--version>"),
    }
}
