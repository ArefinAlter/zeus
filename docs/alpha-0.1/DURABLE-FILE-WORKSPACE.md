# Durable copies and recursive folder work

This continuation builds on ADR 0016. It does not change the already published alpha binaries.

## Copy receipt contract

A copy starts with a complete, bounded no-follow manifest. It binds the selected read and write root identities, original source parent and every source entry identity, full native modification times, destination parent identity and exact final name. Directories are compared as directories; they are not treated as content hashes.

The worker records a durable intent before creating a private `.zeus-partial/copy-<id>` container on the destination volume. Every owned staging object gets an identity receipt before data is written. Each 4 MiB chunk is synced before its SHA-256 checkpoint is synced. A failed checkpoint never authorizes its bytes. Partial trailing receipt frames are retained and only truncated to the last verified frame during an explicit resume of that same receipt. Invalid complete frames, missing identity records and unexpected staging entries require manual recovery.

On restart Zeus only loads receipts. Interrupted, cancelled and failed jobs remain visible and perform no filesystem mutation. Resume obtains current scopes and quarantine facts, matches all recorded root/parent/source/staging identities, and verifies every committed chunk against both the source and staging bytes. Source identity, original size and full native modification time must still match; matching bytes do not authorize a changed source. Uncommitted tails are truncated only on the recorded staging object. Publication records its intent first and atomically moves the complete top-level item without replacement. A crash after publication is reconciled by the recorded destination identity and verified bytes, never by filename alone.

Copy work is limited to four active workers, 64 retained receipts, 100,000 entries and 128 levels per job. Scans and resume verification share a 120 second cooperative deadline per explicit action. A filesystem call already in progress cannot be forcibly interrupted by that deadline. Each receipt is limited to 128 MiB, with a separate 512 MiB aggregate receipt budget; manifests are limited to 64 MiB. Staging reservations are limited to 256 GiB per job and 1 TiB across unfinished jobs, independently of free-space admission. No quota failure, disk-full error or cancellation deletes source data or discards recovery receipts. A root/configuration change pauses active work at the next checkpoint. Shutdown requests cancellation and leaves checkpoints; it never resumes them on the next startup. There is no automatic expiry or cleanup.

## Recursive comparison and sync phases

Recursive comparison is a bounded, cancellable read through retained directory capabilities, with paged results. A metadata match is explicitly different from verified byte equality. Any unreadable, replaced, linked, out-of-scope, capped or timed-out tree is incomplete and cannot produce an executable synchronization plan.

Directional sync consumes an exact frozen plan. Missing files use the durable copy engine. Replacement and destination-only removals must be proposed through the daemon's existing organizer approval, audit, kill-switch and idempotency path and preserve Zeus trash Undo. The daemon's current visible roots constrain that operation; native index permissions cannot silently widen them. Neither the GUI nor agents decide destructive approvals. These phases remain in progress until implementation and acceptance evidence is recorded here.

## Native copy and comparison checkpoint

The first implementation passed all 82 native Windows index tests, including twelve durable-copy regressions and four recursive-comparison regressions. These cover explicit restart resume, source content edits with preserved size/time, original mtime changes, source/staging/destination identity substitution, torn receipt frames, uncommitted data tails, publication-before-receipt recovery, unexpected staging entries, single-store ownership, worker limits, aggregate verification deadline and injected receipt-budget exhaustion. The full frontend suite passed 59 tests with TypeScript and a production build. One strict-Clippy collapsible-if finding was corrected after the test pass; strict will run again on the shared extraction. Logs are in `D:/verne-build/file-workspace/evidence/` and `frontend.log`.

This is native unit/integration validation; installed binary UI acceptance, Linux execution of this continuation and the daemon synchronization phase are not yet complete. The already published alpha remains unchanged. A copy of a folder containing `.zeus` or `.zeus-partial` is refused explicitly instead of silently copying or skipping private recovery data. Interrupted initial creation before its identity checkpoint requires manual recovery. Resume and final revalidation have a cooperative time bound; very large or slow verification may remain held rather than claiming completion.
