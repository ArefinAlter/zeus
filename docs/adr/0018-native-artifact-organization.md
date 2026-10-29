# ADR 0018: Native artifact and organization requests

Accepted 29 October 2026 for the owner's requested desktop file-manager implementation.

The desktop already edits explicitly managed files through its native file workspace.
The daemon's artifact and deterministic organization APIs additionally require
`files.write`. A dedicated desktop screen should use the app's own identity and the
daemon's normal policy, record visibility and write roots.

Add `files.write` to the shipped Agent/Gui `app` profile. Migrate only the exact
previous default profile or its intermediate workflow-enabled form. Preserve IDs,
tokens, creation dates and revocation. Do not widen narrowed/custom profiles or other
GUI, MCP, host or service credentials. This is an explicit additional capability;
the app profile is no longer a read-only file API client. It has no configuration,
account grant, approval-decision or human-channel authority.

The native dispatcher exposes only artifact list/get/lineage/ingest/lifecycle
preview/apply and deterministic organizer plan/request/undo/status. Status is read-only, owner/root scoped, and reports durable pending/completed/skipped outcomes. Preview always forces
`dry_run:true`. Organization submits an exact daemon-derived operation list for
separate human approval; no files move before that decision. Artifact lifecycle
effects retain their existing record hash, plan expiry, quarantine, no-overwrite,
idempotency and destructive-approval rules. Strengthening retention and copying
available verified content need no additional invented approval.

The UI retains exact preview data and retry keys, invalidates review on edits,
shows pending approvals, and opens the verified terminal only on an explicit click.
It never types into that terminal or borrows its credential. Artifact ingestion
registers a file within daemon write roots without moving it. Record-only downloads
do not imply that a stored copy can be materialized. Unknown operations and direct
approval decisions remain unavailable through these routes.

This does not complete model classification, conversion, artifact import/export,
collection administration or filesystem-handle conversion of the daemon organizer.
Those are separate executable paths and acceptance work.
