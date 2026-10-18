# Implementation status and remaining roadmap work

Reviewed 2026-10-16. This is an implementation ledger, not a release announcement or an API freeze. The original roadmap exit criteria remain in [ROADMAP.md](../ROADMAP.md). All three implementation repositories must be updated together because the app uses a sibling path dependency on `zeus-core`.

The [2026-10-18 project handoff](PROJECT_HANDOFF.md) records the exact companion revisions, operating instructions and additional source-review findings; its [discrepancy register](PROJECT_HANDOFF.md#known-gaps-and-documentation-discrepancies) includes unwired configuration, emergency-stop audit recovery and missing model-to-product integration. The [end-to-end roadmap](../ROADMAP.md) covers all four repositories, including the site preview. The test results below remain the October 16 baseline, not a fresh rerun.

## Roadmap position

| Phase | Implemented in this change | Still required to finish the phase |
|---|---|---|
| 0 — Contracts and skeleton | Draft specs 09–12; shared approval/hash/revision, grant, event and artifact types; synchronized fixtures and generated schemas; cursor-aware waiting; private approval decision records. | Review the new drafts; finish every spec 05/07/08 wire and behavioral discrepancy; service-client contract; verify the remaining research corpus. API v1 is not frozen. |
| 1 — Working MVP / public alpha | Executable HTTP download engine, persistent daemon, file verification and recovery, native relay/browser proxy, MCP stdio, CLI/live TUI, durable approval/grant/kill-switch enforcement and integration tests. | Real installed-browser acceptance, platform containment and trust review, fault-injection coverage at every crash point, the 200-URL rescue benchmark and ≥85% result, memory/throughput benchmarks, packaging/signing and release review. |
| 2 — Workflows and models | Public-page workflow extraction/pagination, approval-bound execution, budgets/checkpoints/outbox, model HTTP/OAuth adapters, keyring integration, spend reservations, auth wizard; Chrome/Firefox build targets and extension contract tests. | Authenticated document/page rendering, remaining workflow strategies/report fields, sandboxed converters and organizer, delegated model rungs, production policy-key distribution, live provider/keyring/browser validation and API freeze. |
| 3 — Interoperability / GUI | Optional local A2A bridge and a researched, interactive download-workspace prototype. | Full Tauri application, restricted GUI approval channel, MCP App views and production queue/scheduling/inbox controls. |

## Repository changes

### zeus-core

The engine now opens a redb journal, admits jobs, probes HTTP(S) origins, fetches bounded ranges, commits progress, verifies SHA-256/BLAKE3, finalizes files and recovers incomplete work. It validates range responses and representation changes rather than assuming that a server honors resume requests. Pause/cancel/retry controls have real effects. Automatic retries persist deadlines and bounded failure-class budgets, honor Retry-After, release scheduling slots while waiting and re-arm after restart. Job state cursors are written with their events; keyed control receipts are stored with transitions.

`zeus-fs` performs real file operations, root/path checks, safe name handling, atomic finalization, trash and OS provenance writes. `zeus-verify` provides file verification, durable audit chains and idempotent provenance. `zeus-journal` implements redb transactions, commit rounds, invalidation and recovery. The engine checks the durable kill latch and current grants at commit boundaries.

`zeus-artifact` adds the shared content-addressed library: content ingest, metadata, materialization, deduplication, import, retention/collection and integrity repair. The daemon initially uses **record-only** ingest for completed downloads so it does not silently duplicate every downloaded file. Full artifact management is not yet exposed through all application interfaces. Reflink optimization and adversarial filesystem path-race hardening remain review items.

Transfer concurrency is bounded. Adaptive concurrency/rate/fairness integration, mirror selection, hedging and advanced estimators remain unfinished. Read the core crate table for exact algorithm status. Passing recovery tests is not evidence that every power-loss point has been tested on every filesystem.

### zeus

`zeusd run` now owns the local listener, daemon lock, discovery, engine, consent store, audit/provenance outbox and workflow loop. IPC has owner checks, connection/request bounds, authentication deadlines, a bounded writer and connection cleanup. Shutdown acknowledges its accepted grace period before broadcasting the stopping notification, refuses new work and drains existing IPC requests to the deadline. Slow journal/file synchronization can retain the process lock beyond that deadline to preserve writes. Event subscriptions replay durable events, respect reply ordering and signal lag; wait cursors prevent a repeatedly polled blocked job from appearing newly changed.

Human/admin credentials from unverified executables are downgraded before the authentication reply. Terminal decisions independently require the installed executable and terminal evidence. The native host checks its browser parent, and browser decisions bind the extension/profile, surface, gesture, hash and confirmation. Private decision evidence is stored atomically with the terminal approval and grants. Browser session material remains memory-only. Global stop persists, revokes grants and pauses jobs/runs; release restores neither grants nor work.

`zeusd init` registers a complete installed sibling package. `zeusd trust-clients` supports a stopped-daemon upgrade with a controlling-terminal review of paths/digests, input flush, fresh typed confirmation and audited atomic replacement. Runtime startup never silently repins changed binaries.

The native broker implements handoff, approval/list/control/events/grants/kill-switch, session leases, URL refresh and bounded browser fetch streaming. Streams are correlated per connection, return credits only as data is consumed, recheck consent and stop on disconnect/revocation/timeout. Grants decided in one browser profile cannot silently select a different profile. Ambiguous terminal-granted profiles fail closed until a profile is selected. Browser proxy redirects currently fail closed; cross-origin redirect handling needs a separately authorized design.

The nine original MCP tools plus the constrained `zeus_cloud` tool have a working stdio transport, resources and the rescue prompt. Workflow manifests travel as bounded inline content plus a digest; an agent does not open arbitrary daemon-returned paths. The optional MCP HTTP bridge remains unavailable. The CLI supports daemon lifecycle, downloads, live event-driven TUI, terminal decisions and model setup. Its auth menu provides OAuth sign-in, saved ChatGPT account inspection and sign-out; selecting an OAuth account for inference and configuring its model remain API/configuration tasks. Sign-in alone does not enable inference or paid requests. OS-authenticator approval confirmation (`os_auth`) remains unavailable and fails closed.

A2A runs as a separate opt-in loopback process with a dedicated agent credential, owner-scoped tasks and durable task recovery. It implements a bounded file-download profile with submit/get/list/cancel, not every A2A capability; interoperability with external SDKs remains unvalidated.

The workflow runner supports bounded public HTTP pages, CSS/JSON/regex extraction, numeric/cursor/next/date pagination, filtering, naming/routing and deduplication. A completed dry run and human approval bind the exact plan before execution. Durable job intents close the crash-after-enqueue gap, and audit receipts prevent duplicate run-start/end records after interrupted checkpoints; commit guards enforce run byte budgets across concurrent downloads. Unsupported authenticated page/render, sitemap/scroll, conversion, destructive replace and item-sink capabilities are rejected before execution. They are not simulated as successful work.

Model work includes explicitly configured OpenAI Responses/compatible, Anthropic and local adapters; OpenRouter PKCE and ChatGPT sign-in; signed-policy validation/antirollback; OS keyring storage and durable spend reservations. Download-only is the default. No real account login, browser profile, external paid model call or production signing key was used in tests. Host-agent use works through the ordinary MCP workflow; MCP sampling and CLI-delegate provider adapters remain unimplemented.

The model completion wrapper is not yet called by production workflow or classification paths. Current workflow planning samples pages and validates a caller-supplied candidate; it does not generate a plan from a natural-language goal. Workflow creation/execution is available through the daemon API, while dedicated CLI commands and MCP workflow tools remain unimplemented. Existing MCP resources can expose an already-created run's manifest.

### zeus-extension

Cloud delivery adds the terminal-only `cloud_delivery` approval kind. The extension can recognize the shared contract but does not offer a browser decision for it; its UI directs the person to the terminal. Website-session consent never authorizes cloud disclosure.

Protocol schemas/types, approval fact hashing/revision checks, trust UI, native relay integration assumptions and fetch fallback are updated. Chrome/Firefox development and store bundles build; scripted-daemon and protocol tests run. The original cookie-release boundary remains: only current, explicit consent permits memory-only session delivery.

A successful extension unit test or browser bundle does not prove installation/native-messaging compatibility. Real Chrome, Edge and Firefox runs must cover browser suspension, reconnect, expiry/revocation, cookie release, fetch backpressure and interrupted transfers. Browser-parent verification currently recognizes machine-installed Windows browsers registered under HKLM and known root-owned Linux paths. Per-user/portable Windows installations, unsupported Linux packaging and macOS fail closed. Proposed `session.page` / `page_render` capabilities are not implemented or advertised. Store identifiers, screenshots/disclosures, signing and publication still need their actual production inputs.

## Cloud delivery

Cloud delivery now has a [provider library](../crates/zeus-cloud/README.md), durable daemon service, [CLI commands](../crates/zeus-cli/README.md), constrained [MCP tool](../crates/zeus-mcp/README.md), and [spec 14](spec/14-cloud-destinations-and-delivery.md). The service binds each delivery to an immutable plan, source hash, account revision, byte/time limits and independently evidenced human approval. Private checkpoints and idempotency receipts survive restart. Verified staged sources are retained for resumption; streaming uses bounded reads; provider imports remain pending until a remote receipt is verified.

| Destination | Verified staged upload | Bounded streaming | Provider fetch |
|---|---|---|---|
| Google Drive | Native resumable upload | Known length | Unavailable |
| Yandex Disk | Native upload | Known length | Native asynchronous URL import; explicit weaker-limit consent |
| Baidu Netdisk | Native multipart upload, staged block hashing | Known length and precomputed 4 MiB MD5 manifest | Unavailable |
| MEGA and other configured rclone remotes | Backend-dependent | Known-length `rcat`, no implicit local spool | Unavailable |

These adapters have hermetic protocol coverage; live-account acceptance is still required. Native OAuth application setup, browser/device-code onboarding and automatic token refresh are unfinished. Runtime status reports delivery state and provider-confirmed bytes; the preview's finer source/upload progress display is not yet connected. Partial source staging is not a ranged engine download; use an existing completed Zeus job when engine-level source recovery is required. Existing native access credentials are entered through the physical terminal and stored in the OS keyring. Rclone requires an installed trusted executable and an explicit protected configuration. Its generic write API cannot guarantee atomic no-overwrite against a concurrent external writer; cleanup without sufficient object identity/checksum evidence is refused. Yandex imports cannot promise a hard remote byte cap or remote cancellation. Baidu account tier and registered-application limits remain provider-enforced. No provider URL-fetch capability is inferred from an ordinary upload API.

## Product UI

Open [the download workspace prototype](design/console/index.html), [desktop preview](design/console/preview-desktop.png), [agent run preview](design/console/preview-agent-run.png) or [approval preview](design/console/preview-approval.png). The [design rationale and primary sources](design/download-workspace.md) explain the choices.

The prototype home screen is a dense download table with search, filters, saved/total bytes, speed, state, requester and queue. An adjacent inspector gives details without replacing the queue. Link intake previews duplicates, packages, names and destinations before a simulated enqueue. Agent runs expose the plan, files, budgets, approval and exceptions. The cloud panel compares all three data paths, providers, source size and Baidu block-manifest requirements, and makes remote-fetch cancellation/byte-cap limitations visible before its simulated plan. The proposed consent design gives trusted facts their own plain-text review and excludes chat and downloaded content from approval authority. A prominent global stop is separate from ordinary pause or a queue schedule.

These controls currently manipulate **sample state**. The prototype is not a trusted approval surface, a Tauri app or an installed scheduler. Its illustrative bind codes are never sent to the daemon.

## Remaining contract and safety work

Before freezing v1 or shipping an alpha:

- Reconcile the remaining parameter/report details against the normative schemas, including all idempotency-key behaviors, approval batching/amendment/renewal/cooldowns, service credentials/root registration and advanced workflow reports. Do not mistake generated-type drift checks for complete normative conformance.
- Complete file organization, undo and sandboxed conversion with provenance/lineage and destructive approval binding; these methods remain unavailable.
- Review path operations under concurrent parent-directory/symlink/reparse replacement. Existing root checks and final-component protections are not a complete descriptor-relative sandbox on every OS.
- Broaden platform IPC/permissions testing beyond the passing Windows smoke path, and complete installed binary upgrade, browser parent, physical terminal and OS keyring acceptance. Publish supported combinations only after these checks.
- Finish live browser and provider tests with explicit human login; populate production extension identities and policy signing keys through their release process.
- Run the roadmap benchmark set, all crash-injection points, dependency/license/SBOM gates and packaging/signing. No public release or store submission was performed.

The research corpus in `docs/research` remains unverified unless its own metadata says otherwise. UI research in the linked design document was checked against primary product documentation; that does not verify every historical research claim.

## Validation evidence

Validation uses synthetic credentials and loopback HTTP/model/browser peers. The current cloud update passed the app and extension suites below; the core rows distinguish its preceding full runtime baseline from the focused checks rerun for the cloud approval/schema changes. Live browser/provider and Linux/macOS platform results must not be inferred from Windows-hosted hermetic tests.

| Check | Result |
|---|---|
| `zeus-core`: preceding full runtime baseline, `cargo test --workspace --all-features --offline` | 273 passed; one documentation example intentionally ignored. Includes 20 engine integration tests covering resume, validators, corrupt tails, retries, grants, proxy streams and shutdown. |
| `zeus-core`: current changed types/session and schema conformance | 123 passed; generated core schema and 195 normative fixture files synchronized. |
| `zeus`: `cargo test --workspace --all-features --offline` | 400 passed. Includes actual authenticated API downloads, durable approvals/kill switch, workflow checkpoints/audit/pacing, model adapters and IPC shutdown. |
| `zeus`: workspace strict Clippy and formatting; cloud provider strict Clippy | Passed. The preceding core runtime baseline also passed workspace strict Clippy/formatting; current core changes were formatted and conformance-tested. |
| Generated API schema, checked without update mode | Passed. |
| Extension Vitest | 321 passed across 22 files, including terminal-only cloud approval refusal. |
| Extension TypeScript, ESLint/Prettier, Chrome/Firefox development/store builds and repository checks | Passed. Schema copies, generated protocol types, manifest identities and bundled-code checks agree. |
| Normative contracts | 244 documents validated, four partial fragments skipped, 15 spec files scanned; fixtures synchronized. Cloud API schema drift check passes. MCP catalogue: 10 tools, 23,991 bytes, digest checked. |
| Cloud behavior (included in app total) | 25 provider HTTP/subprocess tests; 13 daemon authorization/recovery/cancellation/retention regressions. No live account calls. |
| Interactive UI prototype | Navigation, filters/search, batch actions, safe URL import/deduplication, approval arming, stop/release, budget and schedule simulation checks passed; desktop/mobile screenshots reviewed. |

Rust tests ran with the Windows MSVC Rust 1.99 toolchain. Extension and UI checks ran with Windows Node. Local smoke evidence follows below; these results do not establish Linux/macOS acceptance, real-browser compatibility or a release benchmark.

The permanent [Windows daemon/CLI smoke test](../tools/smoke-daemon.mjs) passed all 14 checkpoints using isolated state and a loopback HTTP server. It verified initialization/pin stability, authenticated named-pipe IPC, empty cloud account/delivery routes, rejection of a competing daemon, actual file bytes and SHA-256, CLI verification, durable idempotency, audit integrity, graceful shutdown and restart.

It also terminated only its own daemon process after **4,194,304 committed bytes of an 8,388,608-byte transfer**, restarted the daemon, confirmed a nonzero resumed range with the original validator and no re-fetch of the committed prefix, then verified the complete file and audit. This proves the tested Windows crash/restart path; it does not replace the broader fault-injection matrix or real-browser acceptance. See [running the smoke test](../tools/README.md). No real browser session, external provider account or user daemon was used.

The implementation is being submitted as coordinated draft pull requests in the separate repositories. Dependency/license/advisory checks with `cargo deny` remain pending because the tool is not installed in this environment. No store uploads or releases were performed.
