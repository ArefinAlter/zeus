# ADR 0010: One local listener; HTTP surfaces are bridge processes

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |
| Specs affected | 05, 06, 10, 12 (all planned) |

## Context

Research suggested "MCP over HTTP is opt-in, bound to localhost, with token auth" as a daemon feature. Two optional HTTP surfaces exist: MCP Streamable HTTP (for clients that cannot spawn a stdio process) and the A2A endpoint (phase 2). Any HTTP listener inside the daemon exposes the process that holds the journal, grants and approvals to DNS rebinding, cross-origin requests from web pages, and port-scanning by other local users.

## Decision

1. The daemon has exactly **one listener**: the local IPC endpoint of ADR 0002. It binds no TCP port, ever.
2. Optional HTTP surfaces are **separate bridge processes** that are ordinary daemon clients with an `agent` credential: `zeus mcp --http` (feature `http` of `zeus-mcp`) and `zeus a2a serve` (phase 2, `zeus-a2a`). Both are **off by default**, bind `127.0.0.1` only, require a bearer token, validate the `Origin` and `Host` headers against an allow-list (DNS rebinding), reject everything else, and expose no approval or credential operation. Remote exposure of the A2A endpoint requires explicit opt-in and mTLS or OAuth (spec 10).
3. Every request arriving over a bridge is classified as an **agent** client for approvals, whatever the caller claims. A compromised or misconfigured bridge therefore cannot approve anything and cannot exceed agent permissions.

## Consequences

- The daemon's attack surface is local IPC only; a vulnerability in an HTTP stack or MCP SDK cannot reach the journal directly.
- A bridge can be restarted, upgraded or sandboxed independently.
- Cost: one more process for people who use HTTP transports, and a small amount of duplicated plumbing.

## Alternatives considered

- Listener inside the daemon behind a flag: simplest, but the daemon's blast radius grows with each protocol.
- Unix-socket HTTP only: not available to Windows MCP clients that expect URLs.
