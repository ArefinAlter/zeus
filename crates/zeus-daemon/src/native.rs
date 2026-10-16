//! Browser protocol handlers sharing the daemon's engine and approval store.
use super::*;
use zeus_session::protocol as nm;

impl RuntimeBackend {
    /// Dispatch one authenticated native messaging frame.
    pub async fn native_frame(&self, ctx: CallContext, params: serde_json::Value) -> ApiResult<()> {
        let frame: zeus_api::methods::notifications::NmFrame = serde_json::from_value(params)
            .map_err(|_| ApiError::invalid_params("invalid native relay envelope"))?;
        let message = nm::ExtensionMessage::from_json(frame.frame)
            .map_err(|_| ApiError::invalid_params("invalid native message"))?;
        if message.v != 1 {
            return Err(ApiError::new(
                "nm.version_unsupported",
                "unsupported native protocol",
            ));
        }
        if let nm::ExtensionBody::Hello(hello) = message.body {
            if message.re.is_some() {
                return Err(ApiError::invalid_params("hello cannot be a response"));
            }
            let verified =
                self.trust.verified(&ctx) && ctx.attach.as_ref().is_some_and(|a| a.parent_verified);
            let offer = zeus_session::negotiate::DaemonOffer::v1(
                &[
                    nm::capability::HANDOFF,
                    nm::capability::SESSION,
                    nm::capability::APPROVALS,
                    nm::capability::EVENTS,
                    nm::capability::JOB_CONTROL,
                    nm::capability::GRANTS,
                    nm::capability::URL_REFRESH_REFETCH,
                    nm::capability::FETCH_PROXY,
                    "kill_switch",
                ],
                nm::DaemonIdentity {
                    version: self.info.version.clone(),
                    instance_id: self.info.instance_id.clone(),
                },
                nm::HostIdentity {
                    version: ctx
                        .attach
                        .as_ref()
                        .map(|a| a.host_version.clone())
                        .unwrap_or_default(),
                },
            );
            let negotiated =
                zeus_session::negotiate::negotiate(&hello, &offer).map_err(ApiError::from)?;
            let connection = self.browser.attach(ctx, hello, verified)?;
            connection.send(
                Some(&message.id),
                zeus_session::negotiate::hello_ack(
                    &negotiated,
                    &offer,
                    connection.ctx.connection_id,
                    Timestamp::now(),
                    self.journal.meta().map_err(error)?.last_event_seq,
                ),
            )?;
            self.engine.refresh_sessions().await.map_err(error)?;
            return Ok(());
        }
        let connection = self.browser.connection(ctx.connection_id)?;
        if matches!(
            &message.body,
            nm::ExtensionBody::FetchHead(_)
                | nm::ExtensionBody::FetchChunk(_)
                | nm::ExtensionBody::FetchEnd(_)
                | nm::ExtensionBody::FetchError(_)
        ) || (matches!(&message.body, nm::ExtensionBody::Error(_))
            && connection.proxy.has_request(message.re.as_ref()))
        {
            return connection
                .proxy
                .handle(&connection, message.re.as_ref(), message.body);
        }
        if let Some(re) = message.re {
            return self.browser.response(ctx.connection_id, &re, message.body);
        }
        if matches!(message.body, nm::ExtensionBody::Bye(_)) {
            self.browser.disconnect(ctx.connection_id);
            return Ok(());
        }
        let reply = self
            .native_request(&connection, &message.id, message.body)
            .await;
        match reply {
            Ok(Some(body)) => connection.send(Some(&message.id), body),
            Ok(None) => Ok(()),
            Err(e) => connection.send(
                Some(&message.id),
                nm::ErrorBody {
                    error: e.envelope().clone(),
                },
            ),
        }
    }
    pub(super) async fn native_request(
        &self,
        c: &crate::browser::BrowserConnection,
        request_id: &nm::MessageId,
        body: nm::ExtensionBody,
    ) -> ApiResult<Option<nm::DaemonBody>> {
        if !matches!(
            body,
            nm::ExtensionBody::Ping(_) | nm::ExtensionBody::Pong(_)
        ) && !c.trusted
        {
            return Err(ApiError::human_required("verified browser"));
        }
        let reply = match body {
            nm::ExtensionBody::Ping(p) => nm::PongBody { nonce: p.nonce }.into(),
            nm::ExtensionBody::Pong(_) => return Ok(None),
            nm::ExtensionBody::ApprovalList(p) => {
                if !c.trusted {
                    return Err(ApiError::human_required("verified browser"));
                }
                let states = p.states.unwrap_or_else(|| vec![ApprovalState::Pending]);
                let approvals = self
                    .journal
                    .read(|tx| tx.approvals(Some(&states)))
                    .map_err(error)?;
                let result = page(
                    approvals,
                    &zeus_api::PageRequest {
                        limit: p.limit,
                        cursor: p.cursor,
                    },
                )?;
                nm::ApprovalListResponseBody {
                    approvals: result.items,
                    next_cursor: result.next_cursor,
                }
                .into()
            }
            nm::ExtensionBody::ApprovalDecide(p) => {
                if !c.trusted
                    || !c
                        .hello
                        .capabilities
                        .iter()
                        .any(|cap| cap == nm::capability::APPROVALS)
                    || !p.ui.user_gesture
                {
                    return Err(ApiError::human_required("verified browser gesture"));
                }
                let channel = match p.ui.surface {
                    nm::ExtensionSurface::ExtensionPopup => Surface::ExtensionPopup,
                    nm::ExtensionSurface::ExtensionOptions => Surface::ExtensionOptions,
                    nm::ExtensionSurface::ExtensionSidepanel => Surface::ExtensionSidepanel,
                    nm::ExtensionSurface::ExtensionNotification => Surface::ExtensionNotification,
                };
                let outcome = if p.decision == nm::Decision::Approve {
                    DecideOutcome::Approve
                } else {
                    DecideOutcome::Deny
                };
                if outcome == DecideOutcome::Approve {
                    self.admit()?;
                }
                let confirmation = match p.confirmation.unwrap_or(nm::ConfirmationLevel::Gesture) {
                    nm::ConfirmationLevel::Gesture => ConfirmationLevel::Gesture,
                    nm::ConfirmationLevel::TypedCode => ConfirmationLevel::TypedCode,
                    nm::ConfirmationLevel::OsAuth => ConfirmationLevel::OsAuth,
                };
                let evidence = serde_json::json!({"surface":channel,"role":c.ctx.identity.role,"client_id":c.ctx.identity.credential_id.as_str(),
                    "credential_kind":c.ctx.identity.kind,"effective_kind":c.ctx.identity.kind,"exe_verified":true,"peer_pid":c.ctx.peer.pid,
                    "tty":"not_applicable","confirmation":confirmation,"extension":{"browser":c.hello.browser.name,
                    "attested_extension":c.hello.extension_id,"profile_id":c.hello.profile_id,"connection_id":c.ctx.connection_id,"user_gesture":true,"parent_verified":true}});
                let (approval, grant) = self
                    .consent
                    .decide(
                        &c.ctx.identity,
                        &ApprovalsDecideParams {
                            approval_id: p.approval_id,
                            request_hash: p.request_hash,
                            outcome,
                            scope: p.scope,
                            channel,
                            confirmation,
                            idempotency_key: None,
                        },
                        evidence,
                    )
                    .await?;
                c.send(
                    Some(request_id),
                    nm::ApprovalDecideResponseBody {
                        approval: approval.clone(),
                        grant: grant.clone(),
                    },
                )?;
                if approval.job_id.is_some() {
                    self.engine
                        .apply_approval(zeus_engine::api::ApprovalResolution {
                            approval: approval.clone(),
                            grant: grant.clone(),
                        })
                        .await
                        .map_err(error)?;
                }
                return Ok(None);
            }
            nm::ExtensionBody::HandoffOffer(p) => {
                self.admit()?;
                if !c.trusted {
                    return Err(ApiError::human_required("verified browser handoff"));
                }
                if p.download
                    .interrupt_reason
                    .is_some_and(|r| r.rescue_offer() == nm::RescueOffer::Never)
                    || (p.trigger == nm::OfferTrigger::Interrupted
                        && p.download.interrupt_reason.is_none())
                {
                    return Err(ApiError::new(
                        "api.permission_denied",
                        "browser security and user-cancellation decisions cannot be rescued",
                    ));
                }
                if p.download.incognito {
                    return Err(ApiError::invalid_params(
                        "private-window handoff is not enabled",
                    ));
                }
                if p.offer_key.is_empty()
                    || p.offer_key.len() > 256
                    || p.download.filename.contains(['/', '\\'])
                {
                    return Err(ApiError::invalid_params("invalid handoff metadata"));
                }
                let spec:JobSpec=serde_json::from_value(serde_json::json!({"url":p.download.url,"hint_url":p.download.final_url,"referrer":p.download.referrer,
                    "filename":p.download.filename,"expected_size":p.download.total_bytes,"session_origin":p.download.url.origin(),
                    "idempotency_key":blake3::hash(format!("handoff:{}:{}",c.hello.profile_id,p.offer_key).as_bytes()).to_hex().to_string(),
                    "source":{"kind":"browser_handoff","via":"native_messaging","client_id":c.ctx.identity.credential_id.as_str(),
                        "browser":{"name":c.hello.browser.name,"extension_id":c.hello.extension_id,"profile_id":c.hello.profile_id,
                            "download_id":p.download.browser_download_id,"offer_key":p.offer_key,"incognito":false}}})).map_err(|_|ApiError::invalid_params("invalid handoff job"))?;
                match self
                    .engine
                    .enqueue(zeus_engine::api::EnqueueRequest {
                        spec,
                        caller: Self::caller(&c.ctx),
                    })
                    .await
                {
                    Ok(outcome) => {
                        if let Some(a) = &outcome.approval {
                            c.send(
                                None,
                                nm::ApprovalPendingBody {
                                    approval: a.clone(),
                                },
                            )?;
                        }
                        nm::HandoffAcceptBody {
                            job_id: outcome.job.id,
                            state: outcome.job.state,
                            approval_id: outcome.approval.map(|a| a.id),
                            deduplicated: outcome.deduplicated,
                        }
                        .into()
                    }
                    Err(e) => nm::HandoffRejectBody {
                        error: e.to_envelope(),
                    }
                    .into(),
                }
            }
            nm::ExtensionBody::HandoffWithdraw(p) => {
                let job = self.engine.get(p.job_id).await.map_err(error)?;
                if job.spec.source.browser.as_ref().is_none_or(|b| {
                    b.profile_id.as_ref() != Some(&c.hello.profile_id)
                        || b.offer_key.as_ref() != Some(&p.offer_key)
                }) {
                    return Err(ApiError::new("api.not_found", "handoff not found"));
                }
                self.engine
                    .control(p.job_id, zeus_engine::api::ControlAction::Cancel)
                    .await
                    .map_err(error)?;
                return Ok(None);
            }
            nm::ExtensionBody::JobList(p) => {
                let result = self
                    .jobs_list(
                        &c.ctx,
                        JobsListParams {
                            states: p.states.unwrap_or_default(),
                            page: zeus_api::PageRequest {
                                cursor: p.cursor,
                                limit: p.limit,
                            },
                            ..Default::default()
                        },
                    )
                    .await?;
                nm::JobListResponseBody {
                    jobs: result.items,
                    next_cursor: result.next_cursor,
                }
                .into()
            }
            nm::ExtensionBody::JobControl(p) => {
                let action = match p.action {
                    nm::JobControlAction::Pause => ControlAction::Pause,
                    nm::JobControlAction::Resume => ControlAction::Resume,
                    nm::JobControlAction::Cancel => ControlAction::Cancel,
                    nm::JobControlAction::Retry => ControlAction::Retry,
                };
                let result = self
                    .jobs_control(
                        &c.ctx,
                        JobsControlParams {
                            job_id: p.job_id,
                            action,
                            priority: None,
                            idempotency_key: None,
                        },
                    )
                    .await?;
                nm::JobControlResponseBody { job: result.job }.into()
            }
            nm::ExtensionBody::EventsSubscribe(p) => {
                let mut context = c.ctx.clone();
                context.reply = Default::default();
                let id = nm::SubscriptionId::new(format!(
                    "s{}",
                    zeus_types::IdGenerator::new().next_ulid()
                ))
                .map_err(|_| internal())?;
                let profile = if p.scope == nm::SubscribeScope::Own {
                    Some(c.hello.profile_id.clone())
                } else {
                    None
                };
                let result = self
                    .subscribe_events(
                        &context,
                        SubscribeParams {
                            since_seq: p.since_seq,
                            types: p.types,
                            job_ids: Vec::new(),
                            run_id: None,
                            include_progress: p.include_progress,
                            progress_interval_ms: p.progress_interval_ms.map(u64::from),
                        },
                        Some((
                            self.browser.connection(c.ctx.connection_id)?,
                            id.clone(),
                            profile,
                        )),
                    )
                    .await?;
                c.send(
                    Some(request_id),
                    nm::EventsSubscribeResponseBody {
                        subscription_id: id,
                        head_seq: result.head_seq,
                        replay_count: result.replay_count,
                        replay_truncated: result.replay_truncated,
                    },
                )?;
                context.reply.release();
                return Ok(None);
            }
            nm::ExtensionBody::EventsUnsubscribe(p) => {
                self.events_unsubscribe(
                    &c.ctx,
                    UnsubscribeParams {
                        subscription_id: p.subscription_id.to_string(),
                    },
                )
                .await?;
                return Ok(None);
            }
            nm::ExtensionBody::GrantList(p) => {
                let result = self
                    .sessions_list(
                        &c.ctx,
                        SessionsListParams {
                            page: zeus_api::PageRequest {
                                limit: p.limit,
                                cursor: None,
                            },
                            include_inactive: p.include_inactive,
                            origin: None,
                        },
                    )
                    .await?;
                nm::GrantListResponseBody {
                    grants: result.items,
                }
                .into()
            }
            nm::ExtensionBody::GrantRevoke(p) => {
                let actor = c.ctx.identity.decision_actor(Surface::ExtensionOptions)?;
                nm::GrantRevokeResponseBody {
                    grant: self.revoke(p.grant_id, actor).await?,
                }
                .into()
            }
            nm::ExtensionBody::KillSwitch(p) => {
                if !p.ui.user_gesture {
                    return Err(ApiError::human_required("browser gesture"));
                }
                let surface = match p.ui.surface {
                    nm::ExtensionSurface::ExtensionPopup => Surface::ExtensionPopup,
                    nm::ExtensionSurface::ExtensionOptions => Surface::ExtensionOptions,
                    nm::ExtensionSurface::ExtensionSidepanel => Surface::ExtensionSidepanel,
                    nm::ExtensionSurface::ExtensionNotification => Surface::ExtensionNotification,
                };
                let actor = c.ctx.identity.decision_actor(surface)?;
                let result = self
                    .change_kill_switch(
                        actor,
                        p.action == nm::KillSwitchAction::Engage,
                        p.reason.as_deref(),
                    )
                    .await?;
                nm::KillSwitchResponseBody {
                    engaged: result.engaged,
                    changed: result.changed,
                    grants_revoked: u64::from(result.grants_revoked),
                    jobs_paused: u64::from(result.jobs_paused),
                    approvals_pending: u64::from(result.approvals_pending),
                    replayed: None,
                }
                .into()
            }
            _ => return Err(ApiError::not_implemented("native message capability")),
        };
        Ok(Some(reply))
    }
}
