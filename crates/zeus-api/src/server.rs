//! The server framework: an authorising [`Router`], per-connection [`Notifier`], and [`serve_connection`].
//!
//! The daemon registers typed handlers on a [`Router`]. The router, not the handlers, enforces authorisation from
//! [`crate::policy::METHODS`], deserialises parameters, and maps failures to the error envelope, so a handler
//! cannot forget a check. [`serve_connection`] runs the handshake and the request loop over any duplex byte
//! stream; the daemon hands it accepted local-socket streams, tests hand it in-memory pipes.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{Semaphore, mpsc};
use zeus_types::ConnectionId;

use crate::auth::{CredentialStore, IpcSecret, ServerHandshake};
use crate::error::{ApiError, codes};
use crate::framing::{LineLimits, read_line, write_message};
use crate::jsonrpc::{Message, Notification, Request, Response};
use crate::kind::ClientIdentity;
use crate::methods::RpcMethod;
use crate::methods::daemon::{ConnectionLimits, DaemonInfo};
use crate::policy;

#[path = "server_shutdown.rs"]
mod lifecycle;
pub use lifecycle::{ShutdownController, ShutdownNotice};

/// Why a notification could not be queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NotifyError {
    /// The connection's outgoing queue is full: the subscriber is too slow (`events.lagged` territory).
    #[error("outgoing queue full")]
    Full,
    /// The connection is closed.
    #[error("connection closed")]
    Closed,
}

/// Handle for pushing notifications to one connection. Cloneable; never blocks the caller.
#[derive(Debug, Clone)]
pub struct Notifier {
    tx: mpsc::Sender<Message>,
}

impl Notifier {
    /// Queue a replay or terminal lag notification with a bounded wait for the connection writer.
    pub async fn notify_wait(&self, method: &str, params: Value) -> Result<(), NotifyError> {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.tx.send(Message::Notification(Notification::new(
                method,
                Some(params),
            ))),
        )
        .await
        .map_err(|_| NotifyError::Full)?
        .map_err(|_| NotifyError::Closed)
    }
    /// A notifier and the receiver of what it sends (for tests and for the connection's writer task).
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<Message>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    /// Queue a notification without waiting.
    pub fn notify(&self, method: &str, params: Value) -> Result<(), NotifyError> {
        self.tx
            .try_send(Message::Notification(Notification::new(
                method,
                Some(params),
            )))
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => NotifyError::Full,
                mpsc::error::TrySendError::Closed(_) => NotifyError::Closed,
            })
    }

    /// True when the connection has gone away.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// Barrier ensuring a subscription response is queued before its first notification.
#[derive(Debug, Clone)]
pub struct ReplyFence(tokio::sync::watch::Sender<bool>);
impl Default for ReplyFence {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
}
impl ReplyFence {
    /// Wait until the RPC response has been queued on this same connection.
    pub async fn wait(&self) {
        let _ = self.0.subscribe().wait_for(|sent| *sent).await;
    }
    /// Mark the response queued; only the owning transport invokes this.
    pub fn release(&self) {
        self.0.send_replace(true);
    }
}

/// Everything a handler knows about the call.
#[derive(Debug, Clone)]
pub struct CallContext {
    /// Barrier for notifications which must follow this call's response.
    pub reply: ReplyFence,
    /// Operating-system peer evidence; absent for unverified embedded transports.
    pub peer: crate::transport::PeerProcess,
    /// Attach data authenticated with the native-host credential; used only with verified host evidence.
    pub attach: Option<crate::methods::daemon::AttachInfo>,
    /// The authenticated caller.
    pub identity: ClientIdentity,
    /// This connection, for logs.
    pub connection_id: ConnectionId,
    /// Where to push notifications for this connection (event subscriptions).
    pub notifier: Notifier,
    /// Limits in force.
    pub limits: ConnectionLimits,
    /// Capabilities negotiated in `daemon.hello` (05 section 8.2); embedded callers pass [`crate::capabilities::all`].
    pub capabilities: Arc<std::collections::BTreeSet<String>>,
}

type AuthenticationHook = dyn Fn(&mut CallContext) -> Result<(), ApiError> + Send + Sync;
type BoxedHandler =
    Box<dyn Fn(CallContext, Value) -> BoxFuture<'static, Result<Value, ApiError>> + Send + Sync>;

/// Dispatches authenticated requests to typed handlers.
#[derive(Default)]
pub struct Router {
    routes: HashMap<&'static str, BoxedHandler>,
    native_frames: Option<BoxedHandler>,
    disconnected: Option<BoxedHandler>,
    authenticated: Option<Box<AuthenticationHook>>,
    shutdown: Option<ShutdownController>,
    revocations: Option<tokio::sync::watch::Receiver<u64>>,
    rates: Option<Arc<crate::rate::RateLimiter>>,
}

impl Router {
    /// An empty router: every catalogued method answers `api.not_implemented`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the handler of method `M`.
    #[must_use]
    pub fn route<M, F, Fut>(mut self, handler: F) -> Self
    where
        M: RpcMethod + 'static,
        F: Fn(CallContext, M::Params) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<M::Result, ApiError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let boxed: BoxedHandler = Box::new(move |ctx, params| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let params: M::Params =
                    serde_json::from_value(params).map_err(|e| invalid_params(M::NAME, &e))?;
                let result = handler(ctx, params).await?;
                serde_json::to_value(result)
                    .map_err(|_| ApiError::internal("result could not be serialised"))
            })
        });
        self.routes.insert(M::NAME, boxed);
        self
    }

    /// Handle native frames after role and permission checks; no other notification is dispatched.
    #[must_use]
    pub fn native_messaging<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(CallContext, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ApiError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        self.native_frames = Some(Box::new(move |ctx, params| {
            let handler = handler.clone();
            Box::pin(async move {
                handler(ctx, params).await?;
                Ok(Value::Null)
            })
        }));
        self
    }

    /// Release connection-owned subscriptions and browser leases when the socket closes.
    #[must_use]
    pub fn on_disconnect<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(CallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let handler = Arc::new(handler);
        self.disconnected = Some(Box::new(move |ctx, _| {
            let handler = handler.clone();
            Box::pin(async move {
                handler(ctx).await;
                Ok(Value::Null)
            })
        }));
        self
    }

    /// Apply daemon-owned peer classification once, before any authenticated request can run.
    #[must_use]
    pub fn on_authenticate<F>(mut self, handler: F) -> Self
    where
        F: Fn(&mut CallContext) -> Result<(), ApiError> + Send + Sync + 'static,
    {
        self.authenticated = Some(Box::new(handler));
        self
    }

    /// Close live connections whose credential was revoked: the store bumps `revocations` after each revocation,
    /// and every connection then re-checks its own credential. Requests also re-check before dispatch.
    #[must_use]
    pub fn with_revocations(mut self, revocations: tokio::sync::watch::Receiver<u64>) -> Self {
        self.revocations = Some(revocations);
        self
    }

    /// Apply the per-credential token buckets of 05 section 13.2 (authorisation step 8).
    #[must_use]
    pub fn with_rate_limits(self, limits: crate::rate::RateLimits) -> Self {
        self.with_rate_limiter(Arc::new(crate::rate::RateLimiter::new(limits)))
    }

    /// Apply these token buckets, shared with the daemon, which charges the `session_request` token of each
    /// enqueued job that raises an approval.
    #[must_use]
    pub fn with_rate_limiter(mut self, rates: Arc<crate::rate::RateLimiter>) -> Self {
        self.rates = Some(rates);
        self
    }

    /// Apply the token buckets with rates chosen per caller (`api.rate_limits.*`, 05 section 13.2).
    #[must_use]
    pub fn with_rate_limits_per_caller(mut self, limits_for: Arc<crate::rate::LimitsFor>) -> Self {
        self.rates = Some(Arc::new(crate::rate::RateLimiter::per_caller(limits_for)));
        self
    }

    /// Apply one shared admission and connection-drain deadline to this server.
    #[must_use]
    pub fn with_shutdown(mut self, controller: ShutdownController) -> Self {
        self.shutdown = Some(controller);
        self
    }

    /// Names of the registered methods.
    pub fn registered(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.routes.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// Dispatch one request from an authenticated caller. Authorisation happens here, before the handler runs.
    pub async fn dispatch(&self, ctx: &CallContext, request: Request) -> Response {
        let id = request.id.clone();
        match self.dispatch_inner(ctx, &request).await {
            Ok(value) => Response::success(id, value),
            Err(error) => Response::failure(Some(id), &error),
        }
    }

    async fn dispatch_inner(
        &self,
        ctx: &CallContext,
        request: &Request,
    ) -> Result<Value, ApiError> {
        if self
            .shutdown
            .as_ref()
            .is_some_and(|controller| controller.notice().is_some())
        {
            return Err(ApiError::new(codes::UNAVAILABLE, "daemon is stopping")
                .with_detail("reason", "stopping"));
        }
        let spec = policy::lookup(&request.method).ok_or_else(|| {
            ApiError::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown method `{}`", short(&request.method)),
            )
        })?;
        // 05 section 7.4, steps 3 to 8, in order.
        let handler = self.routes.get(spec.name).ok_or_else(|| {
            ApiError::not_implemented("this method is declared but not available in this build")
        })?;
        let params = request.params_or_empty();
        if !params.is_object() {
            return Err(ApiError::invalid_params("params must be an object"));
        }
        policy::require_capabilities(spec, &params, &ctx.capabilities)?;
        policy::authorize(Some(&ctx.identity), spec, &params)?;
        if let Some(rates) = &self.rates {
            rates.check_identity(&ctx.identity, spec, &params)?;
        }
        handler(ctx.clone(), params).await
    }
}

fn short(text: &str) -> String {
    text.chars().take(64).collect()
}

/// Error for bad parameters. serde messages for values can echo the offending value (a possible secret), so only
/// messages that name a missing or unknown *field* are passed on.
fn invalid_params(method: &str, error: &serde_json::Error) -> ApiError {
    let text = error.to_string();
    let safe = text.starts_with("missing field") || text.starts_with("unknown field");
    let detail = if safe {
        text.split(" at line").next().unwrap_or("").to_string()
    } else {
        format!("{:?}", error.classify())
    };
    ApiError::invalid_params(format!("invalid parameters for {method}: {detail}"))
}

/// Everything a connection needs from the daemon.
pub struct ServerContext {
    /// The IPC secret that proves this daemon's identity to clients.
    pub secret: Arc<IpcSecret>,
    /// Stored credentials.
    pub credentials: Arc<dyn CredentialStore>,
    /// Daemon identity for `daemon.hello`.
    pub daemon: DaemonInfo,
    /// The method handlers.
    pub router: Arc<Router>,
}

/// Run one connection to completion: handshake, then requests until the peer closes or violates the protocol.
///
/// Requests are processed concurrently (at most `max_in_flight_requests`); responses may be reordered and are
/// matched by `id`. A line over the limit, or a request before authentication, ends the connection after one error
/// response. The peer-credential check (same OS user) is the caller's job: it happens on the accepted socket.
pub async fn serve_connection<S>(stream: S, ctx: Arc<ServerContext>) -> Result<(), ApiError>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    serve_connection_with_peer(stream, ctx, Default::default()).await
}

async fn serve_connection_with_peer<S>(
    stream: S,
    ctx: Arc<ServerContext>,
    peer: crate::transport::PeerProcess,
) -> Result<(), ApiError>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let (notifier, mut outgoing) =
        Notifier::channel(ConnectionLimits::STANDARD.event_buffer as usize);

    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if !matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    write_message(&mut write_half, &message)
                )
                .await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
    });

    // Dropping an accept-loop task must also close its detached writer and socket half.
    let _writer_guard = AbortWriter(writer.abort_handle());
    let mut call_ctx = None;
    let outcome = connection_loop(&mut reader, &ctx, &notifier, &mut call_ctx, &peer).await;
    if let (Some(call), Some(handler)) = (call_ctx, &ctx.router.disconnected) {
        let _ = handler(call, Value::Null).await;
    }
    drop(notifier);
    // Give queued replies a bounded opportunity to flush, then release all socket handles.
    let mut writer = writer;
    if tokio::time::timeout(std::time::Duration::from_millis(250), &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
    outcome
}

struct AbortWriter(tokio::task::AbortHandle);
impl Drop for AbortWriter {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn connection_loop<R>(
    reader: &mut BufReader<R>,
    ctx: &Arc<ServerContext>,
    notifier: &Notifier,
    call_ctx: &mut Option<CallContext>,
    peer: &crate::transport::PeerProcess,
) -> Result<(), ApiError>
where
    R: AsyncRead + Unpin,
{
    let mut handshake =
        ServerHandshake::new(&ctx.secret, ctx.credentials.as_ref(), ctx.daemon.clone());
    let mut limits = LineLimits::STANDARD;
    let mut tasks = tokio::task::JoinSet::new();
    let mut in_flight: Option<Arc<Semaphore>> = None;
    let shutdown = ctx.router.shutdown.clone().unwrap_or_default();
    let mut stopping = shutdown.subscribe();
    let mut stopping_notified = false;
    let mut revocations = ctx.router.revocations.clone();
    let revoked = |call: &Option<CallContext>| {
        call.as_ref().is_some_and(|call| {
            ctx.credentials
                .get(&call.identity.credential_id)
                .is_none_or(|c| c.revoked)
        })
    };

    loop {
        while tasks.try_join_next().is_some() {}
        let read = {
            // Keep an incomplete line alive while announcing shutdown; the framing reader may
            // already have consumed bytes and is not safe to cancel and restart mid-frame.
            let read = async {
                let read = read_line(reader, limits.max_request_bytes);
                if call_ctx.is_none() {
                    tokio::time::timeout(std::time::Duration::from_secs(5), read)
                        .await
                        .map_err(|_| {
                            ApiError::new(codes::TIMEOUT, "authentication deadline exceeded")
                        })?
                        .map_err(ApiError::from)
                } else {
                    read.await.map_err(ApiError::from)
                }
            };
            tokio::pin!(read);
            loop {
                let notice = shutdown.notice();
                if let Some(notice) = &notice {
                    if call_ctx.is_none() {
                        return Ok(());
                    }
                    if !stopping_notified {
                        notifier.notify("daemon.stopping", serde_json::json!({"reason":notice.reason,"grace_ms":notice.grace_ms}))
                            .map_err(|_| ApiError::new(codes::SLOW_CONSUMER,"shutdown notification could not be queued"))?;
                        stopping_notified = true;
                    }
                    if tokio::time::Instant::now() >= notice.deadline {
                        return Ok(());
                    }
                }
                tokio::select! {
                    biased;
                    _ = stopping.changed() => continue,
                    _ = async {
                        let changed = match revocations.as_mut() {
                            Some(rx) => rx.changed().await.is_ok(),
                            None => false,
                        };
                        if !changed {
                            std::future::pending::<()>().await;
                        }
                    } => {
                        if revoked(call_ctx) {
                            return Ok(());
                        }
                        continue;
                    },
                    _ = async { match notice { Some(notice) => tokio::time::sleep_until(notice.deadline).await, None => std::future::pending().await } } => return Ok(()),
                    result = &mut read => break result,
                }
            }
        };
        let line = match read {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error) => {
                if error.envelope().code.as_str() == codes::PAYLOAD_TOO_LARGE {
                    let _ =
                        notifier_send(notifier, Message::Response(Response::failure(None, &error)));
                }
                return Err(error);
            }
        };
        let message = match Message::parse(&line) {
            Ok(message) => message,
            Err((id, error)) => {
                let _ = notifier_send(notifier, Message::Response(Response::failure(id, &error)));
                continue;
            }
        };
        let request = match message {
            Message::Request(request) => request,
            Message::Notification(note) => {
                if stopping_notified {
                    continue;
                }
                let call = call_ctx.as_ref().ok_or_else(ApiError::not_authenticated)?;
                if note.method != "nm.frame"
                    || call.identity.role != crate::ClientRole::NativeHost
                    || !call.identity.has(crate::Permission::NmRelay)
                    || !call.capabilities.contains(crate::capabilities::NM_RELAY)
                {
                    return Err(ApiError::new(
                        codes::PERMISSION_DENIED,
                        "notification is not permitted",
                    ));
                }
                if let Some(handler) = &ctx.router.native_frames {
                    handler(call.clone(), note.params.unwrap_or(Value::Null)).await?;
                } else {
                    return Err(ApiError::not_implemented("native messaging endpoint"));
                }
                continue;
            }
            Message::Response(_) => continue,
        };

        let response = match request.method.as_str() {
            crate::methods::names::DAEMON_HELLO => handle_handshake(&mut handshake, &request, true),
            crate::methods::names::DAEMON_AUTHENTICATE => {
                let mut response = handle_handshake(&mut handshake, &request, false);
                if let Some(error) = response.error.clone() {
                    let _ = notifier_send(notifier, Message::Response(response));
                    return Err(ApiError::from_error_object(&error));
                }
                if let Some(identity) = handshake.identity() {
                    let connection_limits = ConnectionLimits::for_role(identity.role);
                    limits = LineLimits {
                        max_request_bytes: connection_limits.max_request_bytes as usize,
                        max_response_bytes: connection_limits.max_response_bytes as usize,
                    };
                    in_flight = Some(Arc::new(Semaphore::new(
                        connection_limits.max_in_flight_requests as usize,
                    )));
                    let credential_kind = identity.kind;
                    let mut call = CallContext {
                        reply: Default::default(),
                        peer: peer.clone(),
                        attach: request
                            .params
                            .clone()
                            .and_then(|p| {
                                serde_json::from_value::<
                                        crate::methods::daemon::AuthenticateParams,
                                    >(p)
                                    .ok()
                            })
                            .and_then(|p| p.attach),
                        identity: identity.clone(),
                        connection_id: connection_id_from(&response),
                        notifier: notifier.clone(),
                        limits: connection_limits,
                        capabilities: Arc::new(handshake.capabilities().clone()),
                    };
                    if let Some(classify) = &ctx.router.authenticated
                        && let Err(error) = classify(&mut call)
                    {
                        let _ = notifier_send(
                            notifier,
                            Message::Response(Response::failure(Some(request.id.clone()), &error)),
                        );
                        return Err(error);
                    }
                    if let Some(result) = response.result.as_mut() {
                        result["credential_kind"] = serde_json::to_value(credential_kind)
                            .map_err(|_| ApiError::internal("identity serialization"))?;
                        result["kind"] = serde_json::to_value(call.identity.kind)
                            .map_err(|_| ApiError::internal("identity serialization"))?;
                        result["downgraded"] =
                            serde_json::json!(credential_kind != call.identity.kind);
                        result["permissions"] = serde_json::to_value(&call.identity.permissions)
                            .map_err(|_| ApiError::internal("identity serialization"))?;
                    }
                    *call_ctx = Some(call);
                } else if let Some(error) = response.error.clone() {
                    // One authentication attempt per connection: a failed proof ends it, so a hostile peer cannot
                    // grind guesses against a fixed pair of nonces.
                    let _ = notifier_send(notifier, Message::Response(response));
                    return Err(ApiError::from_error_object(&error));
                }
                response
            }
            _ => {
                let Some(mut call) = call_ctx.clone() else {
                    let error = ApiError::not_authenticated();
                    let _ = notifier_send(
                        notifier,
                        Message::Response(Response::failure(Some(request.id), &error)),
                    );
                    // A peer that sends work before authenticating is not a client we want to keep.
                    return Err(error);
                };
                if revoked(call_ctx) {
                    let error =
                        ApiError::new(codes::NOT_AUTHENTICATED, "the credential was revoked");
                    let _ = notifier_send(
                        notifier,
                        Message::Response(Response::failure(Some(request.id), &error)),
                    );
                    return Err(error);
                }
                if shutdown.notice().is_some() {
                    let error = ApiError::new(codes::UNAVAILABLE, "daemon is stopping")
                        .with_detail("reason", "stopping");
                    let _ = notifier_send(
                        notifier,
                        Message::Response(Response::failure(Some(request.id), &error)),
                    );
                    continue;
                }
                let permit = match in_flight.clone() {
                    Some(semaphore) => tokio::select! {
                        biased;
                        _ = stopping.changed() => {
                            let error = ApiError::new(codes::UNAVAILABLE,"daemon is stopping").with_detail("reason","stopping");
                            let _ = notifier_send(notifier,Message::Response(Response::failure(Some(request.id),&error)));
                            continue;
                        },
                        permit = semaphore.acquire_owned() => permit.ok(),
                    },
                    None => None,
                };
                let router = Arc::clone(&ctx.router);
                let out = notifier.clone();
                call.reply = Default::default();
                tasks.spawn(async move {
                    let response = router.dispatch(&call, request).await;
                    let _ = out.tx.send(Message::Response(response)).await;
                    call.reply.release();
                    drop(permit);
                });
                continue;
            }
        };
        if notifier_send(notifier, Message::Response(response)).is_err() {
            return Ok(());
        }
    }
}

fn notifier_send(notifier: &Notifier, message: Message) -> Result<(), NotifyError> {
    notifier.tx.try_send(message).map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => NotifyError::Full,
        mpsc::error::TrySendError::Closed(_) => NotifyError::Closed,
    })
}

fn handle_handshake(
    handshake: &mut ServerHandshake<'_>,
    request: &Request,
    hello: bool,
) -> Response {
    let id = request.id.clone();
    let params = request.params_or_empty();
    let result: Result<Value, ApiError> = if hello {
        serde_json::from_value(params)
            .map_err(|e| invalid_params("daemon.hello", &e))
            .and_then(|p| handshake.on_hello(p))
            .and_then(|r| serde_json::to_value(r).map_err(|_| ApiError::internal("serialise")))
    } else {
        serde_json::from_value(params)
            .map_err(|e| invalid_params("daemon.authenticate", &e))
            .and_then(|p| handshake.on_authenticate(p))
            .and_then(|r| serde_json::to_value(r).map_err(|_| ApiError::internal("serialise")))
    };
    match result {
        Ok(value) => Response::success(id, value),
        Err(error) => Response::failure(Some(id), &error),
    }
}

fn connection_id_from(response: &Response) -> ConnectionId {
    response
        .result
        .as_ref()
        .and_then(|r| r.get("connection_id"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_else(|| zeus_types::IdGenerator::new().connection_id())
}

/// Accept connections through an owner-only listener. Drop this future to stop accepting.
pub async fn serve(
    ctx: Arc<ServerContext>,
    endpoint: &crate::endpoint::Endpoint,
) -> Result<(), ApiError> {
    let listener = crate::transport::bind(endpoint)?;
    serve_listener(ctx, listener).await
}

/// Serve an already-bound listener, so the daemon publishes discovery only after binding succeeds.
pub async fn serve_listener(
    ctx: Arc<ServerContext>,
    listener: crate::transport::IpcListener,
) -> Result<(), ApiError> {
    let shutdown = ctx.router.shutdown.clone().unwrap_or_default();
    let connections = Arc::new(Semaphore::new(64));
    let mut tasks = tokio::task::JoinSet::new();
    let notice = loop {
        while tasks.try_join_next().is_some() {}
        let permit = tokio::select! {
            biased;
            notice = shutdown.stopping() => break notice,
            permit = connections.clone().acquire_owned() => permit.map_err(|_| ApiError::internal("connection limiter closed"))?,
        };
        let accepted = tokio::select! {
            biased;
            notice = shutdown.stopping() => break notice,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok(stream) => {
                let peer = crate::transport::peer_process(&stream);
                let ctx = ctx.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve_connection_with_peer(stream, ctx, peer).await {
                        tracing::debug!(code = %error.envelope().code, "IPC connection ended");
                    }
                });
            }
            Err(error) => {
                drop(permit);
                tracing::warn!(code = %error.envelope().code, "IPC accept rejected");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    };
    drop(listener);
    // Reads and handlers end at the deadline. Permit only the existing bounded writer drain
    // afterward, including grace_ms=0, so the accepted response precedes the final notification.
    let deadline = notice.deadline + std::time::Duration::from_millis(250);
    if tokio::time::timeout_at(deadline, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    Ok(())
}
