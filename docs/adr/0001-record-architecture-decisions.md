# ADR 0001: Record architecture decisions

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |

## Context

Zeus is built by several people and several coding agents across three repositories (`zeus`, `zeus-core`, `zeus-extension`). Decisions made in conversation are lost, and the next contributor re-litigates them or silently reverses them.

## Decision

Significant, hard-to-reverse decisions are recorded as Architecture Decision Records in `docs/adr/`, one Markdown file per decision, numbered `NNNN-short-title.md`, in the style of Michael Nygard's ADRs.

Each record has: Status (`Proposed`, `Accepted`, `Superseded by NNNN`), Date, Context, Decision, Consequences, and Alternatives considered. Records are immutable once accepted: to change a decision, add a new record that supersedes the old one and change only the old record's status line.

An ADR is required when a change affects a contract in `docs/spec`, a trust boundary, a licence, a persistent format, or the crate dependency graph. Normative behaviour belongs in the specs; ADRs explain why the specs look the way they do.

## Consequences

- Newcomers can read the ADRs in order and understand the shape of the system.
- A reviewer can ask "which ADR does this contradict?" instead of arguing from taste.
- The cost is a short document per decision.

## Index

| ADR | Title |
|---|---|
| 0001 | Record architecture decisions |
| 0002 | Local JSON-RPC daemon API as the single contract |
| 0003 | MCP is the primary agent interface |
| 0004 | Extension-mediated sessions; never read cookie databases |
| 0005 | redb as the transfer journal |
| 0006 | Apache-2.0 open core |
| 0007 | Human-only approvals |
| 0008 | Bring-your-own-key with a provider ladder |
| 0009 | Tauri for the later GUI |
| 0010 | One local listener; HTTP surfaces are bridge processes |
