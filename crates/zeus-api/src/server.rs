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
use crate::framing::{FrameError, LineLimits, read_line, write_message};
use crate::jsonrpc::{Message, Notification, Request, Response};
use crate::kind::ClientIdentity;
use crate::methods::RpcMethod;
use crate::methods::daemon::{ConnectionLimits, DaemonInfo};
use crate::policy;

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

/// Everything a handler knows about the call.
#[derive(Debug, Clone)]
pub struct CallContext {
    /// The authenticated caller.
    pub identity: ClientIdentity,
    /// This connection, for logs.
    pub connection_id: ConnectionId,
    /// Where to push notifications for this connection (event subscriptions).
    pub notifier: Notifier,
    /// Limits in force.
    pub limits: ConnectionLimits,
}

type BoxedHandler =
    Box<dyn Fn(CallContext, Value) -> BoxFuture<'static, Result<Value, ApiError>> + Send + Sync>;

/// Dispatches authenticated requests to typed handlers.
#[derive(Default)]
pub struct Router {
    routes: HashMap<&'static str, BoxedHandler>,
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
        let spec = policy::lookup(&request.method).ok_or_else(|| {
            ApiError::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown method `{}`", short(&request.method)),
            )
        })?;
        let params = request.params_or_empty();
        if !params.is_object() {
            return Err(ApiError::invalid_params("params must be an object"));
        }
        policy::authorize(Some(&ctx.identity), spec, &params)?;
        let handler = self.routes.get(spec.name).ok_or_else(|| {
            ApiError::not_implemented("this method is declared but not available in this build")
        })?;
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
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let (notifier, mut outgoing) =
        Notifier::channel(ConnectionLimits::STANDARD.event_buffer as usize);

    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if write_message(&mut write_half, &message).await.is_err() {
                break;
            }
        }
    });

    let outcome = connection_loop(&mut reader, &ctx, &notifier).await;
    drop(notifier);
    let _ = writer.await;
    outcome
}

async fn connection_loop<R>(
    reader: &mut BufReader<R>,
    ctx: &Arc<ServerContext>,
    notifier: &Notifier,
) -> Result<(), ApiError>
where
    R: AsyncRead + Unpin,
{
    let mut handshake =
        ServerHandshake::new(&ctx.secret, ctx.credentials.as_ref(), ctx.daemon.clone());
    let mut limits = LineLimits::STANDARD;
    let mut call_ctx: Option<CallContext> = None;
    let mut in_flight: Option<Arc<Semaphore>> = None;

    loop {
        let line = match read_line(reader, limits.max_request_bytes).await {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error @ FrameError::TooLong { .. }) => {
                let api: ApiError = error.into();
                let _ = notifier_send(notifier, Message::Response(Response::failure(None, &api)));
                return Err(api);
            }
            Err(error) => return Err(error.into()),
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
            // Clients send notifications only for `nm.frame`; the daemon-side relay is a later task.
            Message::Notification(_) | Message::Response(_) => continue,
        };

        let response = match request.method.as_str() {
            crate::methods::names::DAEMON_HELLO => handle_handshake(&mut handshake, &request, true),
            crate::methods::names::DAEMON_AUTHENTICATE => {
                let response = handle_handshake(&mut handshake, &request, false);
                if let Some(identity) = handshake.identity() {
                    let connection_limits = ConnectionLimits::for_role(identity.role);
                    limits = LineLimits {
                        max_request_bytes: connection_limits.max_request_bytes as usize,
                        max_response_bytes: connection_limits.max_response_bytes as usize,
                    };
                    in_flight = Some(Arc::new(Semaphore::new(
                        connection_limits.max_in_flight_requests as usize,
                    )));
                    call_ctx = Some(CallContext {
                        identity: identity.clone(),
                        connection_id: connection_id_from(&response),
                        notifier: notifier.clone(),
                        limits: connection_limits,
                    });
                } else if let Some(error) = response.error.clone() {
                    // One authentication attempt per connection: a failed proof ends it, so a hostile peer cannot
                    // grind guesses against a fixed pair of nonces.
                    let _ = notifier_send(notifier, Message::Response(response));
                    return Err(ApiError::from_error_object(&error));
                }
                response
            }
            _ => {
                let Some(call) = call_ctx.clone() else {
                    let error = ApiError::not_authenticated();
                    let _ = notifier_send(
                        notifier,
                        Message::Response(Response::failure(Some(request.id), &error)),
                    );
                    // A peer that sends work before authenticating is not a client we want to keep.
                    return Err(error);
                };
                let permit = match in_flight.clone() {
                    Some(semaphore) => semaphore.acquire_owned().await.ok(),
                    None => None,
                };
                let router = Arc::clone(&ctx.router);
                let out = notifier.clone();
                tokio::spawn(async move {
                    let response = router.dispatch(&call, request).await;
                    let _ = out.tx.send(Message::Response(response)).await;
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

/// Accept loop over a bound listener. Not implemented in the skeleton: it needs the hardened bind of
/// [`crate::transport::bind`] first.
pub async fn serve(
    _ctx: Arc<ServerContext>,
    _endpoint: &crate::endpoint::Endpoint,
) -> Result<(), ApiError> {
    Err(ApiError::not_implemented(
        "zeus_api::server::serve (accept loop over the hardened listener)",
    ))
}
