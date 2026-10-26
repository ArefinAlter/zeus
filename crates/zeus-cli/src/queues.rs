//! Queue commands: list queues, start or stop one, set the speed limit, store a queue from a JSON file.
//!
//! Queues hold downloads until they run (05 section 9.14). Nothing here decides an approval; starting a queue is
//! refused while the kill switch is engaged.
use clap::Subcommand;
use zeus_api::methods::queues::{
    LimiterDef, QueueAction, QueueDef, QueueView, QueuesConfigureParams, QueuesControlParams,
    QueuesListParams, QueuesPutParams,
};
use zeus_api::methods::{QueuesConfigure, QueuesControl, QueuesList, QueuesPut};
use zeus_api::{ApiError, ApiResult, Client, call};

use crate::commands::CmdOutput;
use crate::output::sanitize;

/// Largest queue file the CLI reads.
const MAX_QUEUE_FILE_BYTES: u64 = 16 * 1024;

/// `zeus queues`.
#[derive(Debug, Clone, Subcommand)]
pub enum QueuesCommand {
    /// List the queues, the speed limiter and what each queue is doing.
    Ls,
    /// Run a queue now: it releases its downloads up to its concurrency limit.
    Start {
        /// The queue (for example `main`).
        id: String,
    },
    /// Stop a queue now: its running downloads pause and keep their saved bytes.
    Stop {
        /// The queue.
        id: String,
    },
    /// Set the global speed limit, or turn it off.
    Limit {
        /// KiB per second for everything Zeus downloads.
        #[arg(long, conflicts_with = "off")]
        kib_s: Option<u32>,
        /// Turn the limit off.
        #[arg(long)]
        off: bool,
    },
    /// Create or replace a queue from a JSON file holding a queue definition.
    Put {
        /// The file (at most 16 KiB): {"id": "night", "name": "Night", "max_concurrent": 2, ...}.
        file: std::path::PathBuf,
    },
}

fn describe(q: &QueueView) -> String {
    let state = if q.running { "running" } else { "stopped" };
    let limit = q
        .def
        .limit_kib_s
        .map_or(String::new(), |l| format!("  limit {l} KiB/s"));
    format!(
        "{:<12} {:<8} {} active, {} waiting, up to {} at once{}  {}",
        sanitize(&q.def.id),
        state,
        q.active,
        q.waiting,
        q.def.max_concurrent,
        limit,
        sanitize(&q.def.name)
    )
}

fn read_queue(path: &std::path::Path) -> ApiResult<QueueDef> {
    let unreadable = || ApiError::invalid_params("the queue file cannot be read");
    let metadata = std::fs::metadata(path).map_err(|_| unreadable())?;
    if !metadata.is_file() || metadata.len() > MAX_QUEUE_FILE_BYTES {
        return Err(ApiError::invalid_params(
            "the queue file must be a regular file of at most 16 KiB",
        ));
    }
    serde_json::from_str(&std::fs::read_to_string(path).map_err(|_| unreadable())?).map_err(|e| {
        ApiError::invalid_params(format!("the queue file is not a queue: line {}", e.line()))
    })
}

/// Run a queue command against the daemon.
pub async fn run<C: Client + ?Sized>(
    command: &QueuesCommand,
    client: &C,
    dry_run: bool,
) -> ApiResult<CmdOutput> {
    if dry_run && !matches!(command, QueuesCommand::Ls) {
        return Err(ApiError::invalid_params(
            "queue commands change settings; --dry-run shows nothing for them and nothing was changed",
        ));
    }
    match command {
        QueuesCommand::Ls => {
            let result = call::<QueuesList, _>(client, QueuesListParams {}).await?;
            let mut lines: Vec<String> = result.queues.iter().map(describe).collect();
            lines.push(format!(
                "speed limit: {}",
                if result.limiter.def.enabled {
                    format!(
                        "{} KiB/s{}",
                        result.limiter.def.limit_kib_s,
                        if result.limiter.active_now {
                            ""
                        } else {
                            " (outside its window, not applied now)"
                        }
                    )
                } else {
                    "off".to_owned()
                }
            ));
            if result.kill_switch_engaged {
                lines.push("the kill switch is engaged: no queue releases anything".to_owned());
            }
            let items = result
                .queues
                .iter()
                .map(|q| serde_json::to_value(q).unwrap_or_default())
                .collect();
            Ok(CmdOutput::new(lines.join("\n"), &result).with_items(items))
        }
        QueuesCommand::Start { id } | QueuesCommand::Stop { id } => {
            let start = matches!(command, QueuesCommand::Start { .. });
            let action = if start {
                QueueAction::Start { id: id.clone() }
            } else {
                QueueAction::Stop { id: id.clone() }
            };
            let result = call::<QueuesControl, _>(
                client,
                QueuesControlParams {
                    action,
                    idempotency_key: None,
                },
            )
            .await?;
            let text = result.queue.as_ref().map_or_else(
                || format!("queue {}", sanitize(id)),
                |q| {
                    format!(
                        "{}\n{}",
                        if start { "started" } else { "stopped" },
                        describe(q)
                    )
                },
            );
            Ok(CmdOutput::new(text, &result))
        }
        QueuesCommand::Limit { kib_s, off } => {
            if kib_s.is_none() && !*off {
                return Err(ApiError::invalid_params("give --kib-s <number> or --off"));
            }
            let current = call::<QueuesList, _>(client, QueuesListParams {})
                .await?
                .limiter
                .def;
            let limiter = LimiterDef {
                enabled: !*off,
                limit_kib_s: kib_s.unwrap_or(current.limit_kib_s),
                ..current
            };
            let result = call::<QueuesConfigure, _>(
                client,
                QueuesConfigureParams {
                    limiter: Some(limiter),
                    defaults: None,
                    idempotency_key: None,
                },
            )
            .await?;
            let text = if result.limiter.def.enabled {
                format!("speed limit {} KiB/s", result.limiter.def.limit_kib_s)
            } else {
                "speed limit off".to_owned()
            };
            Ok(CmdOutput::new(text, &result))
        }
        QueuesCommand::Put { file } => {
            let result = call::<QueuesPut, _>(
                client,
                QueuesPutParams {
                    queue: read_queue(file)?,
                    idempotency_key: None,
                },
            )
            .await?;
            Ok(CmdOutput::new(describe(&result.queue), &result))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_file_is_bounded_and_must_be_a_queue() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("q.json");
        std::fs::write(&ok, r#"{"id":"night","name":"Night","max_concurrent":2}"#).unwrap();
        let def = read_queue(&ok).unwrap();
        assert_eq!((def.id.as_str(), def.max_concurrent), ("night", 2));
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "[1]").unwrap();
        assert!(read_queue(&bad).is_err());
        let big = dir.path().join("big.json");
        std::fs::write(&big, vec![b' '; 20_000]).unwrap();
        assert!(read_queue(&big).is_err());
        assert!(read_queue(&dir.path().join("missing.json")).is_err());
    }
}
