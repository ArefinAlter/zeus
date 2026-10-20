//! The client side: the [`Client`] trait the CLI, MCP server and native host program against, and
//! [`IpcClient`], its implementation over a local socket (or any duplex stream).
//!
//! `IpcClient` performs the mutual handshake of [`crate::auth`] before it returns, so a value of this type is
//! always an authenticated connection to a daemon that proved it holds the user's IPC secret. Responses are
//! matched by id, so many requests can be in flight; notifications are fanned out on a broadcast channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};

use crate::auth::{ClientCredential, ClientHandshake, IpcSecret};
use crate::endpoint::Endpoint;
use crate::error::{ApiError, ApiResult, codes};
use crate::framing::{LineLimits, read_line, write_message};
use crate::jsonrpc::{Message, Notification, Request, RequestId};
use crate::methods::daemon::{AttachInfo, AuthenticateResult, ClientInfo, HelloResult};
use crate::methods::{DaemonAuthenticate, DaemonHello, RpcMethod};

/// What every front end needs from a daemon connection.
#[async_trait]
pub trait Client: Send + Sync {
    /// Call a method with raw JSON parameters.
    async fn call_raw(&self, method: &str, params: Value) -> ApiResult<Value>;

    /// Notifications pushed by the daemon (`events.event`, `events.lagged`, `daemon.stopping`, `nm.frame`).
    fn notifications(&self) -> broadcast::Receiver<Notification>;

    /// What the daemon granted this connection, from `daemon.authenticate`.
    fn session(&self) -> &AuthenticateResult;
}

/// Typed call: serialise the parameters of `M`, send, deserialise the result.
pub async fn call<M, C>(client: &C, params: M::Params) -> ApiResult<M::Result>
where
    M: RpcMethod,
    C: Client + ?Sized,
{
    let value = serde_json::to_value(params)
        .map_err(|_| ApiError::internal("parameters could not be serialised"))?;
    let result = client.call_raw(M::NAME, value).await?;
    serde_json::from_value(result)
        .map_err(|_| ApiError::internal("the daemon sent a result of an unexpected shape"))
}

type Pending = Arc<Mutex<HashMap<RequestId, oneshot::Sender<ApiResult<Value>>>>>;

/// Connection settings.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Who we are (display and logs only).
    pub info: ClientInfo,
    /// Per-call timeout; the daemon's own long waits (`jobs.wait`) are at most 60 s, so the default leaves margin.
    pub call_timeout: Duration,
    /// Set only by `zeus-native-host`.
    pub attach: Option<AttachInfo>,
    /// Capabilities offered in `daemon.hello`; every capability this build understands by default.
    pub capabilities: Vec<String>,
}

impl ClientConfig {
    /// Defaults for `info`: 90 s per call, no attach, every capability.
    pub fn new(info: ClientInfo) -> Self {
        Self {
            info,
            call_timeout: Duration::from_secs(90),
            attach: None,
            capabilities: crate::capabilities::ALL
                .iter()
                .map(|c| (*c).to_string())
                .collect(),
        }
    }
}

/// An authenticated connection to the daemon.
pub struct IpcClient {
    outgoing: mpsc::Sender<Message>,
    pending: Pending,
    next_id: AtomicI64,
    notifications: broadcast::Sender<Notification>,
    session: AuthenticateResult,
    hello: HelloResult,
    timeout: Duration,
    closed: watch::Receiver<bool>,
    reader_task: tokio::task::AbortHandle,
    writer_task: tokio::task::AbortHandle,
}

impl IpcClient {
    /// Connect to the endpoint of the user's daemon and authenticate.
    pub async fn connect(
        endpoint: &Endpoint,
        credential: ClientCredential,
        ipc_secret: &IpcSecret,
        config: ClientConfig,
    ) -> ApiResult<Self> {
        let stream = crate::transport::connect(endpoint).await?;
        Self::from_stream(stream, credential, ipc_secret, config).await
    }

    /// Authenticate over an already connected stream.
    pub async fn from_stream<S>(
        stream: S,
        credential: ClientCredential,
        ipc_secret: &IpcSecret,
        config: ClientConfig,
    ) -> ApiResult<Self>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let (outgoing, mut queue) = mpsc::channel::<Message>(256);
        let pending: Pending = Arc::default();
        let (notifications, _) = broadcast::channel(1024);

        let (closed_tx, closed) = watch::channel(false);
        let writer_closed = closed_tx.clone();
        let writer = tokio::spawn(async move {
            while let Some(message) = queue.recv().await {
                if write_message(&mut write_half, &message).await.is_err() {
                    break;
                }
            }
            writer_closed.send_replace(true);
        });
        let writer_task = writer.abort_handle();

        let reader_pending = Arc::clone(&pending);
        let reader_notifications = notifications.clone();
        let reader_writer = writer_task.clone();
        let mut reader_closed = closed.clone();
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            loop {
                let read = tokio::select! {
                    _ = reader_closed.wait_for(|value| *value) => break,
                    read = read_line(&mut reader, LineLimits::STANDARD.max_response_bytes) => read,
                };
                let line = match read {
                    Ok(Some(line)) => line,
                    Ok(None) | Err(_) => break,
                };
                match Message::parse(&line) {
                    Ok(Message::Response(response)) => {
                        let Some(id) = response.id.clone() else {
                            continue;
                        };
                        if let Some(waiter) = reader_pending.lock().await.remove(&id) {
                            let _ = waiter.send(response.into_result());
                        }
                    }
                    Ok(Message::Notification(note)) => {
                        let _ = reader_notifications.send(note);
                    }
                    Ok(Message::Request(_)) | Err(_) => {}
                }
            }
            closed_tx.send_replace(true);
            reader_writer.abort();
            // Connection gone: fail everything still waiting.
            let mut waiting = reader_pending.lock().await;
            for (_, waiter) in waiting.drain() {
                let _ = waiter.send(Err(ApiError::new(
                    codes::UNAVAILABLE,
                    "connection to the daemon closed",
                )));
            }
        });

        let mut client = Self {
            outgoing,
            pending,
            next_id: AtomicI64::new(1),
            notifications,
            session: placeholder_session(),
            hello: placeholder_hello(),
            timeout: config.call_timeout,
            closed,
            reader_task: reader_task.abort_handle(),
            writer_task,
        };

        let handshake =
            ClientHandshake::new(credential, config.info).with_capabilities(config.capabilities);
        let hello: HelloResult =
            call_typed::<DaemonHello>(&client, handshake.hello_params()).await?;
        let auth = handshake.authenticate_params(ipc_secret, &hello, config.attach)?;
        let session: AuthenticateResult = call_typed::<DaemonAuthenticate>(&client, auth).await?;
        client.hello = hello;
        client.session = session;
        Ok(client)
    }

    /// Wait until either side of this connection closes. Safe to call after closure.
    pub async fn closed(&self) {
        let mut closed = self.closed.clone();
        let _ = closed.wait_for(|value| *value).await;
    }

    /// The negotiated protocol version and capabilities.
    pub fn hello(&self) -> &HelloResult {
        &self.hello
    }

    /// Send a notification (no response), for example `nm.frame` from the native host.
    pub async fn notify(&self, method: &str, params: Value) -> ApiResult<()> {
        self.outgoing
            .send(Message::Notification(Notification::new(
                method,
                Some(params),
            )))
            .await
            .map_err(|_| ApiError::new(codes::UNAVAILABLE, "connection to the daemon closed"))
    }
}

impl Drop for IpcClient {
    fn drop(&mut self) {
        self.reader_task.abort();
        self.writer_task.abort();
    }
}

async fn call_typed<M: RpcMethod>(client: &IpcClient, params: M::Params) -> ApiResult<M::Result> {
    call::<M, IpcClient>(client, params).await
}

#[async_trait]
impl Client for IpcClient {
    async fn call_raw(&self, method: &str, params: Value) -> ApiResult<Value> {
        if *self.closed.borrow() {
            return Err(ApiError::new(
                codes::UNAVAILABLE,
                "connection to the daemon closed",
            ));
        }
        let id = RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (waiter, response) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), waiter);
        let request = Request::new(id.clone(), method, Some(params));
        if self.outgoing.send(Message::Request(request)).await.is_err() {
            self.pending.lock().await.remove(&id);
            return Err(ApiError::new(
                codes::UNAVAILABLE,
                "connection to the daemon closed",
            ));
        }
        match tokio::time::timeout(self.timeout, response).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ApiError::new(
                codes::UNAVAILABLE,
                "connection to the daemon closed",
            )),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(ApiError::new(
                    codes::TIMEOUT,
                    format!("no answer to {method} in time"),
                ))
            }
        }
    }

    fn notifications(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }

    fn session(&self) -> &AuthenticateResult {
        &self.session
    }
}

fn placeholder_session() -> AuthenticateResult {
    AuthenticateResult {
        credential_kind: crate::kind::ClientKind::Agent,
        downgraded: false,
        client_id: String::new(),
        kind: crate::kind::ClientKind::Agent,
        role: crate::kind::ClientRole::Test,
        permissions: Vec::new(),
        limits: crate::methods::daemon::ConnectionLimits::STANDARD,
        connection_id: zeus_types::IdGenerator::new().connection_id(),
    }
}

fn placeholder_hello() -> HelloResult {
    HelloResult {
        protocol_version: 0,
        capabilities: Default::default(),
        limits: crate::methods::daemon::ConnectionLimits::STANDARD,
        daemon: crate::methods::daemon::DaemonInfo {
            version: String::new(),
            instance_id: String::new(),
        },
        server_nonce: String::new(),
        server_proof: String::new(),
    }
}
