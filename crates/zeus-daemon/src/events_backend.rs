//! Ordered durable replay and bounded progress delivery for both local front ends.
use super::*;
use zeus_session::protocol as nm;
type NativeDestination = Option<(
    Arc<crate::browser::BrowserConnection>,
    nm::SubscriptionId,
    Option<String>,
)>;
fn matches(event: &zeus_types::Event, p: &SubscribeParams) -> bool {
    (p.job_ids.is_empty() || event.job_id.is_some_and(|j| p.job_ids.contains(&j)))
        && p.run_id.is_none_or(|r| event.run_id == Some(r))
        && p.types
            .as_ref()
            .is_none_or(|types| types.iter().any(|t| t == event.payload.type_name()))
}
async fn deliver(
    notifier: &zeus_api::server::Notifier,
    native: &NativeDestination,
    id: &str,
    journal: &Journal,
    event: zeus_types::Event,
) -> bool {
    if let Some((connection, subscription_id, profile)) = native {
        if let (Some(profile), Some(job_id)) = (profile, event.job_id)
            && !journal
                .read(|tx| tx.job(job_id))
                .ok()
                .flatten()
                .is_some_and(|r| {
                    r.job
                        .spec
                        .source
                        .browser
                        .as_ref()
                        .and_then(|b| b.profile_id.as_ref())
                        == Some(profile)
                })
        {
            return true;
        }
        if let zeus_types::EventPayload::ApprovalRequested(data) = &event.payload
            && let Ok(Some(approval)) = journal.read(|tx| tx.approval(data.approval.id))
            && approval.state == ApprovalState::Pending
            && connection
                .send(None, nm::ApprovalPendingBody { approval })
                .is_err()
        {
            return false;
        }
        if event.payload.type_name() == "session.leased" {
            return true;
        }
        let message = nm::DaemonMessage::new(
            nm::MessageId::new(format!("evt{}", zeus_types::IdGenerator::new().next_ulid()))
                .expect("valid event id"),
            nm::JobEventBody {
                subscription_id: subscription_id.clone(),
                event,
            },
        );
        let Ok(frame) = message.to_json() else {
            return false;
        };
        connection
            .ctx
            .notifier
            .notify_wait("nm.frame", serde_json::json!({"frame":frame}))
            .await
            .is_ok()
    } else {
        notifier
            .notify_wait(
                "events.event",
                serde_json::json!({"subscription_id":id,"event":event}),
            )
            .await
            .is_ok()
    }
}
impl RuntimeBackend {
    pub(super) async fn subscribe_events(
        &self,
        ctx: &CallContext,
        p: SubscribeParams,
        native: NativeDestination,
    ) -> ApiResult<SubscribeResult> {
        if p.progress_interval_ms.is_some_and(|n| n < 1000) {
            return Err(ApiError::invalid_params(
                "progress interval must be at least 1000 ms",
            ));
        }
        let mut subscriptions = self.subscriptions.lock().await;
        subscriptions.retain(|_, (_, task)| !task.is_finished());
        if subscriptions
            .values()
            .filter(|(id, _)| id == &ctx.connection_id)
            .count()
            >= ctx.limits.max_subscriptions as usize
        {
            return Err(ApiError::new(
                "api.rate_limited",
                "too many event subscriptions",
            ));
        }
        let head_seq = self.journal.meta().map_err(error)?.last_event_seq;
        let start = p.since_seq.unwrap_or(head_seq);
        if start > head_seq {
            return Err(ApiError::invalid_params(
                "event cursor is ahead of the daemon",
            ));
        }
        let earliest = self
            .journal
            .read(|tx| tx.events_since(None, 1))
            .map_err(error)?
            .first()
            .and_then(|e| e.seq)
            .map(|s| s.get())
            .unwrap_or(head_seq.saturating_add(1));
        let replay_truncated = start.saturating_add(1) < earliest;
        let mut counted = start;
        let mut replay_count = 0;
        while counted < head_seq {
            let batch = self
                .journal
                .read(|tx| tx.events_since(EventSeq::new(counted).ok(), 256))
                .map_err(error)?;
            if batch.is_empty() {
                break;
            }
            for event in batch {
                let seq = event.seq.map_or(0, |s| s.get());
                if seq > head_seq {
                    break;
                }
                counted = seq;
                if matches(&event, &p) {
                    replay_count += 1;
                }
            }
        }
        let subscription_id = native
            .as_ref()
            .map(|(_, id, _)| id.to_string())
            .unwrap_or_else(|| format!("s{}", zeus_types::IdGenerator::new().next_ulid()));
        let id = subscription_id.clone();
        let journal = self.journal.clone();
        let notifier = ctx.notifier.clone();
        let fence = ctx.reply.clone();
        let mut progress = self
            .engine
            .subscribe(EventSeq::new(head_seq).ok(), p.include_progress)
            .map_err(error)?;
        let task = tokio::spawn(async move {
            fence.wait().await;
            let mut cursor = start;
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            let mut progress_at = HashMap::new();
            loop {
                tokio::select! {
                    _=tick.tick()=>{
                        let batch=match journal.read(|tx|tx.events_since(EventSeq::new(cursor).ok(),256)){Ok(batch)=>batch,Err(_)=>break};
                        for event in batch {
                            let Some(seq)=event.seq.map(|s|s.get())else{continue;};
                            if seq>cursor.saturating_add(1) && cursor>=earliest {
                                if let Some((connection,_,_))=&native {let _=connection.send(None,nm::ByeBody{reason:nm::ByeReason::ProtocolError,message:Some("Event history changed; reconnect and refresh the job list".into())});}
                                else{let _=notifier.notify_wait("events.lagged",serde_json::json!({"subscription_id":id,"dropped":seq-cursor-1,"resume_from_seq":cursor})).await;}return;
                            }
                            if matches(&event,&p) && !deliver(&notifier,&native,&id,&journal,event).await{return;}
                            cursor=seq;
                        }
                        if notifier.is_closed(){break;}
                    },
                    event=progress.next(),if p.include_progress=>{
                        let Some(zeus_engine::events::EngineEvent::Event(event))=event else{continue;};
                        if event.seq.is_some()||!matches(&event,&p){continue;}
                        let Some(job)=event.job_id else{continue;};let now=Instant::now();
                        if progress_at.get(&job).is_some_and(|last:&Instant|now.duration_since(*last)<Duration::from_millis(p.progress_interval_ms.unwrap_or(1000))){continue;}
                        if progress_at.len()>=4096 {progress_at.retain(|_,last:&mut Instant|now.duration_since(*last)<Duration::from_secs(60)); if progress_at.len()>=4096 {progress_at.clear();}}
                        progress_at.insert(job,now);
                        if !deliver(&notifier,&native,&id,&journal,event).await{return;}
                    }
                }
            }
        });
        subscriptions.insert(
            subscription_id.clone(),
            (ctx.connection_id, task.abort_handle()),
        );
        Ok(SubscribeResult {
            subscription_id,
            head_seq,
            replay_count,
            replay_truncated,
        })
    }
}
