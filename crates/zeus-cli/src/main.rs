//! The `zeus` binary. Data on stdout, diagnostics on stderr, stable exit codes (see `zeus_cli::exit`).

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
#[cfg(not(feature = "mcp-http"))]
use zeus_api::ApiError;
use zeus_api::ApiResult;
use zeus_api::endpoint::ZeusPaths;
use zeus_cli::cli::{Cli, Command};
use zeus_cli::commands::{self, CmdOutput};
use zeus_cli::connect::{Identity, connect};
use zeus_cli::exit::Exit;
use zeus_cli::output::{Output, OutputMode};
use zeus_cli::trust;

fn print(output: &CmdOutput, settings: Output) {
    match settings.mode {
        OutputMode::Json => println!(
            "{}",
            serde_json::to_string_pretty(&output.json).unwrap_or_default()
        ),
        OutputMode::Ndjson => {
            if output.items.is_empty() {
                println!(
                    "{}",
                    serde_json::to_string(&output.json).unwrap_or_default()
                );
            } else {
                output.items.iter().for_each(|item| {
                    println!("{}", serde_json::to_string(item).unwrap_or_default())
                });
            }
        }
        OutputMode::Human => {
            if !settings.quiet && !output.text.is_empty() {
                println!("{}", output.text);
            }
        }
    }
}

async fn run(cli: &Cli, paths: &ZeusPaths, settings: Output) -> ApiResult<CmdOutput> {
    match &cli.command {
        Command::Tui => zeus_cli::tui::run(paths).await.map(|()| CmdOutput::empty()),
        Command::Mcp {
            http: true,
            port,
            token_file,
            init_token,
        } => {
            #[cfg(feature = "mcp-http")]
            {
                let path = token_file
                    .clone()
                    .unwrap_or_else(|| paths.state_dir.join("mcp-http-token.json"));
                if *init_token {
                    zeus_cli::mcp_http::initialize_token(&paths.state_dir)?;
                }
                let hash = zeus_mcp::http_bridge::token_file_hash(&path)?;
                let client = connect(paths, Identity::Agent).await?;
                zeus_mcp::transport::serve_http(Arc::new(client), *port, hash).await?;
                Ok(CmdOutput::empty())
            }
            #[cfg(not(feature = "mcp-http"))]
            {
                let _ = (port, token_file, init_token);
                Err(ApiError::not_implemented(
                    "this build has no mcp-http feature; use stdio or a build with mcp-http",
                ))
            }
        }
        Command::Mcp { .. } => {
            let client = connect(paths, Identity::Agent).await?;
            zeus_mcp::transport::serve_stdio(Arc::new(client))
                .await
                .map(|()| CmdOutput::empty())
        }
        Command::A2a { token_file, port } => {
            let config = zeus_a2a_bridge::Config::from_token_file(
                *port,
                token_file,
                paths.state_dir.join("a2a-index.redb"),
            )?;
            let client = connect(paths, Identity::A2a).await?;
            let bridge = zeus_a2a_bridge::Bridge::open(config, Arc::new(client))?;
            zeus_a2a_bridge::serve(Arc::new(bridge))
                .await
                .map(|()| CmdOutput::empty())
        }
        Command::Auth => {
            let client = connect(paths, Identity::Cli).await?;
            zeus_cli::auth::run(&client).await
        }
        command if !commands::needs_daemon(command) => {
            commands::run_local(command, &cli.global, paths).await
        }
        command => {
            let client = connect(paths, Identity::Cli).await?;
            // The terminal is opened only for approval decisions, and only the controlling terminal: never stdin.
            let mut terminal = if matches!(
                command,
                Command::Approvals(
                    zeus_cli::cli::ApprovalsCommand::Approve { .. }
                        | zeus_cli::cli::ApprovalsCommand::Deny { .. }
                ) | Command::Daemon(
                    zeus_cli::cli::DaemonCommand::KillSwitch
                        | zeus_cli::cli::DaemonCommand::Release
                )
            ) {
                trust::open_terminal().ok()
            } else {
                None
            };
            commands::run_remote(
                command,
                &cli.global,
                &client,
                settings.color,
                terminal.as_mut(),
            )
            .await
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let no_color_env = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let settings = Output::resolve(
        &cli.global,
        no_color_env,
        std::io::stdout().is_terminal(),
        std::io::stderr().is_terminal(),
    );
    let paths = match ZeusPaths::discover_profile(cli.global.profile.as_deref()) {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("{}", commands::failure(&error).0);
            return Exit::Unavailable.into();
        }
    };
    let outcome = tokio::select! {
        outcome = run(&cli, &paths, settings) => outcome,
        _ = tokio::signal::ctrl_c() => return Exit::Interrupted.into(),
    };
    match outcome {
        Ok(output) => {
            print(&output, settings);
            output.exit.into()
        }
        Err(error) => {
            let (text, exit) = commands::failure(&error);
            eprintln!("{text}");
            exit.into()
        }
    }
}
