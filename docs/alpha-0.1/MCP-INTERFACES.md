# MCP alpha interfaces

The existing stdio server remains available. The CLI's default build now includes the optional `mcp-http` feature; library consumers can leave Zeus MCP's `http` feature disabled.

## Authenticated local HTTP

`zeus mcp --http --init-token` creates a dedicated owner-only `mcp-http-token.json` in this installation's private state directory and starts the HTTP endpoint. An existing token is never replaced and no token is printed. Later runs use `zeus mcp --http`; `--token-file <file>` selects an existing dedicated JSON token file. `--port` defaults to 4747. The endpoint is `http://127.0.0.1:<port>/mcp`.

Clients send `Authorization: Bearer <token>` from that file; IPC/client credential formats are rejected. Tokens are not accepted in URLs. Requests require the exact loopback Host; a present Origin must match that same loopback origin. The profile does not permit arbitrary browser origins, public interfaces or remote deployment. It runs as a separate process, preserving the daemon's single local IPC listener.

The bridge uses the pinned Rust MCP SDK's Streamable HTTP implementation in stateless mode, with JSON responses, a 64 KiB streamed request-body limit, 32 active requests and finite request/body deadlines. SDK protocol/version/message validation remains in place. Every operation uses the ordinary Agent/MCP daemon credential; approval decisions, source-session material and human-only account operations are not exposed. A disconnected mutation may already have committed: inspect its receipt and reuse its original idempotency key.

## Read-only App monitor

`zeus_status` and `zeus_wait` reference `ui://zeus/transfer-monitor/v1`. The host can render its static HTML and deliver the ordinary redacted tool result. Refresh requests only `zeus_status`, bounded to 50 jobs. Names render as text, external connection/resource domains are empty, and no camera, clipboard, file-open, approval or mutation permission is requested. Other Zeus tools declare model-only visibility. A compliant host enforces that boundary when mediating App calls.

The resource has an inline-script integrity policy and no external runtime dependencies. It negotiates the MCP Apps 2026-01-26 lifecycle and supports host theming, resizing and teardown. Clients without Apps keep meaningful text/structured output. A local fixture is separate from installed Claude/Codex/other-host acceptance.

## Artifact lifecycle and URL intake

`zeus_artifacts` adds closed `lifecycle_preview` and `lifecycle_apply` operations over existing daemon methods. Apply accepts the exact typed preview receipt plus the outer intent key. Deletion tombstones metadata, materialization creates a verified copy, and retention weakening still requests human review. Edited/stale previews and forged approval fields are refused; the tool cannot decide an approval or release quarantine.

`zeus get --input-file -` reads bounded UTF-8 URL data from stdin. Exact signed URLs survive, exact duplicates are removed in order, and the entire 1–50 URL batch is validated before enqueue. Approval answers continue to use the controlling terminal, separately from piped data.

## Durable inbox and terminal recovery

`zeus_inbox` groups ten owner-scoped intake methods. Read `zeus://inbox/schema` for typed parameters; owner inspection selectors and inner idempotency keys are refused. Review saves up to50 exact requests; submit names that saved preview, and retry reconciles its existing batch. Session approvals remain separate. Oversized tool replies preserve a typed package/submission resource handle when available, rather than silently dropping identifiers.

The trusted terminal exposes `zeus inbox create`, `add`, `list`, `inspect`, `remove`, `archive`, `preview`, `submit`, `submission` and `retry`. `add -i <file>` or `-i -` validates1–200 exact URL requests before retaining them; use successive revision-bound additions for larger packages. Every mutation requires an explicit idempotency key. `list`, `inspect` and `submission` alone accept `--owner` for read-only human inspection; `inspect --history` explicitly includes retained redacted review/dispatch history. These commands never decide approvals.

## Validation state

Catalogue generation/budget checks pass with14 tools and259 valid spec examples. The monitor passed12 checks in a fresh isolated Edge profile: CSP-pinned execution, initialization, exact bounded refresh, text-only remote names, sibling-message rejection, theme/narrow layout, empty state, ping, teardown and no external network/page errors. Screenshots/results are retained at D:/zeus-alpha/evidence/mcp-app. All 51 native MCP tests now pass, covering HTTP admission/deadlines, stdio discovery, App resources, inbox and artifact lifecycle, and rejection without request-body echo. Strict Clippy also passes. Actual external MCP host acceptance remains pending; these fixtures do not by themselves make the alpha release-ready.

Primary protocols: [current MCP Streamable HTTP](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http), [MCP Apps](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx).

Repeat the browser fixture with `ZEUS_MCP_APP_EVIDENCE=<scratch> node crates/zeus-mcp/tests/browser/monitor.mjs`. It resolves `@playwright/test` from the checkout, or from the explicit package.json path in `ZEUS_PLAYWRIGHT_PACKAGE`; `ZEUS_BROWSER_CHANNEL=msedge` selects an installed Edge for Windows. It creates a fresh headless profile and binds only loopback. Store browser temporary files on a drive with sufficient space.
