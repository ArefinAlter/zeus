//! "Connect your AI assistant": the exact configuration for each assistant, and a real handshake test.
//!
//! An assistant starts the bundled `zeus mcp` (an MCP server over standard input and output). `zeus mcp` is an ordinary
//! agent client of the engine: it holds the installation's `agent` credential, never an approval decision, and never
//! sees a browser session. It finds the engine through the same `ZEUS_*` variables the app uses, so each configuration
//! carries them.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use zeus_app::App;
use zeus_app::failure::{Failure, Result};

/// Where `zeus` is, and the environment that selects this app's engine.
fn server(app: &App) -> (String, Vec<(String, String)>) {
    let exe = app
        .supervisor
        .bin_dir()
        .join(format!("zeus{}", std::env::consts::EXE_SUFFIX))
        .display()
        .to_string();
    (exe, app.home.zeus_env())
}

fn quote_shell(windows: bool, s: &str) -> String {
    if s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || "'\"$`\\&|;<>()*?[]{}!#~".contains(c))
    {
        if windows {
            format!("\"{}\"", s.replace('"', "\\\""))
        } else {
            format!("'{}'", s.replace('\'', "'\\''"))
        }
    } else {
        s.to_owned()
    }
}

fn toml_string(s: &str) -> String {
    if s.contains('\'') || s.contains('\n') {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("'{s}'")
    }
}

/// The configurations for Claude Code, Cursor, Codex and a generic stdio entry.
pub fn config(app: &App, windows: bool) -> Value {
    let (exe, env) = server(app);
    let env_json: serde_json::Map<String, Value> =
        env.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let entry = json!({"command": exe, "args": ["mcp"], "env": env_json});
    let claude_cli = {
        // Options come before the server name, the command after `--` (Claude Code's own documented order).
        let mut parts = vec!["claude mcp add --transport stdio --scope user".to_owned()];
        for (k, v) in &env {
            parts.push(format!(
                "--env {}",
                quote_shell(windows, &format!("{k}={v}"))
            ));
        }
        parts.push(format!("zeus -- {} mcp", quote_shell(windows, &exe)));
        parts.join(" ")
    };
    let claude_json = serde_json::to_string_pretty(&json!({"mcpServers": {"zeus": {"type": "stdio", "command": exe, "args": ["mcp"], "env": env_json}}})).unwrap_or_default();
    let cursor_json =
        serde_json::to_string_pretty(&json!({"mcpServers": {"zeus": entry}})).unwrap_or_default();
    let codex_toml = {
        let envs = env
            .iter()
            .map(|(k, v)| format!("{k} = {}", toml_string(v)))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "[mcp_servers.zeus]\ncommand = {}\nargs = [\"mcp\"]\nenv = {{ {envs} }}\n",
            toml_string(&exe)
        )
    };
    let generic = serde_json::to_string_pretty(&entry).unwrap_or_default();
    json!({
        "command": exe,
        "args": ["mcp"],
        "env": env_json,
        "claude_code_cli": claude_cli,
        "claude_code_json": claude_json,
        "cursor_json": cursor_json,
        "codex_toml": codex_toml,
        "generic_json": generic,
        "paths": {
            "claude_code_json": ".mcp.json in your project folder, or ~/.claude.json through the command above",
            "cursor_json": "~/.cursor/mcp.json (all projects) or .cursor/mcp.json (one project)",
            "codex_toml": "~/.codex/config.toml",
        },
    })
}

async fn read_response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: i64,
    deadline: Instant,
) -> Result<Value> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = tokio::time::timeout(left, lines.next_line())
            .await
            .map_err(|_| Failure::new("mcp.timeout", "zeus mcp did not answer in time."))?
            .map_err(|_| Failure::new("mcp.io", "zeus mcp closed its output."))?
            .ok_or_else(|| Failure::new("mcp.closed", "zeus mcp ended before it answered."))?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("id").and_then(Value::as_i64) == Some(id) {
            if let Some(e) = v.get("error") {
                return Err(Failure::new(
                    "mcp.error",
                    format!(
                        "zeus mcp refused: {}",
                        e.get("message").and_then(Value::as_str).unwrap_or("error")
                    ),
                ));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

/// Start `zeus mcp` the way an assistant does and run initialize, initialized and tools/list.
pub async fn test(app: &App) -> Result<Value> {
    let (exe, env) = server(app);
    if !Path::new(&exe).is_file() {
        return Err(Failure::new(
            "mcp.missing",
            format!("The Zeus command line was not found at {exe}."),
        ));
    }
    let started = Instant::now();
    let mut cmd = Command::new(&exe);
    cmd.arg("mcp")
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let mut child = cmd.spawn().map_err(|e| {
        Failure::new(
            "mcp.spawn",
            format!("zeus mcp could not be started ({}).", e.kind()),
        )
    })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Failure::new("mcp.io", "no input pipe"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Failure::new("mcp.io", "no output pipe"))?;
    let mut lines = BufReader::new(stdout).lines();
    let deadline = Instant::now() + Duration::from_secs(25);
    let send = |v: Value| format!("{v}\n");
    stdin
        .write_all(send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "zeus-app-test", "version": env!("CARGO_PKG_VERSION")}}})).as_bytes())
        .await
        .map_err(|_| Failure::new("mcp.io", "zeus mcp closed its input."))?;
    let init = match read_response(&mut lines, 1, deadline).await {
        Ok(v) => v,
        Err(f) => {
            let tail = stderr_tail(&mut child).await;
            return Err(Failure::new(&f.code, format!("{}{tail}", f.message)));
        }
    };
    stdin
        .write_all(
            send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).as_bytes(),
        )
        .await
        .map_err(|_| Failure::new("mcp.io", "zeus mcp closed its input."))?;
    stdin
        .write_all(
            send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}))
                .as_bytes(),
        )
        .await
        .map_err(|_| Failure::new("mcp.io", "zeus mcp closed its input."))?;
    let list = read_response(&mut lines, 2, deadline).await?;
    drop(stdin);
    let _ = child.kill().await;
    let tools: Vec<Value> = list
        .get("tools")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|t| {
                    json!({
                        "name": t.get("name"),
                        "title": t.pointer("/annotations/title").or_else(|| t.get("title")),
                        "read_only": t.pointer("/annotations/readOnlyHint"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "ok": !tools.is_empty(),
        "server": init.get("serverInfo"),
        "protocol_version": init.get("protocolVersion"),
        "tool_count": tools.len(),
        "tools": tools,
        "ms": started.elapsed().as_millis() as u64,
    }))
}

async fn stderr_tail(child: &mut tokio::process::Child) -> String {
    use tokio::io::AsyncReadExt;
    let Some(mut err) = child.stderr.take() else {
        return String::new();
    };
    let _ = child.kill().await;
    let mut text = String::new();
    let _ = tokio::time::timeout(Duration::from_millis(500), err.read_to_string(&mut text)).await;
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.is_empty() {
        String::new()
    } else {
        format!(" ({})", line.chars().take(200).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quoting_follows_the_platform() {
        assert_eq!(
            quote_shell(true, "C:\\Program Files\\Zeus\\zeus.exe"),
            "\"C:\\Program Files\\Zeus\\zeus.exe\""
        );
        assert_eq!(
            quote_shell(false, "/opt/it's here/zeus"),
            "'/opt/it'\\''s here/zeus'"
        );
        assert_eq!(quote_shell(false, "zeus-app"), "zeus-app");
    }

    #[test]
    fn toml_strings_prefer_literals() {
        assert_eq!(toml_string("C:\\x\\zeus.exe"), "'C:\\x\\zeus.exe'");
        assert_eq!(toml_string("it's"), "\"it's\"");
    }
}
