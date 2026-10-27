# ADR 0012: App-owned stores beside the engine journal

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-18 |
| Decided by | Pending maintainer review. Records the boundary as built in the 2026-10-16 runtime and its 2026-10-18 follow-ups. |
| Specs affected | 03 (state layout), 05 (idempotency, clients), 14 (cloud state) |
| Amends | ADR 0005 decision 4 ("No crate in `zeus` depends on redb") |

## Context

ADR 0005 made redb the engine's transfer journal and kept it behind `zeus-journal`, stating that no crate in the app repository depends on redb. The journal still has exactly that boundary. The app, however, grew durable state the engine has no business holding: idempotency receipts for API mutations, workflow drafts and checkpoints, model configuration and spend reservations, ChatGPT account metadata, cloud delivery checkpoints, file-operation records with their undo outcomes, and the optional A2A bridge's task index. Each was given its own small redb database instead of new journal tables, so that product policy stays out of `zeus-core` (ADR 0011 decision 2) and Ogun, which embeds the same engine, never inherits Zeus product state.

## Decision

1. **The engine journal keeps ADR 0005's boundary.** `journal.redb` is opened only through `zeus-journal` and `zeus-engine`. No app store reads or writes its tables.
2. **App services own separate stores.** Each lives in the owner-only state directory, has one owning component and one writer, and holds no session material:

| Store | Owner | Contents |
|---|---|---|
| `api-receipts.redb` | daemon mutation receipts | Request bindings and stored results for idempotent API mutations (05 section 12) |
| `workflows.redb` | workflow service | Drafts, samples, runs, checkpoints, job and audit intents (08) |
| `models.redb`, `chatgpt.redb` | model service (`zeus-llm` runtime) | Model configuration, durable spend reservations, account metadata; secrets stay in the OS keyring (09) |
| `cloud.redb` (+ `cloud-staging/`) | cloud service | Plans, deliveries, private checkpoints and idempotency receipts (14) |
| `files.redb` | file organiser | Approval-bound operation lists and per-step outcomes for undo (05 section 9.6) |
| `queues.json` | queue service | Download queues, their members and held state, the speed limiter's settings and the scheduler defaults (05 section 9.14). A small JSON file rewritten atomically (temporary file, fsync, rename) after every change: the data is a few kilobytes, has one writer, and needs no transaction across stores because a job that is missing from it simply runs unqueued. A damaged file is set aside as `queues.json.damaged` and the daemon starts with empty queues. |
| `a2a-index.redb` | A2A bridge process | Peer task ownership and retry bindings; the bridge's only durable state (10) |

3. **Credentials and roots stay in `credentials.json`.** The verifier index is a small JSON file written atomically; service roots live beside the credential they belong to, so revocation deactivates both in one write.
4. **ARCHITECTURE section 1 rule 1 is narrowed.** `zeusd` is the only process that holds session material and the only owner of the journal; the optional A2A bridge is a second process with durable state (its index), and nothing else is.
5. **Moving a store into the journal, or a journal table out, needs its own ADR**, because it changes recovery ordering and backup consistency.

## Consequences

- A consistent backup is the whole state directory taken while the daemon and bridge are stopped (the handoff's backup procedure). Copying one store alone can replay or lose app-level effects even when the journal is intact.
- Crash recovery spans stores: for example, workflow job intents and file-operation steps are written before the engine or file system effect and reconciled against it on restart. Each owner documents and tests its own order.
- `cargo tree` layering checks must allow redb in `zeus-daemon`, `zeus-llm` and `zeus-a2a-bridge`, and nowhere else in the app.
- Schema migrations are per store. None exists yet; a versioned layout change needs a migration and a rollback note before release (roadmap R01).

## Alternatives considered

- **More journal tables.** One database, one fsync domain, but product policy and app record shapes would leak into `zeus-core`, against ADR 0011.
- **SQLite for app stores.** Familiar tooling, but a C dependency for small key-value records that redb already serves in the same process.
- **JSON files everywhere.** Simple, but without transactions the receipts and checkpoints would need their own crash protocol; kept only for the tiny credential index.
