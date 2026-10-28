# Zeus 0.1 alpha implementation ledger

Started 28 October 2026, Asia/Dhaka. Owner instruction: implement the remaining Zeus roadmap, including IDM-style dynamic transfers/media grabbing and an Everything-style file workspace plus uploads; analyze a separate Go Bubble Tea/Lip Gloss TUI and device-wide integration.

This is a work ledger, not release evidence. An item is complete only after its executable path and appropriate checks pass. Existing 0.0.1c acceptance is a baseline, not evidence for newly added alpha behavior. The target includes zeus-core, app, extension and site; Ogun compatibility must remain intact. Sources are isolated under D:/verne-build/0.1 and /tmp/zeus-alpha. Research-tool forks are D:/ghidra and D:/rea. Outputs, tool runtimes and caches go on D:; do not consume the nearly full C: drive.

## Confirmed release scope

Owner clarification on28 October: **implement everything except Android/iOS apps**. Desktop feature work, R05 optional source adapters, R06 team/sync/SSO service capabilities and release tooling remain in scope. The Go TUI/device-wide takeover is a separate feasibility assessment as originally requested; the report does not claim a Go client or interception driver is implemented. External account registration, real signing identities, hosted CI billing and physical trust-surface acceptance require their actual operational evidence.

## Latest validation checkpoint

- Core adaptive transfer/control/HTTP:99 native Windows tests passed (51 control,34 engine,14 HTTP), plus strict Clippy. The paced64MiB1/4/16-connection benchmark completed with exact SHA-256; see TRANSFER-EVIDENCE.md. New identity-bound source hooks, held admission and header-latency estimator changes require their next rerun.
- Native file index/archive/Everything:46 Windows tests and strict Clippy passed;11 native file-UI checks and5 real isolated Everything SDK broker checks passed. Inbox runtime and newly added agent policy checks are queued separately.
- Native media:25 tests plus strict Clippy passed before the final expanded helper cases. Real pinned FFmpeg9.0.2 acceptance now verifies video+audio→MP4 and TS→MP4/MKV, AAC framing/decoding, caps, pin changes and offline publication recovery. The final combined regression rerun is running.
- Cloud OAuth/staging:26 unit+6 subprocess provider tests,89 API+8 IPC tests,31 daemon cloud tests (including11 OAuth),2 CLI callback tests and strict Clippy passed. No live provider sign-in is claimed. A follow-up native provider-switch credential isolation fix awaits its own rerun.
- MCP catalogue:14 tools,35,194 compact bytes within40,960-byte budget;259 spec examples pass. All10 inbox terminal commands and owner-scoped MCP operations are implemented; native interface tests are queued. The read-only App monitor passed12 fresh-browser fixture checks, with desktop/narrow screenshots and no external requests or page errors. Installed-host acceptance remains distinct.
- Tool forks are cloned on D:; portable Rea/Ghidra completed static reference analysis. Exact observations and evidence are in REFERENCE-FINDINGS.md; no proprietary implementation code is copied into Zeus.

## Reference evidence

Installed reference versions, read from executable resources: IDM 6.42.64.2 and Everything 1.5.0.1391a. Analyze copied executable bytes with Ghidra/REA, record their hashes and distinguish observations from inferences. Keep raw proprietary analysis out of the open-source implementation; derive behavior and independently implement protocols/algorithms. Do not read activation data, private indexes or browser profiles. Native targets are not executed by the static-analysis tools.

## Implementation tracks

| Track | Current work | Required completion evidence |
|---|---|---|
| HTTP execution | Integrate adaptive segment sizes, live splitting/work stealing, per-host limits/AIMD/fairness and safe partial-request recovery. | Actual engine requests change size/concurrency; heterogeneous-worker and throttle tests, exact hashes, cancellation/restart, no overlapping committed writes; measured 1/4/16 connection runs. |
| Media | Discover and review direct media plus HLS/DASH, stream/quality selection, bounded segment execution and assembled-file verification. Optional trusted helper capabilities are explicit. | Loopback master/media playlist and MPD fixtures, range/redirect/expiry behavior, cancellation/restart and accurate unsupported cases. |
| File workspace | Fast scoped indexing/search, incremental change handling, file identity, durable operations, duplicate/size views and file manager controls. Evaluate native NTFS/USN and Everything's public IPC/SDK with Ghidra evidence. | Scoped fixture searches and journal replay/gaps; index exclusions versus browsing; collisions, links, interruption and undo; measured index startup and query latency. |
| Uploads/cloud | Complete account lifecycle, supported provider progress/resume/cancellation and source staging/recovery, UI and receipts. | Native wire fixtures and restart/idempotency tests; real account acceptance clearly separated where credentials/registrations are required. |
| Workflows/artifacts | Authenticated pages, extraction/pagination/continuation, conversion, classification and artifact maintenance surfaces. | Executable paths with matching schemas, policy/visibility/budget/approval tests and recovery evidence. |
| Browser integration | Refresh/expiry/redirect flows, media capture, stable development identities, reconnect/backpressure and protocol parity. | Dedicated fresh browser profiles only; hermetic extension/host/daemon tests and physical-person-only trust decisions. |
| Workspace/agent interfaces | Event replay and virtualization, persistent inbox/packages, queue/DST/network policies, rich run actions, CLI parity and bounded MCP/A2A profiles. | Actual daemon state, disconnected/replay tests, schema/catalogue parity and negative authority tests. |
| Release and contracts | Reconcile every C/W/M/D/R roadmap item against current code, public alpha metadata, reproducible packages, integrity/recovery and support matrix. | Strict Rust checks, frontend/extension checks, platform packaging and exact evidence; CI billing, signing certificates, store identities, real hardware and external-account gates stay explicitly recorded. |

## Roadmap reconciliation

C01-C05; Phase 1 HTTP/scheduling/filesystem/journal/artifact/daemon/approval/browser/agent items; W01-W06; M01-M05; D01-D06; Phase 3 workspace/interoperability surfaces; R01-R06 all require explicit disposition. Do not erase or relabel unresolved work as completed. Mobile/other hardware, external audits, certificates and service registrations cannot be inferred from desktop protocol tests.

## Device-wide feasibility

A separate report will distinguish: cooperative URL/download handoff; per-app proxy/routing and bandwidth policy; Git/Git LFS/package-manager adapters; Windows BITS/Delivery Optimization; protocol-specific resumable uploads. A network filter alone cannot turn arbitrary encrypted/proprietary transfers into parallel HTTP file requests. No system proxy, root certificate, network filter driver or update policy is installed as part of this assessment. The Go TUI is a client of the existing Rust daemon and agent APIs; no new approval authority is given to agents.

## Desktop resize and restored interface checkpoint

All four desktop panel fixes are merged: Ogun PR24, Asten PR45 plus keyboard/ARIA follow-up PR46, Dynodoc PR4 and Zeus PR30. Frontend checks/builds, 28 additional workspace views, Windows native release builds and fresh real WebView2 acceptance passed. Asten also passed all52 frontend tests. Existing release downloads were not replaced. Zeus's frontend patch is applied to this alpha worktree and typechecks.

The restored alpha interface run passed51 MCP tests and59 workflow tests, plus strict Clippy. Sitemap discovery/restart and the bounded HTTP/stdio/App/inbox/lifecycle paths are covered. The optional team service now has7 passing actual Node24 loopback tests covering OIDC PKCE/nonce/key rotation, one-use enrollment, tenant/role isolation, CAS/restart/tombstones, session rotation/revocation, distinct DSSE domains/audit and metadata boundaries. No real IdP, native team guard or team deployment is claimed.

The engine follow-up now passes102 tests plus a dedicated cancellation/drain regression, with strict Clippy. The original slow-tail fixture's short delay was unreliable on a busy Windows disk; the strengthened fixture stalls the original response after3MiB and requires actual reassignment, preserving exact final bytes. The API/cloud/daemon/CLI batch passed383 tests after registering the new OAuth error codes, and strict checks passed. Uncertain refresh outcomes explicitly remain non-retryable. Ogun's operators/LLM compatibility batch passed74 tests;4 real-provider tests remain intentionally unrun.

The final sitemap integration now passes 62 workflow tests, including bounded gzip integrity/expansion, strict UTF-8 and XML→HTML dry-run content negotiation. The daemon forwards those Accept choices and classifies malformed documents without echoing bytes. The expanded API/cloud/daemon/CLI batch passes 384 tests and strict Clippy. The alpha CLI and its enabled dependencies pass an actual Rust 1.90 all-targets check.

Subscription reference check: current T3 documentation offers both existing CLI authentication and its own explicit ChatGPT authorization flow. Atlas external agents use ACP subprocesses. Official Grok Build now documents `grok login`, headless mode and `grok agent stdio`; a verified Grok adapter is still separate work. Its tool allowlist retains MCP meta-tools unless explicitly denied, so enabling it requires more than adding a provider name. No browser-token reader or private web-endpoint proxy is introduced.

The native two-pane manager now passes20 actual-harness checks, with independent paged/virtualized folders, existing reviewed copy/move operations and bounded selected-file hashing. Compact windows preserve each pane behind a Left/Right switch. A metadata match never claims identical content. The existing six frontend test files, typecheck and harness build pass; a stale13-tool assertion was corrected to the implemented14-tool catalogue. See FILE-WORKSPACE.md for evidence and remaining file capabilities.

The R05 acquisition library now passes12 native tests, all-feature strict Clippy and an actual Rust1.90 all-feature/all-target check. The pinned SDK intentionally disconnects on upload requests; the peer fixture now delivers requested valid data first, and separately proves corrupt pieces cannot produce a verified receipt. Daemon/account/review/publication wiring and broader torrent discovery remain explicit work in SOURCE-ADAPTERS-PLAN.md.

Independent source feature configurations (types-only, S3, Hub and torrent) also pass strict Clippy. The complete specification validation passes259 examples, and the14-tool MCP catalogue occupies35,194 bytes within its40,960-byte limit. Desktop help text and existing MCP/agent acceptance expectations now include the inbox tool.

Fresh native daemon/CLI/host binaries passed the actual bundled stdio MCP scenario (20 checks: generated configuration,14-tool handshake, download/hash verification and approval refusal) and the synthetic-provider agent/UI scenario (43 checks: both provider protocols, tool permissions, consent/offline/caps, cancellation, rendered thread/details and key non-disclosure). Logs are `D:/zeus-alpha/logs/s09-mcp.mjs.log` and `s10-agent.mjs.log`. These are loopback fixtures, not live subscription-provider inference or external MCP-host certification.

The native desktop now has bounded durable journal replay and an atomic display-history cache. Failed or overlapping snapshot requests cannot clear/race the current job list; replay never overwrites current job state. Four daemon replay tests, 20 app tests, 98 API/IPC tests, strict Clippy, 58 frontend tests and builds pass. Both CLI and app pass actual Rust 1.90 all-target checks; specification validation now covers 261 examples. The actual native/Chromium recovery scenario passes 16 checks, including missed pause/resume transitions, exact completed SHA-256, failed reads, no overlapping snapshot requests, webview reload, daemon restart, corrupt-cache recovery and refusal of mutations through the history route. Evidence: `D:/zeus-alpha/evidence/history/20261028-213054/`. See DESKTOP-EVENT-RECOVERY.md for bounds and remaining snapshot/push-delivery limitations. The release is still incomplete; drafts are Zeus #31, zeus-core #11 and zeus-extension #4.
