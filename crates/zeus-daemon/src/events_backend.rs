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
    pub(super) fn read_events(
        &self,
        ctx: &CallContext,
        p: EventsReadParams,
    ) -> ApiResult<EventsReadResult> {
        let limit = p.limit.unwrap_or(64);
        if !(1..=128).contains(&limit)
            || p.cursor.as_ref().is_some_and(|c| {
                c.instance_id.len() > 64 || c.seq > zeus_types::limits::MAX_SAFE_INTEGER
            })
        {
            return Err(ApiError::invalid_params("invalid event page bounds"));
        }
        // Head, retention boundary and records come from one journal snapshot. No full-history scan.
        let (meta, reset, start, batch) = self
            .journal
            .read(|tx| {
                let meta = tx.meta()?;
                let reset = p.cursor.as_ref().and_then(|c| {
                    if c.store_id != meta.store_id {
                        Some(EventReset::StoreChanged)
                    } else if c.instance_id != self.info.instance_id {
                        Some(EventReset::DaemonRestarted)
                    } else if c.seq > meta.last_event_seq {
                        Some(EventReset::CursorAhead)
                    } else {
                        None
                    }
                });
                let start = if reset.is_some() {
                    0
                } else {
                    p.cursor.as_ref().map_or(0, |c| c.seq)
                };
                let batch = tx.events_since(EventSeq::new(start).ok(), limit as usize)?;
                Ok((meta, reset, start, batch))
            })
            .map_err(error)?;
        let scope = Self::scope(ctx);
        let mut cursor = start;
        let mut bytes = 0;
        let mut events = Vec::new();
        let mut history_lost = false;
        let empty = batch.is_empty();
        for event in batch {
            let seq = event.seq.ok_or_else(internal)?.get();
            let visible = scope.event(&event, &self.journal);
            let size = if visible {
                serde_json::to_vec(&event).map_err(|_| internal())?.len()
            } else {
                0
            };
            if bytes + size > 512 * 1024 {
                if cursor == start {
                    return Err(internal());
                }
                break;
            }
            history_lost |= seq != cursor.saturating_add(1);
            cursor = seq;
            if visible {
                bytes += size;
                events.push(event);
            }
        }
        // All remaining records may have been pruned. Advance, but expose that loss explicitly.
        if empty && cursor < meta.last_event_seq {
            history_lost = true;
            cursor = meta.last_event_seq;
        }
        Ok(EventsReadResult {
            events,
            cursor: EventCursor {
                store_id: meta.store_id,
                instance_id: self.info.instance_id.clone(),
                seq: cursor,
            },
            head_seq: meta.last_event_seq,
            has_more: cursor < meta.last_event_seq,
            reset,
            history_lost,
        })
    }

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
        // Native subscriptions come from the human extension; API subscriptions follow 05 section 7.5.
        let scope = Self::scope(ctx);
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
                if matches(&event, &p) && scope.event(&event, &self.journal) {
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
                            if matches(&event,&p) && scope.event(&event,&journal) && !deliver(&notifier,&native,&id,&journal,event).await{return;}
                            cursor=seq;
                        }
                        if notifier.is_closed(){break;}
                    },
                    event=progress.next(),if p.include_progress=>{
                        let Some(zeus_engine::events::EngineEvent::Event(event))=event else{continue;};
                        if event.seq.is_some()||!matches(&event,&p)||!scope.event(&event,&journal){continue;}
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

#[cfg(test)]
mod read_tests {
    use super::*;
    use crate::runtime::tests::{context, setup};

    fn append(
        backend: &RuntimeBackend,
        count: usize,
        job: Option<zeus_types::JobId>,
        bytes: usize,
    ) {
        backend
            .journal
            .transact(|tx| {
                for _ in 0..count {
                    let job = job
                        .or_else(|| (bytes > 0).then(|| zeus_types::IdGenerator::new().job_id()));
                    let payload = if job.is_some() {
                        zeus_types::event::JobCompletedData {
                            path: "x".repeat(bytes),
                            size_bytes: 0,
                            sha256: "00".repeat(32).parse().unwrap(),
                            blake3: "11".repeat(32).parse().unwrap(),
                            quarantined: false,
                            duration_ms: 1,
                        }
                        .into()
                    } else {
                        zeus_types::event::DaemonStoppingData {
                            reason: zeus_types::event::StopReason::Signal,
                        }
                        .into()
                    };
                    tx.append_event(Timestamp::now(), job, None, payload)?;
                }
                Ok(())
            })
            .unwrap();
    }

    #[tokio::test]
    async fn bounded_replay_advances_over_invisible_events_without_reporting_false_loss() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (mut ctx, _) = context(dir.path(), "agent");
        let initial = backend
            .read_events(&ctx, EventsReadParams::default())
            .unwrap();
        ctx.identity.kind = zeus_api::ClientKind::Service;
        append(
            &backend,
            5,
            Some(zeus_types::IdGenerator::new().job_id()),
            0,
        );
        append(&backend, 1, None, 0);
        let mut cursor = initial.cursor;
        for i in 0..3 {
            let page = backend
                .read_events(
                    &ctx,
                    EventsReadParams {
                        cursor: Some(cursor.clone()),
                        limit: Some(2),
                    },
                )
                .unwrap();
            assert_eq!(page.cursor.seq, cursor.seq + 2);
            assert_eq!(page.events.len(), usize::from(i == 2));
            assert_eq!(page.has_more, i != 2);
            assert!(!page.history_lost);
            cursor = page.cursor;
        }
        backend.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn replacement_restart_and_future_cursors_reset_instead_of_silently_skipping() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (ctx, _) = context(dir.path(), "agent");
        append(&backend, 3, None, 0);
        let initial = backend
            .read_events(&ctx, EventsReadParams::default())
            .unwrap();
        for expected in [
            EventReset::StoreChanged,
            EventReset::DaemonRestarted,
            EventReset::CursorAhead,
        ] {
            let mut cursor = initial.cursor.clone();
            match expected {
                EventReset::StoreChanged => {
                    cursor.store_id = zeus_types::IdGenerator::new().store_id()
                }
                EventReset::DaemonRestarted => cursor.instance_id = "old".into(),
                EventReset::CursorAhead => cursor.seq += 1,
            }
            let page = backend
                .read_events(
                    &ctx,
                    EventsReadParams {
                        cursor: Some(cursor),
                        limit: None,
                    },
                )
                .unwrap();
            assert_eq!(page.reset, Some(expected));
            assert_eq!(page.events, initial.events);
            assert_eq!(page.cursor, initial.cursor);
        }
        for limit in [0, 129, u32::MAX] {
            assert!(
                backend
                    .read_events(
                        &ctx,
                        EventsReadParams {
                            cursor: None,
                            limit: Some(limit)
                        }
                    )
                    .is_err()
            );
        }
        backend.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retention_and_empty_retained_history_report_actual_loss() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (ctx, _) = context(dir.path(), "agent");
        append(&backend, 6, None, 0);
        let head = backend.journal.meta().unwrap().last_event_seq;
        backend
            .journal
            .transact(|tx| {
                tx.set_cursors(head, head)?;
                tx.set_artifact_cursor(head)?;
                assert_eq!(tx.prune_events(EventSeq::new(head - 1).unwrap())?, head - 1);
                Ok(())
            })
            .unwrap();
        let page = backend
            .read_events(&ctx, EventsReadParams::default())
            .unwrap();
        assert!(page.history_lost);
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.cursor.seq, head);
        backend
            .journal
            .transact(|tx| {
                tx.prune_events(EventSeq::new(head).unwrap())?;
                Ok(())
            })
            .unwrap();
        let page = backend
            .read_events(&ctx, EventsReadParams::default())
            .unwrap();
        assert!(page.history_lost);
        assert!(page.events.is_empty());
        assert_eq!(page.cursor.seq, head);
        assert!(!page.has_more);
        backend.engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn large_events_obey_frame_budget_and_resume_without_loss() {
        let dir = tempfile::tempdir().unwrap();
        let backend = setup(dir.path()).await;
        let (ctx, _) = context(dir.path(), "agent");
        let initial = backend
            .read_events(&ctx, EventsReadParams::default())
            .unwrap();
        append(&backend, 20, None, 60_000);
        let mut cursor = initial.cursor;
        let mut count = 0;
        loop {
            let page = backend
                .read_events(
                    &ctx,
                    EventsReadParams {
                        cursor: Some(cursor.clone()),
                        limit: Some(128),
                    },
                )
                .unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() < 530_000);
            assert!(!page.history_lost);
            assert!(page.cursor.seq > cursor.seq);
            count += page.events.len();
            cursor = page.cursor;
            if !page.has_more {
                break;
            }
        }
        assert_eq!(count, 20);
        backend.engine.shutdown().await.unwrap();
    }
}
