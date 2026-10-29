# 0022: Durable file workspace and reviewed directory sync

Status: accepted for implementation by the owner's instruction to finish the non-mobile file-manager roadmap. Copy and comparison implementation is in validation; daemon synchronization remains a separate integration phase.

## Decision

Share durable local copy and bounded recursive comparison through a small `zeus-file-workspace` Rust crate. Desktop and daemon acquire their own existing root capabilities and provide current-authority checks. The library does not create grants, decide approvals, invoke a shell, read browser credentials or widen scopes. Keep application error/transport adapters outside the library.

A durable copy is a frozen identity-bound source manifest, an exclusive private staging tree on the destination volume, and a checksummed append-only receipt. Chunk data reaches disk before its digest checkpoint. Every restart holds unfinished work until explicit resume verifies the original source identity, full native modification time and committed source/staging digests. Publication is no-replace; uncertain initial object creation is held for manual recovery, never silently adopted. A shared worker/admission limit bounds scans, verification and transfers; receipt and staging disk budgets are independent. Disk-full, cancellation and revoked scopes preserve existing data and the last synced checkpoint.

Recursive comparison is read-only and bounded across both trees. A metadata match is distinct from content equality established by SHA-256. Directory rows do not claim recursive byte equality. Any incomplete scan is unsuitable for an executable sync plan. The full internal manifest retains root, parent and entry identities; public result pages expose only bounded comparison data.

Directional synchronization is daemon owned. A new exact plan binds the source and destination manifests, content digests, current visible roots, every copy/create/trash step and the chosen direction. The existing human-only organizer approval, durable operation hash, audit, idempotency, kill switch and current-credential restrictions control execution. The GUI and agents can propose and observe; neither decides approvals. Missing files use the shared durable copy worker. Replacements and destination-only removals first preserve the approved existing object in Zeus trash with its identity-bound Undo receipt. No implicit overwrite or broad root grant bridges the native index and daemon scope sets.

The file-workspace report records actual limits, interruption tests and release status. Existing public binaries remain unchanged until the integrated replacement has passed acceptance. Native filesystem reads have cooperative deadlines; a blocked operating-system call is not forcibly cancelled. Unix retained handles do not provide an atomic snapshot against a concurrent process under the same user. Windows directory sync has no portable power-loss guarantee.
