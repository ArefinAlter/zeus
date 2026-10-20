//! Shared connection admission and drain deadline for an orderly server shutdown.
use super::ReplyFence;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// The accepted shutdown request, activated after its RPC response is queued.
#[derive(Debug, Clone)]
pub struct ShutdownNotice {
    /// Notification reason, such as `shutdown_requested` or `signal`.
    pub reason: String,
    /// Accepted grace period in milliseconds.
    pub grace_ms: u64,
    /// Monotonic deadline for in-flight requests and connection reads.
    pub deadline: tokio::time::Instant,
}

#[derive(Debug)]
struct State {
    requested: Mutex<Option<(u64, String)>>,
    stopping: watch::Sender<Option<ShutdownNotice>>,
}

/// Coordinates a listener and all of its connections without aborting work before its grace period.
/// The first accepted request owns the deadline; later requests cannot extend it.
#[derive(Debug, Clone)]
pub struct ShutdownController(Arc<State>);
impl Default for ShutdownController {
    fn default() -> Self {
        Self(Arc::new(State {
            requested: Mutex::new(None),
            stopping: watch::channel(None).0,
        }))
    }
}
impl ShutdownController {
    /// Reserve shutdown and activate it after `reply` is queued. No reply fence is needed for OS signals.
    /// A disconnected caller cannot stall shutdown indefinitely: the transport's 30-second write limit applies.
    pub fn request(&self, grace_ms: u64, reason: &str, reply: Option<ReplyFence>) -> u64 {
        let grace_ms = grace_ms.min(60_000);
        {
            let mut requested = self
                .0
                .requested
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((accepted, _)) = &*requested {
                return *accepted;
            }
            *requested = Some((grace_ms, reason.to_owned()));
        }
        if let Some(reply) = reply {
            let controller = self.clone();
            tokio::spawn(async move {
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(30), reply.wait()).await;
                controller.activate();
            });
        } else {
            self.activate();
        }
        grace_ms
    }
    fn activate(&self) {
        let request = self
            .0
            .requested
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some((grace_ms, reason)) = request {
            self.0.stopping.send_if_modified(|state| {
                if state.is_some() {
                    return false;
                }
                *state = Some(ShutdownNotice {
                    reason,
                    grace_ms,
                    deadline: tokio::time::Instant::now()
                        + std::time::Duration::from_millis(grace_ms),
                });
                true
            });
        }
    }
    /// Current activated stop request; absent until the initiating response is queued.
    pub fn notice(&self) -> Option<ShutdownNotice> {
        self.0.stopping.borrow().clone()
    }
    /// Wait for an activated stop request. Late subscribers observe it immediately.
    pub async fn stopping(&self) -> ShutdownNotice {
        let mut receiver = self.subscribe();
        loop {
            if let Some(notice) = receiver.borrow().clone() {
                return notice;
            }
            let _ = receiver.changed().await;
        }
    }
    pub(super) fn subscribe(&self) -> watch::Receiver<Option<ShutdownNotice>> {
        self.0.stopping.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reply_fence_precedes_stop_and_duplicate_requests_cannot_extend_grace() {
        let controller = ShutdownController::default();
        let reply = ReplyFence::default();
        assert_eq!(
            controller.request(25, "shutdown_requested", Some(reply.clone())),
            25
        );
        assert_eq!(controller.request(60_000, "signal", None), 25);
        tokio::task::yield_now().await;
        assert!(controller.notice().is_none());
        reply.release();
        let notice = tokio::time::timeout(std::time::Duration::from_secs(1), controller.stopping())
            .await
            .unwrap();
        assert_eq!(notice.grace_ms, 25);
        assert_eq!(notice.reason, "shutdown_requested");
        assert_eq!(controller.request(60_000, "signal", None), 25);
        assert_eq!(controller.notice().unwrap().deadline, notice.deadline);
    }
}
