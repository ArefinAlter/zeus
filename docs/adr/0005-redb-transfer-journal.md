# ADR 0005: redb as the transfer journal

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |
| Specs affected | 03 |
| Owner repository | `zeus-core` (crate `zeus-journal`) |

## Context

Resumable downloads need a durable record of which byte ranges are committed, job state, grants and events, with atomic multi-row updates and crash safety. The daemon is a single process. The record must survive power loss, never trust uncommitted bytes, and recover quickly at startup.

## Decision

1. The journal is a single embedded **redb** (4.x) database, `zeus.redb`, in the owner-only state directory (spec 03 section 3). redb is pure Rust, ACID, copy-on-write, one writer at a time and many readers, and detects torn commits with checksummed commit slots.
2. `zeus-journal` hides redb behind a `JournalStore` trait so SQLite (WAL, `synchronous=FULL`) can replace it without touching `zeus-engine`.
3. The data file is `<final name>.zpart` in the destination directory; only ranges committed to the journal after the data was synced are trusted (the commit protocol and its twelve invariants are spec 03).
4. The app repository does not open the journal directly except through `zeus-engine` and `zeus-journal`'s public API (events replay, audit anchors). No crate in `zeus` depends on redb.

## Consequences

- Recovery is a bounded, testable algorithm (03 section 10) rather than a heuristic scan.
- Single-process ownership is part of the design (the daemon holds `zeusd.lock`); tools that want to read the journal ask the daemon.
- redb 4.x requires a recent compiler; the workspace MSRV follows it.

## Alternatives considered

- SQLite: mature and inspectable, but a C dependency and a heavier build; kept as the fallback behind the trait.
- sled: unreleased 1.0 for years; avoided for new work.
- Ad-hoc journal files: easy to get wrong; this is the part that must not be wrong.
