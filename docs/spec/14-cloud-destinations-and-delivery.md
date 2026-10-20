# 14 — Cloud destinations and delivery

Status: Draft 1, 2026-10-16. This extends the local download and artifact contracts in [01](01-domain-model.md) and [13](13-artifacts-and-content-store.md). Approval authority remains defined by [07](07-approvals-trust-surface-and-consent.md). A provider integration and a successful local test are not evidence that a user's account has the necessary quota, access or application registration.

## 1. Scope

Cloud delivery is a separate operation from a local download. A completed local job MUST NOT imply that its cloud delivery completed. A remote operation MUST NOT report completion until the provider acknowledges a completed object and its receipt has been recorded durably.

The three explicit modes are:

| Mode | Data path | Local storage | Principal constraint |
|---|---|---|---|
| `staged_upload` | Source → verified local content → provider | Complete file while delivering | Local verification precedes disclosure to the destination. |
| `stream_upload` | Source → bounded Zeus buffer → provider | Bounded buffer; no implicit whole-file spool | Replay, known-length requirements and resumability depend on the adapter and source. |
| `remote_import` | Source → provider; Zeus submits and tracks the operation | Metadata only | The provider must offer an actual external-URL import API and be able to access the source. |

An explicit mode MUST NOT silently fall back to another mode. A plan MAY explain alternatives, but a change in data path, destination or guarantee requires a newly authorized plan. In particular, `rclone copyurl` is a relay transfer; it MUST NOT be presented as provider-side fetching.

## 2. Provider capabilities

Each configured account advertises capabilities derived from its actual adapter. Capabilities distinguish supported modes, known-length requirements, local buffering, restart/resume behavior, remote cancellation, duplicate prevention and available verification. An unavailable executable, unconfigured account, unsupported source or unsupported provider mode fails before dispatch with an actionable reason.

Provider names alone do not establish capabilities. A generic adapter MUST inspect or constrain the selected backend rather than grant every backend the capabilities of the strongest one. Unknown provider capabilities fail closed. Access restrictions and failures MUST NOT be bypassed by impersonating an official application, scraping a browser cookie store or weakening account security.

The initial adapter families are native resumable Google Drive uploads, native Yandex Disk uploads/external-URL imports, native Baidu Netdisk multipart uploads, and configured rclone remotes for additional storage services. Exact implemented coverage and real-account acceptance are recorded separately in the implementation ledger.

| Adapter | Staged upload | Bounded streaming | Provider URL fetch | Restart behavior |
|---|---|---|---|---|
| Google Drive | Yes | Exact length required | Unavailable | Persisted resumable session and preallocated file ID; reconcile before retry. |
| Yandex Disk | Yes | Exact length required | Yes; explicit weaker-limit consent | Poll recorded import operations; reconcile upload objects; no invented remote cancellation. |
| Baidu Netdisk | Yes; compute 4 MiB block MD5 manifest first | Exact length and complete block manifest required | Unavailable | Precreate and upload only missing parts, then create with no overwrite. |
| Explicit rclone remote, including MEGA and other supported backends | Backend-dependent | Exact length, bounded rcat input | Unavailable | Reconcile unique object path; automatic upload replay unavailable. |

Baidu uses the documented precreate, locateupload, multipart upload and create protocol. Returned upload origins are validated before sending the access token. MD5 is a required legacy block identifier, never the approval hash or independent remote SHA-256 evidence. Account-specific file-size and application-access limits remain provider-enforced. The implementation ceiling is 20 GiB; this does not grant ordinary accounts paid-account limits.

Generic rclone writes have no portable atomic no-overwrite precondition. Unique operation names, an existence check and refusal of ambiguous replay avoid duplicate retries but cannot prevent a concurrent external writer racing the check. Plans and documentation MUST NOT claim atomic no-clobber for that adapter.

## 3. Accounts and destinations

Account creation, credential replacement and revocation require an authenticated human channel. An agent MAY inspect non-secret capabilities and request a delivery to an existing account, but MUST NOT configure credentials, choose arbitrary executables/configuration files or broaden allowed destinations.

The configured account binds the adapter, account identity, allowed destination root and policy revision. A delivery binds a specific account revision and destination. Changing the account identity, destination root or authorization policy invalidates prior delivery authorization; an OAuth access-token refresh for the same authorized identity does not itself broaden authority.

Credential material belongs in the OS keyring or an explicitly protected credential store. Tokens, passwords, refresh credentials and upload-session capabilities MUST NOT appear in agent responses, public configuration, Debug output, logs, errors or audit records. Human account setup must not accept secrets through command-line arguments that expose them in process listings or shell history.

Rclone integration uses a configured executable and remote with validated arguments, without a shell. Agent input MUST NOT supply arbitrary rclone commands, flags, backend overrides or configuration files. Subprocess output is bounded and sanitized; a process failure does not authorize replay of an ambiguous remote mutation.

## 4. Planning and consent

Planning is read-only with respect to source content and remote objects. It checks the requested mode against configured capabilities and reports what still requires runtime verification. Possession of cloud credentials is not consent to upload a particular file or disclose a source URL.

Human review binds at least:

- The requester and delivery identity, account and policy revision, destination and conflict policy.
- The exact mode, source identity, maximum bytes and expected content hash when supplied.
- Local-copy retention, retry/replay policy, remote verification level and the limits of remote cancellation.
- Whether the provider receives the complete source URL and whether a hard remote byte ceiling can actually be enforced.

The private request hash includes sensitive source identity without displaying URL query strings. Public facts identify the origin and whether sensitive URL components will be disclosed. A changed source, mode, destination, budget or account revision requires a new approval. Existing approval hash/revision, trusted-channel and gesture requirements continue to apply.

Agents MAY submit, inspect and stop their authorized delivery operations. They MUST NOT approve them. Global stop blocks new network work and cancels local work promptly. It cannot promise to recall a remote import that the provider does not support canceling; that limitation must be visible before dispatch and retained in the resulting status.

## 5. Source access and network policy

Local sources must be explicitly authorized files or verified completed Zeus downloads. Agent-supplied arbitrary paths are not a source authorization mechanism. Quarantined content is not silently released to a provider. Temporary cleanup is limited to files owned by the delivery operation; an existing user download MUST NOT be deleted merely because cloud delivery succeeded.

URL sources are HTTP(S), with public-address enforcement on the actual connected peer and every followed redirect. Credentials are not forwarded across origins. Browser cookies and session material MUST NOT be forwarded to cloud providers. An authenticated browser source requires a separately supported, consented Zeus relay path; otherwise the request fails explicitly.

Native provider clients use HTTPS and validate upload/status endpoints against the provider's allowed origins. Returned URLs are untrusted input. Bearer credentials MUST NOT follow arbitrary redirects or provider-returned cross-origin links. Tests may explicitly use isolated loopback endpoints; this must not weaken production defaults.

Provider-side fetching shares the source URL with the selected provider. A signed URL is a credential-bearing disclosure even when no Authorization header is used. The user must authorize that disclosure. Provider-side imports cannot be assumed to honor the source validators or header policy used by Zeus's own HTTP engine.

## 6. Staged delivery

The staged path obtains a stable, verified source before initiating upload. It preserves the verified source across interrupted uploads until a remote receipt is durably recorded. A changed file is rejected rather than uploaded under an old approval/hash. Providers supporting resumable uploads retain session and confirmed-offset checkpoints privately.

Optional removal of a delivery-owned temporary copy happens only after confirmed remote completion and the requested verification level. Cleanup failure is reported separately from delivery failure and does not trigger another upload. The artifact store remains on suitable local storage; a cloud-synchronized folder is not its replacement.

## 7. Streaming delivery

The source and provider are connected by bounded buffers and backpressure. The implementation MUST bound memory, request-body chunks, subprocess pipes and metadata responses. A slow provider must stop source reads once the configured buffer is full. No helper may silently spool the entire input to disk when the user selected this mode.

Adapters requiring a known length reject missing lengths before upload. A claimed length is validated against actual bytes; truncation and excess bytes fail. A locally computed streaming digest describes the bytes Zeus observed, not an independent verification of the stored remote object.

Streaming crash recovery may require re-reading the source. The plan must disclose whether a session can resume, a source must restart, or reconciliation/manual retry is required. Non-replayable streams MUST NOT be retried as if they were durable local files. Partial remote objects and uncertain completion retain their receipt/identity for reconciliation.

## 8. Provider-side fetching

The adapter submits an external-URL import and records the returned operation identity. An accepted/queued response is pending, never completed. The daemon polls within bounded deadlines and persists state so a restart does not blindly submit the same request again.

HEAD metadata is not a hard remote byte limit: the resource can change before the provider fetches it. If the provider cannot enforce a requested guarantee, planning must reject that guarantee or expose an explicitly approved weaker policy. Detecting an oversize result afterward is not equivalent to enforcing a transfer ceiling.

If cancellation is unsupported or ambiguous, Zeus records that remote work may continue. It MUST NOT fabricate successful remote cancellation. Stopping local polling does not revoke a URL already disclosed to the provider. Deleting a resulting object is a separate authorized action and is limited to the operation-owned object.

## 9. Durability and idempotency

Submission binds a requester-scoped idempotency key to canonical request content. Reusing a key with different content fails. Replaying the same key returns the original operation, including after a daemon restart.

Before a remote mutation, persist its operation identity and available duplicate-prevention token/object identity. After a crash or lost response, reconcile using that identity before retrying. A provider with no reliable reconciliation mechanism produces an explicit uncertain state; it does not receive an automatic duplicate submission.

Account revocation, expiry, approval invalidation, global stop and process shutdown are checked before dispatch and during long-running operations. Local worker tasks and child processes must be canceled and reaped. Durable state distinguishes an interrupted worker from confirmed remote completion or cancellation.

## 10. Verification and receipts

A receipt records the provider/account reference, remote object identity, confirmed size, verification evidence and completion time. Raw upload capabilities and secret URLs remain private.

Verification levels MUST distinguish provider acknowledgement, size confirmation, matching provider checksum and independent readback where implemented. No adapter invents a remote checksum from the local digest. A mismatched expected hash/size fails verification and retains enough information to reconcile or clean up the operation-owned remote object.

Errors use stable codes and sanitized descriptions. Retryable transport failures, authentication expiry, quota/rate limits, unsupported capabilities, policy denial, integrity failures and uncertain remote outcomes remain distinct.

A cloud error's `retryable` flag MUST NOT authorize blind resubmission of a remote mutation. The daemon inspects the durable checkpoint before any retry. In particular, `cloud.reconciliation_required`, `cloud.checkpoint_mismatch`, `cloud.already_exists`, `cloud.integrity_failed` and `cloud.source_changed` require inspection; authentication/authorization failures require human attention. Provider status codes may be returned as numeric diagnostic facts, never raw provider messages or secret URLs.

## 11. Interfaces and acceptance

The daemon exposes account capability/configuration and delivery plan/submit/list/get/control operations. Agent-facing MCP exposes only safe subsets; account configuration and approval decisions remain human-only. CLI commands show the selected mode and separate source-transfer, upload and provider-import progress. A UI preview remains explicitly simulated until wired to the authenticated daemon.

Acceptance includes loopback provider protocol tests and daemon tests for stale approvals, changed destinations/accounts, idempotency, bounded streams, interrupted uploads, ambiguous import submission, completed receipts, cancel/revoke/kill switch and secret redaction. Provider responses are adversarial test input. Real-account and installed-helper acceptance is recorded separately and requires user-owned authorization.

### 11.1 Daemon methods and records

The contract is [`cloud.v1.schema.json`](schema/cloud.v1.schema.json), referenced by the daemon and MCP schemas. Plans use `clp_` ULIDs and deliveries use `cld_` ULIDs. Account aliases are human-selected lowercase ASCII letters, digits, underscores and hyphens, 1–64 characters.

| Method | Permission | Effect |
|---|---|---|
| `cloud.accounts.list` | `jobs.read` | Public scopes and actual capabilities; no credentials or local configuration paths. |
| `cloud.accounts.configure` | `config.write`, verified physical terminal | Create/update explicit scope and write credential to keyring. Updates require the reviewed revision. |
| `cloud.accounts.revoke` | `config.write`, verified physical terminal | Increment revision, revoke scope, stop local workers; retain uncertainty about continuing provider operations. |
| `cloud.plan` | `jobs.write` | Persist immutable dry run and hash. Reads no source bytes and creates no remote object. |
| `cloud.submit` | `jobs.write` | Reserve delivery and raise `cloud_delivery` approval; does not authorize transfer. |
| `cloud.list`, `cloud.get` | `jobs.read` | Agent callers see their own deliveries; human callers may inspect all. |
| `cloud.control` | `jobs.control` | Pause/resume/cancel within the original authority; never broadens it. |

Mutations require a caller-scoped `idempotency_key`. A plan binds account revision, source hash, mode, unique remote name, exact size, byte/time budgets, expected SHA-256 when known, provider cancellation limits and remote-fetch disclosure. `max_seconds` is 1–86400. A `public_url` source includes `size_bytes` and optionally `sha256` and `block_md5`; the other source kind is `completed_job` with `job_id`. No source kind accepts an arbitrary local path or browser session.

```json validate=cloud.v1#/$defs/CloudPlanParams
{
  "account_id": "research",
  "mode": "staged_upload",
  "source": { "kind": "public_url", "url": "https://files.example.org/report.zip", "size_bytes": 1048576 },
  "remote_name": "report.zip",
  "max_bytes": 1048576,
  "max_seconds": 1800,
  "allow_unbounded_remote": false,
  "idempotency_key": "research-report-plan-1"
}
```

```json validate=cloud.v1#/$defs/CloudSubmitParams
{
  "plan_id": "clp_01M45MN5M0PZ0D9W0APDCGRNZD",
  "plan_hash": "bd7268e060112daf04c306d6e71be7fab7b2eb41cb587462ae38fc6697457268",
  "idempotency_key": "research-report-submit-1"
}
```

Delivery states are `awaiting_approval`, `queued`, `running`, `remote_pending`, `paused`, `reconciling`, `completed`, `failed`, `canceled`, and `remote_cancellation_unavailable`. `committed_bytes` counts provider-confirmed progress. `artifact` is completion evidence. `cleanup_pending` marks failed cleanup of a Zeus-owned local temporary copy without discarding a successful remote receipt. Public output contains sanitized reason codes, never provider response bodies or private checkpoints.

### 11.2 Agent and human surfaces

`zeus_cloud` (06 section 6.10) exposes account listing, planning, submission, listing, inspection and control. All mutations require intent keys. It exposes no account mutation or approval decision. A pending delivery directs the agent to ask the person; it never suggests using browser-session consent to authorize a cloud upload.

The CLI provides `zeus cloud accounts`, `plan`, `submit`, `list`, `get` and `control`. Native account credentials are entered on the physical terminal without echo, never in arguments. The initial implementation accepts existing authorized access tokens; OAuth application provisioning, interactive browser/device-code flows and automatic refresh are separate work. Rclone authentication is managed by its explicit protected configuration. Configuration and executable hashes are pinned at human setup and checked before launch; changed files require human reconfiguration.

### 11.3 Provider references

- [Google Drive resumable uploads](https://developers.google.com/workspace/drive/api/guides/manage-uploads) and [pre-generated file IDs](https://developers.google.com/workspace/drive/api/guides/create-file).
- [Yandex's official client, including saveFromUrl](https://github.com/yandex-disk/yandex-disk-restapi-java/blob/master/disk-restapi-sdk/src/main/java/com/yandex/disk/rest/RestClient.java).
- Baidu's [precreate](https://pan.baidu.com/union/doc/基础网盘服务/上传/预上传/), [locate upload](https://pan.baidu.com/union/doc/基础网盘服务/上传/获取上传域名/), [multipart upload](https://pan.baidu.com/union/doc/基础网盘服务/上传/分片上传/) and [create file](https://pan.baidu.com/union/doc/基础网盘服务/上传/创建文件/) contracts.
- [rclone rcat](https://rclone.org/commands/rclone_rcat/) describes known-length uploads and replay limits; [rclone MEGA](https://rclone.org/mega/) describes backend-specific access.

The current daemon bounds outstanding deliveries, account/plan counts and private record sizes. It retains idempotency and uncertain-operation evidence and returns `cloud.capacity_exceeded` rather than silently dropping it. Exact current limits and cleanup behavior are in the [daemon implementation guide](../../crates/zeus-daemon/src/cloud/README.md). Automatic retention removes finished deliveries 30 days after their last change, expired plans without a live delivery, and idempotency receipts a week after they were written once their delivery is finished; uncertain or remote-pending operations and their receipts are kept.

## Open issues

- Validate live provider accounts, OAuth application registrations, scope restrictions and quotas without treating simulated provider tests as vendor acceptance.
- Complete human OAuth onboarding and refresh for native providers. Baidu application eligibility and account limits require live acceptance; no verified public remote-import API is enabled.
- Broaden provider-specific resumability, cancellation and readback verification as those APIs permit; retain explicit capability limits.
- Reconcile the implementation and generated API schemas with this draft before freezing the cloud contract.
