# ADR 0002: Local JSON-RPC daemon API as the single contract

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |
| Specs affected | 05 (planned), 02 section 3.4, 00 sections 4.2, 9 and 10 |

## Context

Zeus has several front ends that must behave identically: the CLI and TUI, the MCP server, the native-messaging host for the browser extension, a future GUI and a phase-2 A2A endpoint. If each talked to the engine through its own interface, authorisation, idempotency, event delivery and error mapping would be implemented several times, and the approvals rule (only a human may approve) could be bypassed through whichever interface was checked least carefully.

The daemon `zeusd` owns all durable state (journal, grants, approvals, audit log). Everything else is a client.

## Decision

1. `zeusd` exposes **one** API: JSON-RPC 2.0 over a **local** socket. On Windows a named pipe, on Unix a domain socket. Nothing else listens (ADR 0010).
2. **Framing is newline-delimited JSON**: one JSON-RPC message per line, UTF-8, no embedded newlines (compact serialisation). Why not `Content-Length` headers (LSP style): the stream is machine-to-machine, so a human-readable framing that `nc`, `socat` and shell scripts can speak is a real debugging asset; a length prefix cannot be resynchronised after a corrupt byte whereas a newline can; and every JSON serializer already escapes newlines inside strings. Why not the native-messaging 4-byte prefix: that is dictated by browsers, not chosen. Lines are bounded (1 MiB for requests, 16 MiB for responses, 00 section 10); a longer line closes the connection.
3. **Endpoint naming.** Windows: `\\.\pipe\zeusd-<h>` where `h` is the first 16 hex digits of BLAKE3 over a fixed label and the user's identity (SID where it can be read, otherwise `DOMAIN\user`). The pipe is created with `FILE_FLAG_FIRST_PIPE_INSTANCE` (a squatted name makes the daemon refuse to start instead of serving a shadow), `PIPE_REJECT_REMOTE_CLIENTS`, and a DACL granting only the current user. Unix: `$XDG_RUNTIME_DIR/zeus/zeusd.sock` inside a `0700` directory, socket mode `0600`; fallbacks are used when the path would exceed `sun_path`. Peer credentials are checked after accept (`SO_PEERCRED` on Linux, `LOCAL_PEERCRED` on macOS, pipe client process token on Windows): the peer must run as the daemon's user.
4. **Mutual challenge-response authentication** instead of sending a token to whatever answers the pipe. `daemon.hello` carries a client nonce, the protocol versions and capabilities; the answer carries the chosen version, a server nonce and `server_proof = BLAKE3-keyed(ipc_secret, client_nonce || "zeus-server-v1")`, where `ipc_secret` lives in a `0600` file only the user can read. The client verifies the proof, then calls `daemon.authenticate` with `{credential_id, proof = BLAKE3-keyed(token, server_nonce || client_nonce || "zeus-client-v1")}`. The token itself never crosses the wire, and a process that squatted the endpoint cannot produce `server_proof`.
5. **Client kinds and permissions** (`agent`, `human`, `admin`; fine-grained `Permission` strings) are bound to the credential, not claimed by the client. `approvals.decide` is human-only and additionally requires channel evidence (ADR 0007).
6. **Versioning** follows 00 section 4: integer protocol version plus capability strings, negotiated once in `daemon.hello`.
7. **Events** are JSON-RPC notifications with sequence numbers and resume-from-seq (01 section 9). A slow subscriber is told it lagged and resumes; the daemon never blocks the engine on a client.
8. **The native host** uses the same API: after authentication as role `native_host` it relays native-messaging frames as `nm.frame` notifications. Lines on host connections may be up to 5 MiB (a 4 MiB extension frame plus envelope). This replaces the "length-prefixed framing on the host-to-daemon leg" sentence in spec 02 section 3.4; the extension never sees the difference. Spec 02 needs a one-line follow-up edit.
9. The Rust contract lives in the `zeus-api` crate (types, client and server frameworks, transport). The JSON Schema `docs/spec/schema/daemon-api.v1.schema.json` is generated from those types and checked in CI once spec 05 exists.

## Consequences

- One place implements authentication, authorisation, idempotency, pagination and error mapping. A new front end is a thin client.
- Third parties can drive Zeus from any language with a socket and a JSON parser.
- The API is a stability commitment. It is versioned from day one and frozen as v1 after the workflow phase (roadmap).
- The daemon has no network listener, which removes a class of DNS-rebinding and port-scanning exposure from the core.
- Cost: we build and test a small RPC framework (envelope, framing, auth, subscriptions) instead of adopting gRPC or HTTP.

## Alternatives considered

- **gRPC or HTTP on localhost.** Larger dependency surface, needs port management and token plus Origin checks to resist DNS rebinding and malicious web pages. Rejected for the core; kept for bridge processes (ADR 0010).
- **Length-prefixed frames everywhere.** Consistent with native messaging, but harder to debug and not resynchronisable.
- **Embedding the engine in each client.** Loses a single owner for journal, approvals and grants; impossible for the browser handoff.
- **D-Bus / XPC / COM.** Platform-specific and unavailable on one of the three target platforms.
