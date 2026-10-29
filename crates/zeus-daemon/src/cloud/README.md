# Durable cloud delivery

`CloudService` joins the typed `cloud.*` API, the existing terminal-attested approval store,
private redb records, the OS keyring and `zeus-cloud` providers. The public contract is
[spec 14](../../../../docs/spec/14-cloud-destinations-and-delivery.md).

Every plan binds the caller, account revision, configured remote folder, reserved unique filename,
explicit mode, exact source identity, size, optional expected SHA-256 and limits. A delivery starts
only after an independently verified physical terminal approves those facts. Agents can request and
control their own deliveries but cannot configure accounts or create approval authority. A changed
account revision invalidates existing plans and pauses active work; it does not silently migrate an
old approval onto a new account. Configuration uses explicit bearer credentials, native OAuth
keyring bundles, or an explicitly chosen rclone configuration file. Google/Yandex PKCE and Baidu
device authorization are initiated only by the verified human terminal; regular transfers refresh
near-expiry native credentials under a per-account lock. Refresh dispatch uncertainty is persisted
before HTTP so a potentially rotated credential is not silently replayed after interruption.
See [native cloud sign-in](../../../../docs/alpha-0.1/CLOUD-OAUTH.md).

Completed-job sources must belong to the caller (human clients may inspect all), must be completed,
and must not be quarantined. They support staged upload and are never deleted. Public HTTP(S) URL
sources support the selected provider's actual modes, with no browser cookies, ambient proxy or
userinfo. Each DNS result and connected peer must be public, redirects are bounded, and HTTPS
cannot downgrade. The source must report the exact approved Content-Length; unknown lengths and
encoded representations are refused. A 60-second read timeout bounds a stalled source while the
approved `max_seconds` bounds the whole local operation. URL query strings and private upload/status
URLs do not appear in public views, errors or audit records.

Staged URL downloads use only `state/cloud-staging/<delivery_id>.part`. Full hashing is durably bound
before upload, and interrupted uploads reuse that verified file without fetching an expired URL.
Missing or changed verified staging fails closed. Streaming uses bounded readers and provider
backpressure without a complete local spool. Baidu streaming needs the approved 4 MiB block-MD5
manifest; staged Baidu computes that manifest during full verification. SHA-256 remains the content
integrity digest.

An HTTP 202 creates a pending remote operation. Reconciliation checks the reserved object before
any retry; an ambiguous import is never automatically submitted again. Native remote import has no
hard provider byte cap or dependable remote cancellation, so `allow_unbounded_remote` must be
explicitly approved. `remote_cancellation_unavailable` preserves the receipt when remote work may
continue. The global kill switch cancels local workers and also prevents new metadata-poll or cleanup
network requests until released. Cancellation signals the provider and gives it two seconds to finish
its own subprocess kill/wait path before the task is dropped; an unresponsive external process is
not reported as successfully reaped.

Only positively verified remote receipts become `completed`. Receipts distinguish actual provider
SHA-256 from local SHA-256 and a provider size check. Completion is audited before being acknowledged.
Remote cleanup is restricted by provider ownership checks; lack of sufficient evidence retains the
checkpoint for manual reconciliation. Zeus never treats partial remote content as a successful delivery.
Owned local staging cleanup is independent of remote success: `cleanup_pending` and
`cloud.local_cleanup_failed` remain visible and are retried without another upload. Failed deliveries
retain their staging; existing downloaded user files are never cleanup candidates.

Current bounded retention is deliberate: at most 64 accounts, 128 nonterminal deliveries, 128
unexpired plans per requester and 1,024 plans overall. The private store permits 4,096 records,
2 MiB per serialized record and 256 MiB total serialized state, reserving 64 MiB for updating existing
operations. Reaching a limit returns `cloud.capacity_exceeded`. Retention runs at startup and then at most hourly, in one
write transaction, and is audited as `cloud.retention`: a completed, failed or canceled delivery goes 30 days
after its last change once its staging file is gone and no cleanup is pending; a plan goes once it expired
without a live delivery; an idempotency receipt goes 7 days after it was written once the delivery it names is
gone or finished (receipts from older stores are stamped on the first pass and age from then). `remote_pending`,
`reconciling` and `remote_cancellation_unavailable` deliveries, and their receipts, are never removed.
Do not delete the private database to clear an uncertain operation: inspect the provider's reserved
object and preserve the receipt for reconciliation. These logical bounds do not cap redb's physical
file allocation or transient filesystem space.

Hermetic daemon tests cover approval and requester isolation, account revocation, global stop,
stream-read interruption, staged-file preservation/tampering, exact idempotency across restart,
uncertain dispatch, remote-pending cancellation, recovery without duplicate upload, atomic capacity
rejection and cleanup retry. Provider loopback and child-process tests are separate. No live account,
real credential, store publication or installed-provider acceptance is implied by those tests.
