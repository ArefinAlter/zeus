//! The daemon side of the native-messaging protocol (spec 02).
//!
//! The browser extension talks to `zeus-native-host`, which relays each native-messaging frame to the daemon as an
//! `nm.frame` notification on an authenticated `native_host` connection (ADR 0002). The daemon is the protocol
//! peer: it validates every frame against `native-messaging.v1.schema.json`, negotiates versions in `hello`, keys
//! connections by `profile_id`, and applies the rules of spec 02 sections 5 to 13.
//!
//! The implementation depends on `zeus-session` (message types, codec, consent logic) and is a later task; this
//! module fixes the seam so the rest of the daemon can be built and tested against it.

use async_trait::async_trait;
use zeus_api::{ApiError, ApiResult, CallContext};

/// Handles frames arriving from one native-host connection.
#[async_trait]
pub trait NativeMessagingEndpoint: Send + Sync + 'static {
    /// A frame from the extension (`params.frame` of an `nm.frame` notification), already size-checked by the
    /// host. Replies are pushed back through `ctx.notifier` as `nm.frame` notifications.
    async fn on_frame(&self, ctx: &CallContext, frame: serde_json::Value) -> ApiResult<()> {
        let _ = (ctx, frame);
        Err(ApiError::not_implemented(
            "native-messaging endpoint (needs zeus-session)",
        ))
    }

    /// The host connection ended: drop its leases, abort its fetch streams, fail its pending requests with
    /// `nm.timeout` semantics (spec 02 section 3.5).
    async fn on_disconnect(&self, ctx: &CallContext) {
        let _ = ctx;
    }
}

/// Endpoint with no behaviour.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotImplementedNm;

impl NativeMessagingEndpoint for NotImplementedNm {}
