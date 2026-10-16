# 12 — Security invariants and verification

Status: Draft 1, 2026-10-16. Consolidates [00](00-conventions.md), [02](02-native-messaging-v1.md), [03](03-journal-and-commit-protocol.md), [04](04-audit-log-and-provenance.md), [05](05-daemon-api-v1.md), [07](07-approvals-trust-surface-and-consent.md), [08](08-workflow-plan-v0.md), and [13](13-artifacts-and-content-store.md). Those documents define the wire formats and algorithms; this document defines release evidence and failure boundaries.

## 1. Threat model

Protect browser sessions, the user's filesystem, durable transfer progress, consent decisions and model-spend limits from malicious remote content, unauthenticated local peers, agent mistakes and replayed requests. Treat pages, response headers, filenames, model output, agent text and self-declared client names as untrusted.

A privileged attacker or malicious program running as the same OS user can read that user's files, modify executables and automate a genuine terminal. Binary checks, private files, request hashes and typed confirmation raise the cost and preserve evidence; they do not provide a security boundary against arbitrary same-user malware. Product onboarding and the approval UI must not promise otherwise. A missing OS attestation is an unavailable capability, never a successful check.

## 2. Required invariants

| ID | Requirement | Required adversarial evidence |
|---|---|---|
| SI-01 | Only the authenticated credential determines effective kind, role and permissions. Human decisions additionally require verified channel evidence. | An agent claims CLI/TUI/browser roles and every surface; no grant or policy mutation results. |
| SI-02 | The daemon has one owner-only local listener. The OS supplies peer identity; remote named-pipe clients and foreign users are refused. Server proof precedes credential proof. | Wrong user, squatted endpoint, second instance, invalid server proof, failed handshake and timeout. |
| SI-03 | No path reads browser cookie databases or profile storage. Session release uses the installed extension and verified native host. | Source review and fake filesystem deny access; browser-parent and extension-identity mismatch tests. |
| SI-04 | Cookie names and values, tokens, secret URL components and keys never enter logs, audit, events, tool results or model prompts. Session buffers are bounded and wiped when no longer needed. | Canary markers across success, redirect, failure, cancellation, expiry and diagnostic paths. |
| SI-05 | Session material is attached only to the exact granted HTTPS origin and applicable cookie domain/path. Redirects do not widen consent. | Cross-origin redirects, wrong origin, expired lease, overlarge cookies, partitioned and malformed cookies. |
| SI-06 | A decision binds the complete canonical facts, requester, approval id, revision and expiry. It is single use and audited before authority is created. | Stale hash/revision, altered facts, replay, concurrent decisions, changed subject, expired request and audit-write failure. |
| SI-07 | Every approval surface recomputes the hash and validates the closed facts schema before offering approve. Untrusted annotations are separately labeled. | Unknown fields/types, hash mismatch, terminal control/bidi text, stale UI, queued terminal input and forged browser gesture. |
| SI-08 | The kill switch survives restart, revokes allow grants, preserves never entries, halts network admission and prevents new commits under revoked consent. | Stop during request, fsync and commit; restart; late lease response; blocked agent release; human release does not silently resume jobs. |
| SI-09 | Paths remain inside allowed roots after canonicalization. Symlinks/reparse points, hardlinks and file identity changes cannot redirect writes or destructive operations. | Symlink/junction escape, path swap, traversal, reserved names, filename collision and foreign destination replacement. |
| SI-10 | Durable committed ranges describe only bytes already flushed to storage; epochs fence stale writes. Recovery validates file identity and validators before resume. | Fault injection around write/fsync/transaction/final rename; changed ETag, wrong Content-Range, premature EOF and local corruption. |
| SI-11 | Final files are verified with both hashes. Unsafe or disguised content remains quarantined with explicit reasons. | Hash mismatch, disguised HTML/executables, MIME mismatch, canceled verification and quarantine bypass attempts. |
| SI-12 | Audit records form a durable hash chain and write-ahead actions fail closed when logging fails, except emergency stop. | Tamper, deletion, reordering, rotation, partial tail, disk full and retained external anchor comparisons. |
| SI-13 | Native streams have bounded frames, outstanding requests and credits; replies cannot cross profiles or connections. | Oversized length prefix, depth violation, wrong response id, late response, duplicate id, missing credits and reconnect. |
| SI-14 | MCP/A2A never expose decision operations or generic unrestricted RPC. Their returned remote text is labeled untrusted. | Catalogue/policy consistency, forged tool names, input schema rejection and end-to-end agent workflow. |
| SI-15 | Workflow plans contain no arbitrary executable code. Domain and byte/file/page/time/depth budgets apply to every expansion and redirect. | Cycles, endless pagination, drift, robots denial, redirect escape and cancellation while checkpointing. |
| SI-16 | Models cannot bypass approval or spend/privacy policy. API credentials stay in the OS keyring; unavailable policy/key material cannot enable a provider. | Prompt injection, secret redaction, concurrent spend reservation, stale/forged policy, OAuth state replay and provider fallback. |
| SI-17 | Artifact identity, provenance and physical content are distinct. Collection cannot delete user files or protected/reachable objects. | Duplicate ingest, interrupted rename/index commit, double-digest import check, hardlink mutation, quarantine, retention and repair. |
| SI-18 | Agent/service views isolate private approval payloads by requester. Credentials and idempotency keys are scoped to authenticated identity and method. | Cross-client reads, changed payload under same key, restart retry and concurrent conflicting writes. |

A test name alone is insufficient evidence. Record OS, toolchain, command, commit/worktree identifier and outcome. Mark unsupported platforms and unexecuted tests explicitly. Hermetic tests use synthetic credentials and loopback servers; they do not inspect installed browser profiles or contact production model providers.

## 3. Failure handling

Authorization, durable state, schema integrity, executable verification and audit failures refuse the dependent operation. Error messages contain registered codes and safe context, not the original request or third-party response body. Partial work is visible and resumable where safe; no error is translated into completed, verified, approved or published.

Emergency stop prioritizes removing authority even when the audit disk is unavailable. Its pending audit record must be retained for later append. Verification/finalization already using no session may finish; stopped transfers require deliberate restart after the latch is released. Revocation and durable commit share the journal serialization boundary.

Unknown contract fields affecting authority, budgets, destinations or plan behavior are rejected, not silently dropped before hashing. A schema-version upgrade has a migration path and fixtures. A service that cannot interpret a newer approval can show deny or defer, but never approve.

## 4. Resource and process boundaries

The daemon owns its journal and audit store for its whole lifetime. A second process cannot silently take over a live socket or unlock another owner's state. Connection cleanup cancels subscriptions and outstanding browser requests without canceling independent downloads. Authentication, read/write stalls, replay, wait operations, HTTP responses and filesystem enumeration all have explicit bounds.

Conversion tools are separate processes with fixed argument construction, resource limits, cancellation and confined input/output directories. Filenames and extracted page text never become shell commands. Destructive organization defaults to a preview and uses the approved operation hash; trash restore never overwrites a file that appeared later.

HTTP bridges are separate disabled-by-default processes, with loopback/token/Host/Origin checks and the agent permission set. Provider HTTP calls and source requests occur only as a consequence of authorized user work. Telemetry is absent.

## 5. Release evidence and API freeze

Before public alpha, run the crash/recovery matrix and full browser → native host → daemon → engine acceptance paths on real Chrome and Edge. Test revocation, expiry, browser suspension, lost connections and stream backpressure. The roadmap's 200-URL rescue corpus, minimum success rate, memory budgets and segment throughput comparisons need recorded measurements; unit tests do not substitute for those gates.

Before phase-2 API freeze, validate Firefox behavior, workflow checkpoint compatibility, model credential lifecycle, conversion confinement and the artifact migration story. Freeze only after canonical schemas, generated Rust/TypeScript, examples and front-end method catalogues agree. Publish the supported platform/protocol matrix and residual risks with the release.

## 6. Open issues

Cross-platform syscall-level path race hardening, macOS peer executable/terminal attestation, real-browser acceptance, browser-store signing and deployment identities, external audit anchoring, conversion sandbox profiles, provider policy-signing key provisioning and production performance measurements remain explicit gates until evidence is recorded. This document does not assert that every invariant already has a passing implementation.
