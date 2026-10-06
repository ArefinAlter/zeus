# ADR 0003: MCP is the primary agent interface

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |
| Specs affected | 06 (planned), 01 section 12 |

## Context

The product exists so that coding and research agents can fetch files reliably. Agents reach tools through MCP today (Claude Code, Cursor, Codex and others); A2A is a peer-to-peer protocol for later. Tool schemas cost context tokens on every request, and tool outputs are untrusted-content channels.

## Decision

1. MCP is the **primary agent interface**. It is implemented by the `zeus-mcp` library (rmcp 3.x), started by `zeus mcp` over stdio. It is a *client of the daemon API* with an `agent` credential (ADR 0002) and holds no state of its own.
2. The tool set is **small**. MVP: `zeus_probe`, `zeus_enqueue`, `zeus_status`, `zeus_wait`, `zeus_control`, `zeus_verify`, `zeus_session_list`, `zeus_session_request`, `zeus_files_list` (nine). Later: `zeus_files_organize`, `zeus_convert`, `zeus_workflow_plan`, `zeus_workflow_run`, `zeus_manifest_export`. Tool names use only `[a-z0-9_]`, at most 64 characters, `zeus_` prefix.
3. **Jobs, not calls.** `zeus_enqueue` returns job ids immediately. `zeus_wait` takes a timeout of at most 60 seconds and returns progress; no call blocks for minutes. Every mutating tool takes an `idempotency_key`; every move, delete or convert defaults to `dry_run: true`.
4. **MCP is not a trust surface.** No tool decides an approval, creates or extends a grant, returns session material or changes policy. `zeus_session_request` creates a pending approval and answers `pending_user_approval`. MCP elicitation is not a secure approval channel because the host agent mediates it; Zeus does not use it for consent.
5. Everything derived from a remote server or file (names, headers, labels, content) is returned in clearly labelled fields as untrusted data (01 section 12).
6. Results use `structuredContent` with an `outputSchema`, annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`), and short descriptions written for model consumption. Resources: `zeus://jobs/{id}`, `zeus://runs/{id}/manifest`, `zeus://audit/today`.
7. The Streamable HTTP transport is optional, off by default, and runs as a bridge process (ADR 0010).

## Consequences

- Agents get a stable, small, well-annotated surface; most of the product's safety properties are enforced below it, in the daemon, so a buggy or hostile MCP client cannot exceed an agent's permissions.
- The MCP layer is easy to test against a fake daemon client.
- Any future agent protocol (A2A) reuses the same daemon calls.

## Alternatives considered

- Expose every daemon method as a tool: rejected, context cost and attack surface.
- Put MCP inside the daemon: rejected, widens the daemon and ties its release cycle to the MCP SDK.
