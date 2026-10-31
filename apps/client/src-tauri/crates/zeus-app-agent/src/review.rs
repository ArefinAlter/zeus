//! One-use confirmation from the trusted desktop, bound to a suspended tool call.
//! Models receive neither this route nor a method for producing a decision.
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Decision {
    pub review_id: String,
    pub approve: bool,
}

struct Pending {
    id: String,
    expires: Instant,
    sender: oneshot::Sender<bool>,
}

pub(crate) struct Ticket {
    pub id: String,
    expires: Instant,
    receiver: oneshot::Receiver<bool>,
}

pub(crate) struct Gate {
    pending: Mutex<Option<Pending>>,
    lifetime: Duration,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            pending: Mutex::new(None),
            lifetime: Duration::from_secs(300),
        }
    }
}

impl Gate {
    pub fn begin(&self) -> Result<Ticket, &'static str> {
        let mut pending = self.pending.lock().map_err(|_| "review unavailable")?;
        if pending.is_some() {
            return Err("another call is waiting for review");
        }
        let mut random = [0u8; 16];
        rand::fill(&mut random);
        let id = hex::encode(random);
        let expires = Instant::now() + self.lifetime;
        let (sender, receiver) = oneshot::channel();
        *pending = Some(Pending {
            id: id.clone(),
            expires,
            sender,
        });
        Ok(Ticket {
            id,
            expires,
            receiver,
        })
    }

    pub fn decide(&self, decision: Decision) -> Result<(), &'static str> {
        let mut pending = self.pending.lock().map_err(|_| "review unavailable")?;
        let current = pending.as_ref().ok_or("this review is no longer pending")?;
        if current.id != decision.review_id {
            return Err("this review does not belong to this call");
        }
        if Instant::now() >= current.expires {
            pending.take();
            return Err("this review has expired");
        }
        let Some(current) = pending.take() else {
            return Err("this review is no longer pending");
        };
        current
            .sender
            .send(decision.approve)
            .map_err(|_| "this call is no longer waiting")
    }

    pub async fn wait(
        &self,
        ticket: Ticket,
        cancel: &CancellationToken,
    ) -> Result<(), &'static str> {
        let result = if Instant::now() >= ticket.expires {
            Err("the review expired; nothing was changed")
        } else {
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err("stopped before approval; nothing was changed"),
                response = tokio::time::timeout_at(tokio::time::Instant::from_std(ticket.expires), ticket.receiver) => match response {
                    Ok(Ok(true)) => Ok(()),
                    Ok(Ok(false)) => Err("you declined this action; nothing was changed"),
                    Ok(Err(_)) => Err("the review closed; nothing was changed"),
                    Err(_) => Err("the review expired; nothing was changed"),
                },
            }
        };
        if let Ok(mut pending) = self.pending.lock()
            && pending.as_ref().is_some_and(|p| p.id == ticket.id)
        {
            pending.take();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decision(id: &str, approve: bool) -> Decision {
        Decision {
            review_id: id.into(),
            approve,
        }
    }

    #[tokio::test]
    async fn approval_is_pending_exact_and_single_use() {
        let gate = Gate::default();
        let mut ticket = gate.begin().unwrap();
        let id = ticket.id.clone();
        assert!(matches!(
            ticket.receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(gate.decide(decision("wrong-call", true)).is_err());
        assert!(gate.begin().is_err());
        gate.decide(decision(&id, true)).unwrap();
        assert!(gate.decide(decision(&id, true)).is_err());
        assert!(gate.wait(ticket, &CancellationToken::new()).await.is_ok());
        assert_ne!(gate.begin().unwrap().id, id);
    }

    #[tokio::test]
    async fn decline_expiry_and_stop_never_authorize() {
        let gate = Gate::default();
        let ticket = gate.begin().unwrap();
        gate.decide(decision(&ticket.id, false)).unwrap();
        assert!(gate.wait(ticket, &CancellationToken::new()).await.is_err());
        let expired = Gate {
            pending: Mutex::new(None),
            lifetime: Duration::ZERO,
        };
        let ticket = expired.begin().unwrap();
        assert!(expired.decide(decision(&ticket.id, true)).is_err());
        assert!(
            expired
                .wait(ticket, &CancellationToken::new())
                .await
                .is_err()
        );
        let ticket = gate.begin().unwrap();
        gate.decide(decision(&ticket.id, true)).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(gate.wait(ticket, &cancel).await.is_err());
    }

    #[tokio::test]
    async fn a_different_run_cannot_consume_another_review() {
        let first = Gate::default();
        let second = Gate::default();
        let ticket = first.begin().unwrap();
        let other = second.begin().unwrap();
        assert!(second.decide(decision(&ticket.id, true)).is_err());
        second.decide(decision(&other.id, false)).unwrap();
        assert!(second.wait(other, &CancellationToken::new()).await.is_err());
    }

    #[test]
    fn the_desktop_cannot_replace_arguments_in_a_decision() {
        assert!(
            serde_json::from_value::<Decision>(
                serde_json::json!({"review_id":"x","approve":true,"arguments":{"url":"changed"}})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<Decision>(serde_json::json!({"review_id":"x"})).is_err());
    }
}
