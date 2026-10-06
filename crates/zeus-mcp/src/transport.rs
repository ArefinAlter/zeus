//! Binding to the `rmcp` transports.
//!
//! * stdio (`zeus mcp`): the MCP client starts the process and speaks over stdin/stdout.
//! * Streamable HTTP (`zeus mcp --http`, feature `http`): optional, **off by default**, a separate bridge process
//!   that is an ordinary daemon client (ADR 0010). It MUST bind `127.0.0.1` only, require a bearer token, validate
//!   the `Origin` and `Host` headers against an allow-list (DNS rebinding), and expose nothing else.
//!
//! The binding implements `rmcp::ServerHandler` over [`crate::ZeusTools`]: `tools/list` from
//! [`crate::catalogue::catalogue`] (`ToolDef::to_rmcp`), `tools/call` through [`crate::ZeusTools::call`] mapping
//! [`crate::ToolOutcome`] to a `CallToolResult` (`structuredContent`, `isError`, one-line text), resources and the
//! prompt from [`crate::resources`], `initialize` instructions from [`crate::catalogue::SERVER_INSTRUCTIONS`].
//! MCP elicitation is **not** used for consent (it is not a secure channel); sampling is not used by the MVP.
//!
//! Not implemented in the skeleton: rmcp 3.5 changed its server API (`CallToolResponse`, `ServerConfig`) and the
//! binding is written against the real crate sources in the implementation task.

use std::sync::Arc;

use zeus_api::{ApiError, ApiResult, Client};

/// Serve MCP over stdio until the client disconnects.
pub async fn serve_stdio<C: Client>(_client: Arc<C>) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "zeus_mcp::transport::serve_stdio (rmcp ServerHandler binding)",
    ))
}

/// Serve MCP over Streamable HTTP on `127.0.0.1:<port>` with a bearer token. Requires feature `http`.
#[cfg(feature = "http")]
pub async fn serve_http<C: Client>(
    _client: Arc<C>,
    _port: u16,
    _bearer_token_hash: [u8; 32],
) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "zeus_mcp::transport::serve_http (Origin and Host validation, bearer token)",
    ))
}
