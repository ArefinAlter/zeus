# Zeus project handoff

## 2026-10-31 — packaged desktop verified; final presentation and license correction

Windows candidate r6 was built from Zeus `c0cb1f9`, Core `74ecfcb`, Extension `cf17904`, and TUI `0a7e3b5`. All asset hashes, manifest source identities, portable payload entries and runtime versions pass. Actual packaged Tauri/WebView2 acceptance passes: the Add URL UI saves a MIME-named PDF with independently verified bytes; redirects, Content-Disposition names and rejected ranges complete correctly; the packaged TUI shares the desktop profile and displays the real digest; Claude is discovered using its existing installation without API keys or implicit consent; and the app exits cleanly with no page exceptions. Evidence: `D:\verne-build\audit-zeus-package\windows-r6-r3`. Earlier fixture runs exposed two test assumptions (Windows extended paths and installed-provider enabled state), not application failures; those records are preserved.

Linux r5 passes frontend, core, root, native, secondary and real-daemon acceptance at those same four revisions. Retained evidence is `/opt/verne-builds/zeus-alpha2-20261031-r5/logs`; no GitHub Actions or production-service changes were used.

Visual inspection found stale API-key-only text in the Agent panel, despite the working subscription connector. The panel and missing-provider guidance now describe installed-agent sign-in and API providers accurately, and distinguish reviewed desktop actions from the external read-only MCP boundary. Installer resource manifests now include the project LICENSE and NOTICE, already present in portable/CLI archives. These presentation/packaging changes require fresh final artifacts; r6 remains an unpublished verification candidate. NSIS install/upgrade and final Linux packaged-artifact acceptance are still required. Live Claude inference is not claimed while the user's subscription quota is exhausted. Public downloads stay on the existing public personal repository because the organization mirror is private.

## 2026-10-31 — browser rescue passes both approval surfaces

**Zeus remains an unpublished alpha.2 repair candidate.** The final real Edge checks now pass both terminal approval (r18) and the checked-in popup acceptance (r19). Each waits with zero Zeus bytes before consent, refuses the app's decision route, then saves the complete 8 MiB file with the original filename and an independently computed SHA-256 (`7ba7e27fa4c8dce1d309310d2204a3b7779311089f63e6bc1f496689a567a397`). The development extension ID works after explicit installation consent and host pinning; earlier claims that development IDs inherently cannot approve were incorrect. The same-OS-user automation limitation remains documented in ADR 0007.

Repairs cover missing-key panics in ordinary handoff grants, missing negotiated control/event capabilities, Windows console access, and Unicode review output. Windows checks pass: **714 root tests, 226 native desktop tests (4 ignored), 319 extension tests, 70 frontend tests**, strict root/native Clippy and Rust 1.90. Final frontend production build and packaging evidence are separate gates.

The empty HTTP 204 observed in r11 was a workstation capture conflict: the fixture sent 200 and Node received 200, while Edge saw 204. Applying the user-approved **127.0.0.1-only** exclusion through IDM's own UI let Edge receive the file. Registry-only edits did not update the running program. Every applied exclusion was restored and verified; no other site's capture setting or active download was changed. Final browser tests ran under that temporary exclusion, so simultaneous IDM capture is not a supported acceptance claim. Choose a single capture handler for sites used with Zeus browser rescue. Fixed desktop error messages now suggest checking competing capture.

The fixture also needed correction: current Edge automatically recovered its original broken transfer, so the fixture now holds retries at an empty response until origin recovery is explicitly released while Zeus still awaits consent. It never mocks browser decisions or daemon grants. Browser downloads now use their actual CDP context and D: profile preferences. One earlier 8 MiB fixture file landed in C:\Users\AREFIN\Downloads; it was removed only after matching the disposable profile's URL, creation interval, size and full SHA-256. The latest sampled C: free space is **19.00 GiB**.

Evidence: `D:\verne-build\security-2026-10-30\zeus-alpha2-browser-terminal-r18.log` and `...r19.log`; `zeus-idm-localhost-r18.json` and `...r19.json` record exact restoration. r19 is the popup test despite the historical log filename. r17's old final log label incorrectly mentioned the terminal; its structured decision mode was popup, and r19 supersedes it. GUI evidence: `D:\verne-build\audit-zeus-desktop\evidence\s08-terminal-20261031-r18` and `...r19`.

Next gates: commit the reviewed repairs, refresh secret scans, rebuild and validate Windows/Linux packages from the final clean revisions, reconcile required manual checks and protected PRs, then publish verified assets and site metadata. No replacement release, merge, GitHub Actions job or VPS hardening change has occurred. Status stays on the workstation at `A:\ogun\audit\2026-10-30\remediation\STATUS.md`; central handoffs remain under `A:\verne-handoff\2026-10-26`. Earlier checkpoints below are historical.

## 2026-10-31 — browser response discrepancy under investigation

Windows verification is complete for the current uncommitted handoff repairs: **714 root tests**, **226 native desktop tests** (4 ignored), strict root/native Clippy and Rust 1.90 checks pass. The native harness embeds the rebuilt extension; all **319 extension tests** pass. The latest C: guard reading was **19.23 GiB**. These are source checks, not replacement installer validation.

Real Edge r9–r11 now negotiates control/event capabilities, accepts the once-only terminal decision, preserves `from-the-browser.bin`, and reaches the fixture without a daemon panic. Completion still fails: the fixture records HTTP 200, a separate Node request sees 200/8 MiB, but the browser sees an empty HTTP 204. Internet Download Manager is running; interference is a hypothesis pending a controlled comparison. The user approved temporarily excluding **only 127.0.0.1** from IDM capture and restoring the original setting. No other capture settings or running downloads are to be changed. The recovery backup is Windows-DPAPI protected; proof omits existing exclusion sites.

No new release, merge, push, GitHub Actions job or VPS hardening change accompanies this checkpoint. Repairs remain in `D:\verne-build\security-2026-10-30`; browser evidence is in `D:\verne-build\audit-zeus-desktop\evidence\s08-terminal-20261031-r11`. This workstation status is `A:\ogun\audit\2026-10-30\remediation\STATUS.md`. The remaining browser completion gate is real and is not waived.

## 2026-10-31 — browser handoff repairs and validation in progress

The Windows console-access fix is committed locally as `5d25a8d`. Subsequent repairs are still uncommitted in the isolated D: worktrees; remote Zeus drafts remain at `af08878`, core at `cf2c931`, extension at `c96e06e`. No replacement release or installer is published.

- Real Edge r6 reproduced a worker panic in `crates/zeus-daemon/src/browser.rs`: ordinary handoff approvals lack workflow-only fact keys. Optional facts now use checked lookups, and incomplete workflow profile bindings fail closed. The corrected broker regression reproduces the panic before the patch (`zeus-handoff-before-r2.log`) and passes afterward. The earlier before/after logs contain an invalid test fixture and are not evidence of that regression.
- Edge r7/r8 confirmed that the panic is gone but the browser fetch still failed. The extension omitted `approvals`, `events`, `job_control` and `grants` from hello, so the daemon's capability intersection removed the event stream and terminal grants never reached the browser consent gate. The extension now advertises those existing capabilities. Its fake daemon now uses the real intersection rule; a regression proves only a displayed, human-approved, job-bound grant can authorize the browser fetch. All 319 extension tests, typecheck, lint, builds and package checks pass.
- Earlier popup refusal checks encountered the missing-capability error; the previous claim that development IDs inherently cannot approve was incorrect. The host explicitly allows a development ID only after installation consent and pins it; the daemon still checks the host executable, browser parent, matching extension identity and gesture evidence. ADR 0007 allows that trust surface. The rebuilt acceptance separately verifies real popup and terminal decisions, with exact file bytes. The repository browser scenario now fails if completion is missing instead of treating this capability failure as a passing limit.
- Windows review text used raw UTF-8 through CONOUT$ and displayed mojibake under an OEM code page. A bounded UTF-16 console writer now serves CLI approvals and offline executable review. A real isolated CP437 console reads back `Café …` correctly; ordinary files and detached processes remain refused. No user console code page is changed.
- All 714 Windows root tests (37 suites), strict Clippy, formatting and Rust 1.90 pass; runtime binaries are rebuilt. Native desktop checks/rebuild are running to embed the corrected extension. Full real-browser transfer acceptance remains pending. C: free space at the last guard check was 19.70 GiB.
- The normative hello example and its one core fixture are corrected; the producer regenerated 214 files with only that example changed. Specification validation passes: 262 documents, 15 spec files. Core runtime and dependency bytes are unchanged.

Evidence is under `D:\verne-build\security-2026-10-30`; real Edge profiles/reports are under `D:\verne-build\audit-zeus-desktop\evidence\s08-terminal-20261031-r*`. Workstation-visible status remains `A:\ogun\audit\2026-10-30\remediation\STATUS.md`. VPS hardening stays staged, GitHub Actions stays unused, and old package attempts are superseded.

## Windows controlling-console repair — 31 October 2026

Final Edge handoff acceptance exposed a Windows release blocker: `zeus approvals approve` refused a real console with `api.human_required`. The CLI and daemon's offline trust-review path opened CONOUT$ write-only, while Rust's `IsTerminal` uses [GetConsoleMode, which requires GENERIC_READ](https://learn.microsoft.com/en-us/windows/console/getconsolemode). Both paths now request read access only on Windows; terminal detection is retained.

A native child-console regression fails before repair. Shared test support now checks both a newly allocated console with redirected standard IO and a detached child: the former must be accepted and the latter refused. Both regressions pass, and the complete Windows root suite passes 709 tests, strict Clippy, formatting and Rust 1.90. The runtime is rebuilt. Real Edge terminal completion and final packages remain pending at this checkpoint. The older af08878 package candidates were not published and must be superseded; sidecars must be rebuilt for the new source revision.


## Validation update — October 31, alpha.2

Baseline Windows tests pass (707 application, 226 native, 339 core), along with MSRV, native Clippy and the final 12-case file acceptance rerun. Linux application/native/GTK macro/MSRV checks and HTTP/TUI/directory/maintenance/review acceptance pass in the isolated Ubuntu 22.04 builder. Full root/native/core cargo-deny policy checks pass without new exceptions.

Core `cf2c931` additionally fixes real TLS failures previously mislabeled as connection refusals: the real desktop now reports tls.cert_invalid, stops without retries and does not bypass verification. The owner's approved bounded Libgen/CDN probe returns one payload byte with a .pdf Content-Disposition filename and application/octet-stream. A matching 307 cross-origin/range fixture completes with exact bytes through the desktop. No signed query is saved, and full remote file integrity is not claimed.

The rebuilt TUI integration, real FFmpeg media workflow and agent approval/replay tests pass. Media selector names were corrected after the mux GUI regression failed. Real Windows console connection, download rendering, actual SHA-256 display and fixture cleanup now pass. The Hash field previously displayed a path; the regression failed before its correction. Native pinned-FFmpeg decode/cap/pin-change acceptance also passes. Latest TLS/UI/TUI changes still require final Windows/legacy checks, Linux verification and packaged install acceptance. The all-target Rust 1.90 compiler crashed with STATUS_ACCESS_VIOLATION; the unchanged gate is being retried without incremental compilation, with the failed evidence retained. Release workspaces now share an absolute Cargo target; Tauri installer tools use its project-local cache to avoid C: growth. Browser terminal-completion acceptance remains open. Source has not been pushed, merged or released; Actions are disabled for the six Zeus/core/extension personal/org repositories and protections remain unchanged. See RELEASE-REPAIR-alpha2.md and the A-drive status. Earlier sections below are historical.

## Earlier release checkpoint — October 31, 2026

Zeus is being prepared as **v0.1.0-alpha.2**; it is not published or installed-release validated yet. GitHub Actions remain prohibited. All source changes are in the D: repair worktrees, with original A:/B: worktrees preserved. No new merge or source push has occurred.

- Core: Windows **335 tests** and complete Rust **1.90** compatibility check pass. Isolated Linux **340 tests / 28 suites** pass (two ignored). Linux source is clean commit `b4b66da`; archive SHA-256 `ccc88057c9da21509025107ebdb4696e58656adbca9d92fb389c69ec719402fd`. Evidence is copied locally to `D:\verne-build\security-2026-10-30\zeus-core-linux-evidence`.
- Agent: installed-CLI discovery, preview and execution now share a registry, with first-use consent. **74 agent + 14 provider tests** pass (one live opt-in and one subprocess fixture ignored by the default runner); full GUI scenario s10 passes **45 checks**. Exact-action review/reload/replay scenario s30 passes. Claude subscription sign-in is valid but its weekly quota remains exhausted; successful live Claude inference is unverified.
- File features: directory, archive, retained-read/index, workflow, connected-source discovery, organizer, team-policy, inbox-rule and storage-accounting scenarios pass. These are bounded local fixtures, not claims that every external provider/account works. Maintenance s29 now passes between separate stores: Windows path rewriting had invalidated hashed plans; the protocol preserves exact plan bytes. Self-import remains correctly refused; metadata import preserves digests without transferring content. Pending repair still requires human approval.
- TUI: real Windows desktop Settings > Backend launch attaches to the same engine/profile; a console probe reads `LOCAL ENGINE CONNECTED` and the downloaded PDF, with independent hash verification. Evidence: `zeus-s31-terminal-e.log`. This is separate from the human approval terminal.
- Extension: **318 tests**, lint/typecheck and Chrome/Firefox builds pass; dependency audit reports zero known advisories. Browser downloads are preserved until explicit handoff. Real Edge reaches pending approval without cookie access; development extension IDs cannot approve. Completion after terminal approval remains a gate.
- Portable acceptance tests now declare their own Playwright dependency and archive fixtures. Harness shutdown waits for verified clean termination instead of silently killing the daemon. A final regression run and full app/workspace/MSRV checks remain before packaging.
- Linux builds use dedicated unprivileged, resource-limited containers on the VPS; tests have no external network, sources/rootfs are read-only, and no production secrets or Docker socket are mounted. Core build completed; an Ubuntu 22.04 desktop builder is being prepared. No Jenkins/host account/sudo grant was added. Remote scratch: `/opt/verne-builds`; local scripts: `A:\ogun\audit\2026-10-30\remediation`.
- Zeus demo is retired (HTTPS 410). Other demo retirement is queued app by app; Dynodoc's signed-in workspace is preserved. Earlier SSH/HSTS changes remain staged. Remaining release gates include Linux app validation, packaged install checks, checksums/provenance, protected-branch manual verification, source synchronization and publication. FTP/BITS and universal interception are not implemented support claims.

Logs: `D:\verne-build\security-2026-10-30\zeus-s10-final-review.log`, `zeus-agent-discovery-tests.log`, `zeus-s29-artifact-maintenance-20261031-e.log`, `zeus-s31-terminal-e.log`. GUI evidence: `D:\verne-build\audit-zeus-desktop\evidence`. Previous checkpoints below are historical, including older test counts and release-complete claims.

## October 31 — additional repair evidence

Core local commit `b4b66da` passes 335 tests and full Rust 1.90 checking. Extension local commits `275e2b0`/`dd5ccff` fix vulnerable dependencies and premature cancellation/erasure of browser downloads; 318 tests/lint and real Edge rescue pass. Browser decisions remain refused for the development extension ID; terminal approval is required.

Desktop agent writes now wait for a single-use, expiring review of fixed normalized arguments. Artifact-maintenance and encoded-batch policy gaps are closed. The GUI/daemon test covers denial, cancellation, wrong IDs, argument replacement, replay, immediate reload and exactly one hash-verified download. Save ordering and hydration races are fixed. Native review tests: 72+14; frontend: 68. The subsequent installed-CLI registry/first-use-consent fix is still being verified.

No replacement release or source push has occurred. Logs and screenshots: `D:\verne-build\security-2026-10-30` and `D:\verne-build\audit-zeus-desktop\evidence`; authoritative status: `A:\ogun\audit\2026-10-30\remediation\STATUS.md`. Earlier checkpoints below are historical.


## Release repair checkpoint — 2026-10-31

The material below is a historical October 21 snapshot. The owner reports broken `v0.1.0-alpha.1` desktop downloads and agent usage; the current repair work is on `security/audit-2026-10-30`, not yet a published replacement release. Central live handoff: `A:\verne-handoff\2026-10-26\STATE-zeus.md`; cross-product status: `A:\ogun\audit\2026-10-30\remediation\STATUS.md`.

Implemented locally: response-MIME/redirect filename preservation, initial 400/416 range-probe fallback, actual destination names in job summaries, actionable daemon failures, measured progress, safe subscription-limit reporting, native folder-tree pagination independent of search snapshots, and stable counts on background refresh. Native desktop-to-daemon-to-disk, full-size transfer, crash/resume and file-manager GUI acceptance pass. The file-index suite passes 71 tests; desktop/agent/provider suites previously passed 111. Exact user Libgen/PDF URLs are still pending; successful live Claude inference is blocked by the account's weekly limit until November 1 at 05:00 Asia/Dhaka.

Zeus's public sslip demo was retired with an atomic Nginx update; it returns 410. Its data volumes/images are retained. The separate team service and signed-in Dynodoc workspace are preserved. Source removes public-demo selection/links; site source builds but has not been republished. No unrelated VPS hardening was applied.

Do not use GitHub Actions. No new release/merge/mirror push has occurred. Remaining gates include full core/app/MSRV/Linux validation, browser-extension acceptance, real Go TUI integration, runtime/agent feature reconciliation, manual protected-branch checks, release packaging and asset/site reconciliation. Do not claim FTP/BITS, full TUI parity, or complete security closure before implementation and acceptance evidence exists.


Updated **2026-10-21** after coordinated integration into `main`. Historical snapshot reviewed **2026-10-18**. Runtime/test baseline recorded **2026-10-16** and rerun **2026-10-18** after the continuation work. Audience: the maintainer and developers taking over implementation, integration and release work across the four Zeus repositories.

Zeus has an executable pre-alpha download engine, persistent daemon, CLI/TUI, MCP stdio, browser relay, public-page workflow runtime, model adapters and cloud delivery. It is **not a released product**. Installed browser/provider acceptance, remaining contract/security gaps, sandboxed conversion, model file classification, scheduler integration and production desktop acceptance remain unfinished. Organization/undo, artifact lifecycle RPC/CLI and a restricted real-state desktop shell execute. Start with the [end-to-end roadmap](../ROADMAP.md), then use the commands and discrepancy register below.

## Current main integration

The user authorized the coordinated push and merge. All four implementation/preview PRs are merged; the local checkouts are on `main`. The table records the implementation merges; the subsequent CI-access and TLS dependency updates are recorded in the [CI guide](CI_ACCESS.md).

| Repository | Tested implementation head | Main integration merge |
|---|---|---|
| App | `b017f5a52b7f9638f1045cbe66d63786efe02787` | [`e3fa7d1`](https://github.com/ArefinAlter/zeus/commit/e3fa7d1d989903a46525c655ac4c51575f64d29e), [PR 3](https://github.com/ArefinAlter/zeus/pull/3) |
| Core | `9b9cd8142b27b8c04151b0ebc854ad4649b1315a` | [`a94655c`](https://github.com/ArefinAlter/zeus-core/commit/a94655cdef51e452f6f582fb4be6193ec4a1db1c), [PR 1](https://github.com/ArefinAlter/zeus-core/pull/1) |
| Extension | `8d6dd0d6164696d614abb323e89a2195df9da2e2` | [`60db4ac`](https://github.com/ArefinAlter/zeus-extension/commit/60db4ac3c15aa23ef7b62b1cfd8abec1ff4cf8c7), [PR 1](https://github.com/ArefinAlter/zeus-extension/pull/1) |
| Site | `74bdcca5e21bc6ff11b05ea2821c9bdbb8b5f750` | [`b5f7176`](https://github.com/ArefinAlter/zeus-site/commit/b5f7176dc2e367c28c674b9145845ea8e5f8fb55), [PR 1](https://github.com/ArefinAlter/zeus-site/pull/1) |

Final validation: **496 app tests** (126 daemon, 52 workflow), **287 core tests** (one documentation example ignored), **321 extension tests**, five desktop Rust tests and four frontend model tests pass. Strict Clippy and formatting pass in all three Rust workspaces. Rust **1.90.0** source checks pass for core, app and desktop. The bundled Windows desktop executable builds; all 256 normative examples and the generated-schema/catalogue/fixture checks pass. The extension passes typecheck/lint/invariants and all Chrome/Firefox development/store builds; the site builds 25 pages. Tests use Windows x86-64 MSVC Rust 1.99.0 from WSL, not native Linux/macOS/ARM/mobile acceptance.

At integration, app CI lacked its private-core credential and GitHub refused to start core/extension jobs because of account payment/spending-limit status. No required checks or branch protections were disabled; the merge used recorded local validation. The user-approved followup configures [read-only deploy-key access](CI_ACCESS.md) through `ZEUS_CORE_SSH_KEY`, verified against private core over SSH and hosted checkouts. Its initial hosted run passes app tests on Windows x64, Linux x64/ARM64 and Intel/Apple Silicon macOS, MSRV 1.90, contracts and Windows/macOS desktop builds/tests. Windows ARM checkout ACLs and a newly published TLS advisory required the followup fixes documented in the guide. Account billing remains unresolved for core/extension. Native GUI/browser/provider, device/mobile, signing/store and rescue-corpus acceptance remain open.

The app now pins compatible core `6c622a18d6822222575902dc3e16b24be6d95ebc`, which adds the TLS lockfile patch after the original functional core `6ec728e10aec14ecd10b088b7d077da3d35feada`. Core/extension CI pin app `f4da89dee3b0485e49f0f2b112143315048be321`, whose normative specs match these merges; subsequent receipt/recovery corrections changed private app runtime behavior without changing copied contracts.

## Repository and review snapshot

The following table is the historical October 18 review snapshot, not the current main/publication record. Later documentation-only commits can advance the PR head without changing this baseline. All four PRs were **open drafts against main** at that October 18 check; no merge was recorded in that snapshot. All source worktrees were clean before this documentation update.

| Repository and original directory | Branch | Implementation revision | Draft PR |
| --- | --- | --- | --- |
| `zeus` — `A:\zeus` / `/mnt/a/zeus` | `codex/prealpha-runtime-20261016` | `762fced6b68dae70ee2f7de208e4ee7bce26d5aa` | [zeus PR 3](https://github.com/ArefinAlter/zeus/pull/3) |
| `zeus-core` — `A:\zeus-core` / `/mnt/a/zeus-core` | `codex/prealpha-engine-20261016` | `ab77d6d20c92c56278fb78b3b755724d0057ecd1` | [core PR 1](https://github.com/ArefinAlter/zeus-core/pull/1) |
| `zeus-extension` — `A:\zeus-extension` / `/mnt/a/zeus-extension` | `codex/browser-contracts-20261016` | `b0491c2b7027c91102294dfba273ce73816d3928` | [extension PR 1](https://github.com/ArefinAlter/zeus-extension/pull/1) |
| `zeus-site` — `A:\zeus-site` / `/mnt/a/zeus-site` | `codex/download-workspace-preview-20261016` | `74bdcca5e21bc6ff11b05ea2821c9bdbb8b5f750` | [site PR 1](https://github.com/ArefinAlter/zeus-site/pull/1) |

The app depends on nine `zeus-core` crates through sibling paths. Do not combine app changes with an old core checkout. The extension has generated/copied contracts from the app specs. Keep exact companion revisions in PR descriptions and release records. That statement described the October 18 draft branches. Coordinated integration has since advanced; use the final companion revisions recorded after the current validation/publish pass rather than reproducing this historical table as current state.

GitHub showed successful CodeRabbit statuses on all four PRs and successful Vercel preview checks on the site PR. Those statuses **do not establish Rust or browser test CI**. No tracked GitHub Actions workflows were found in the app/core baseline despite older prose claiming CI coverage. Required test/MSRV/security CI is an open task.

All product source is in these original repositories. `.implementation/` is ignored local build/review/log material, not a fifth product repository or an installable source tree. Historical temporary copies there must not become a source of truth. No release/store submission was performed as part of the implementation work; a Vercel preview check is not production release acceptance.

### Continuation of 2026-10-18 (uncommitted)

The continuation work in the [implementation ledger](IMPLEMENTATION_STATUS.md#2026-10-18-continuation) sits as uncommitted changes on the branches above; nothing was committed or pushed. It touches three repositories, which must land together:

- `zeus`: daemon, API, CLI, MCP, workflow crates, specs 01/05/07/09/10/11/12/14, the normative daemon API schema, ADR 0012, `deny.toml`, `tools/bench-daemon.mjs` and `.github/workflows/ci.yml`.
- `zeus-core`: `ApprovalResolution.job_id` (engine), two registered policy codes (`zeus-types`), the `zeus-control` algorithms, resynchronized `spec-fixtures/` and `.github/workflows/ci.yml`.
- `zeus-extension`: resynchronized `schema/` copies and `.github/workflows/ci.yml`.

The original token setup is superseded by the configured [read-only SSH deploy key](CI_ACCESS.md); the current workflow does not use `ZEUS_CORE_TOKEN`. Resolve account billing, confirm green runs, then mark the Linux, Windows, MSRV, contracts and deny jobs as required checks. The core and extension workflows read the public `zeus` repository and need no private-core credential.

### Continuation of 2026-10-21 (uncommitted)

The app working tree additionally contains the three grouped MCP tools, generated embedded catalogue, durable workflow retry receipts, corrected manifest resource parsing, artifact CLI, URL-list intake, live TUI search/selection fixes, ADR 0013, the [workflow/platform design](design/client-workflows-and-platforms.md), and expanded informational CPU/OS CI. Earlier edits were preserved; no commit, push or release occurred. No sibling repository was modified in this continuation. Spec consumers must resynchronize their fixtures before coordinated integration.

The MCP catalogue is now generated with `node docs/spec/tools/mcp-catalogue.mjs --write`; `--check` compares the file embedded in Rust and verifies the stated digest/budgets. Use `zeus artifacts --help` and `zeus get --help` for the new commands. `/` searches the live TUI snapshot; it does not search the filesystem. See the [current implementation ledger](IMPLEMENTATION_STATUS.md#2026-10-21-continuation) for verification and remaining gaps. The October 18 revision/PR table above is historical, not a new GitHub check.

### Parallel continuation of 2026-10-21 (merged)

The coordinated pass adds handoff batching and atomic cancellation amendment/withdrawal in app/core, a two-permission native-host ceiling with legacy migration, artifact lifecycle preview/apply RPC and CLI, versioned workflow reports, and the [restricted Tauri client](../apps/desktop/README.md). Matching core/extension contracts accompany the app; the earlier same-day statement about no sibling changes applies only to that earlier pass.

The desktop uses a dedicated `gui.token` (Agent/Gui, seven explicit permissions), real daemon snapshots and five typed bridge commands. It previews and enqueues retained URL batches, controls eligible selected jobs, and reads run/file/artifact facts. It cannot decide approvals or borrow the CLI/host token. Artifact deletion is an approved record tombstone; retention weakening needs terminal approval, and materialization is a verified new copy. CLI callers save the exact JSON preview and retry `artifacts apply` with the same key after human approval.

Final local evidence: **496 app tests, 287 core tests and 321 extension tests pass**. Strict app/core/desktop Clippy, formatting, the bundled Windows desktop executable build and source check, five headless Rust bridge tests, four Node UI tests, generated schema/fixture checks and all 256 normative examples pass. The extension also passes typecheck/lint/invariant checks and all four browser build variants. Website build passes for 25 pages. Full details are in the [current ledger](IMPLEMENTATION_STATUS.md#2026-10-21-parallel-continuation).

Hosted CI was blocked independently of local code validation by the missing app credential and core/extension account payment/spending-limit status. Read-only private-core access is now configured and verified in the [CI-access followup](CI_ACCESS.md); billing remains a maintainer action. No native GUI launch, mobile/device, live browser/provider, signing/store or rescue-corpus acceptance is inferred.

## Architecture and ownership

```mermaid
flowchart LR
  B[Browser extension] <-->|Native messaging| N[Native host]
  N <-->|Authenticated local IPC| D[zeusd]
  C[CLI and TUI] --> D
  M[MCP stdio] --> D
  A[Optional A2A bridge] --> D
  G[Restricted Tauri client] --> D
  D --> E[zeus-core engine]
  D --> W[Workflow and model services]
  D --> CL[Cloud orchestration and adapters]
  E --> J[Journal, audit and artifacts]
  CL --> P[Configured cloud providers]
  S[Marketing site and simulated preview]
```

The daemon has one local IPC listener; HTTP bridges are separate optional processes. Browser sessions stay inside the extension-mediated daemon path and are memory-only. The A2A bridge has a separate durable peer/task index; the daemon is not literally the only process with any durable state.

| Location | Responsibility and current implementation |
| --- | --- |
| `zeus-core/crates/zeus-types` and `zeus-session` | Domain/state/error/redaction contracts, approval facts, session framing/negotiation/correlation and consent abstractions. |
| `zeus-core/crates/zeus-control` and `zeus-http` | Transfer helpers and HTTP execution/parsers. Advanced control algorithms and full scheduling integration remain partial. |
| `zeus-core/crates/zeus-fs`, `zeus-journal`, `zeus-verify` | File operations, redb commits/recovery, file hashes, audit and provenance. |
| `zeus-core/crates/zeus-engine` and `zeus-artifact` | Executable download lifecycle and content-addressed/record-only artifact library. Product policy remains in the embedding app. |
| [zeus-api](../crates/zeus-api) | Wire types, permissions, authentication, client/server transport and generated API schema. A policy/type declaration is not proof a handler exists. |
| [zeus-daemon](../crates/zeus-daemon) | IPC ownership, engine adapter, durable approval/grant/stop state, browser broker, workflows, models, cloud orchestration and receipt stores. |
| [zeus-cloud](../crates/zeus-cloud/README.md) | Explicit provider capabilities, native Drive/Yandex/Baidu, configured rclone, private checkpoints and credential-store port. |
| [zeus-cli](../crates/zeus-cli) and [zeus-mcp](../crates/zeus-mcp/README.md) | Human CLI/TUI/auth/cloud review and constrained agent tools/resources. MCP has no decision/account-configuration authority. |
| [zeus-native-host](../crates/zeus-native-host) | Native framing, browser-parent verification, registration and bidirectional relay; only `daemon.read`/`nm.relay`, with popup decisions authorized by native channel evidence. |
| [desktop client](../apps/desktop/README.md) | Independent Tauri workspace, restricted Agent/Gui IPC bridge, real paged downloads, link review/enqueue and run/file/artifact views; no GUI decision authority. |
| [zeus-workflow](../crates/zeus-workflow) and [zeus-llm](../crates/zeus-llm) | Plan validation/extraction/runtime and model/provider/privacy/spend mechanisms; daemon owns persistence/policy. |
| [zeus-a2a](../crates/zeus-a2a) and [zeus-a2a-bridge](../crates/zeus-a2a-bridge/README.md) | Task/card mapping and opt-in loopback four-method download profile. |
| `zeus-extension` | Browser consent, rescue, session release, fetch fallback, popup/options and generated protocol contracts. |
| `zeus-site` | Static marketing/docs site and disclosed sample-only preview; no daemon connection. |

Ogun's architecture embeds `zeus-core` while keeping its own product state, policy and interfaces. Its intended browser-download path is through Zeus's service boundary; service-client provisioning/integration remains unfinished. Review shared API changes with both consumers; do not move product configuration or credentials into core. [ADR 0011](adr/0011-ogun-built-on-zeus-core.md) defines this boundary.

## What works and what is still partial

| End-to-end path | Current result | Boundary still open |
| --- | --- | --- |
| Public URL → local file | Daemon/CLI and API execute download, verification, provenance and recovery. Windows actual-process smoke passed. | Broad platform/origin/fault acceptance and advanced scheduling. |
| Browser failure → rescued file | Extension, native host and daemon/core pieces execute under fixtures. | No installed Chrome/Edge/Firefox end-to-end acceptance recorded. Refresh/redirect integration is incomplete. |
| Agent → daemon | Thirteen MCP tools over stdio, including workflows/files/artifacts; API events/wait. | Host compatibility, remaining resource/pagination details and optional MCP HTTP. |
| Workflow → files | Caller-supplied plans can be sampled, validated, approved and run through the daemon API with budgets/checkpoints; since 2026-10-18 a terminal user can ask for model-drafted selectors, and `zeus workflow` drives runs. | Authenticated rendering, remaining strategies and revised-plan continuation. Versioned reports and grouped MCP workflow tools exist. |
| Model configuration/auth | Provider adapters, auth flows, keyring port, policy verification and spend ledger exist; workflow drafting is the first production caller (2026-10-18). | Further callers such as file classification, actual account/keyring validation and production policy distribution. |
| Cloud destination | Staged/streaming/native URL-import modes with durable approvals/reconciliation and provider fixtures. | Native cloud OAuth/refresh, live providers, richer recovery/progress and archival. |
| Files → organization/conversion | The organiser and undo use terminal-decided approvals. Artifact reads/lineage/ingest and exact preview/apply tombstones, retention and verified copy materialization execute through RPC/CLI. | Sandboxed conversion, model classification, artifact import/export/events and full platform containment acceptance. |
| Human desktop workspace | Live Rust TUI and restricted Tauri client connected to real daemon state; separate HTML/site simulations remain labeled. | Installed GUI/accessibility acceptance, event replay, scheduler, persistent inbox/packages, index and trusted GUI decisions. |
| Other agents → Zeus | Authenticated local A2A submit/get/list/cancel with restart recovery. | External SDK conformance and any additional/remote profile. |

This inventory supersedes the old assessment that `EngineBuilder::build()`, daemon startup, MCP stdio and native relay are still stubs. It does not mean every declared interface now works.

## Get a matching development checkout

Clone all four repositories side by side. Core/extension access may require the maintainer's repository permissions.

```text
workspace/
  zeus/
  zeus-core/
  zeus-extension/
  zeus-site/
```

Use the branches/revisions above for reproduction. Before changing branches, inspect and preserve local work. In a fresh clone, fetch the named branch and use `git switch --detach <implementation-revision>` for an exact baseline; create a task branch before editing. `git rev-parse HEAD` should match the table. The site's app-preview content is copied, not a runtime dependency of the Rust workspace.

Requirements: Rust stable with rustfmt/Clippy, declared MSRV **1.90**, Node **22.12+** and npm for extension/spec tooling. The recorded Rust run used **Windows MSVC Rust 1.99**; that is not proof of the minimum-version or macOS build. Since 2026-10-20 the workspace also builds on Linux (the peer pid is a `pid_t` there) and passes fmt, strict Clippy and its tests with Rust 1.98.1. Windows MSVC needs Visual Studio Build Tools and a Windows SDK. [CONTRIBUTING.md](../CONTRIBUTING.md) describes a GNU fallback, which still needs target-specific validation.

Run one Cargo process at a time on constrained machines; use `CARGO_BUILD_JOBS=3` or another measured limit. Shared build caches are optional, but the Windows smoke harness requires the actual package in `zeus/target/debug`. In WSL, do not mix Linux and Windows executables/build outputs without choosing the target deliberately.

## Build and operate the developer package

From the app repository, in PowerShell on Windows:

```powershell
$env:CARGO_BUILD_JOBS = '3'
cargo build -p zeus-cli -p zeus-daemon -p zeus-native-host --bins --all-features --target-dir target
.\target\debug\zeusd.exe init
.\target\debug\zeusd.exe run
```

Keep the daemon running in that terminal. In a second terminal using the same configuration/state environment:

```powershell
.\target\debug\zeus.exe daemon status
.\target\debug\zeus.exe status --json
.\target\debug\zeus.exe tui
.\target\debug\zeus.exe approvals list
.\target\debug\zeus.exe audit verify
```

On Unix, the corresponding executables are `./target/debug/zeusd` and `./target/debug/zeus`. These commands describe the interface, not completed Unix acceptance. For a deliberate real download, `zeus get URL --wait --sha256 EXPECTED_HASH` uses your reviewed URL/hash. `zeus get -o` takes a destination **directory**, not a filename. Use allowed roots; the default is `<Downloads>/Zeus` with the documented home-relative fallback.

`zeus daemon install` is the CLI provisioning path; it creates state/credentials and pins sibling executables. It **does not install an automatic OS service**. `zeus daemon start` launches a background process. Build the complete sibling package before provisioning, and do not change executable files under a running acceptance test.

For an intentional binary upgrade, stop the daemon, install/build the reviewed sibling package, and run `zeusd trust-clients` from a physical terminal. Review paths/digests and complete its fresh typed confirmation, then restart. Startup does not silently accept changed pins. Persisted-format rollback/migration is not yet a supported release procedure; preserve a consistent backup before testing an upgrade.

### Human authority and global stop

- `zeus approvals approve ap_...` and `deny` require a verified human terminal. Pipes, `--yes`, model output and agent tool calls cannot approve.
- Browser approvals independently bind extension/profile, surface, gesture and current facts. Cloud approvals are terminal-only.
- `zeus daemon kill-switch` persists the stop latch, removes active grant authority and pauses work. `zeus daemon release` restores **neither** revoked grants nor paused work.
- OS-authenticator confirmation is unavailable and fails closed. Same-user malicious software remains a residual trust risk.

### Agent and browser entry points

Run `zeus mcp` as the MCP host's stdio command. Its credential has agent authority. Processes sharing the same agent credential share its ownership/idempotency namespace; this is not isolation between individual assistants.

The optional A2A entry point is `zeus a2a --token-file /absolute/private/a2a-peers.json --port 4750`. Follow the [bridge guide](../crates/zeus-a2a-bridge/README.md) for peer tokens and protocol headers. It initiates real downloads and returns metadata-only artifacts. It is local-only and does not expose the daemon as a general HTTP service.

Use `zeus native-host install --help` and the extension's [identity guide](https://github.com/ArefinAlter/zeus-extension/blob/codex/browser-contracts-20261016/docs/EXTENSION_ID.md) before browser registration. Pin actual development or production extension IDs. Current parent verification recognizes machine-installed Windows browsers under HKLM and known root-owned Linux paths. Per-user/portable Windows, unsupported Linux packaging and macOS fail closed; do not label them supported on the strength of a successful bundle.

## Cloud operation handoff

| Provider | Staged upload | Stream | Provider URL import |
| --- | --- | --- | --- |
| Google Drive | Native resumable | Known exact size | Unavailable |
| Yandex Disk | Native upload | Known exact size | Native, asynchronous, with explicit weaker limits |
| Baidu Netdisk | Native multipart | Exact size and precomputed block manifest | Unavailable |
| MEGA/other configured rclone remotes | Backend-dependent | Known-size backend support | Unavailable |

The [CLI guide](../crates/zeus-cli/README.md) contains full syntax. Operational sequence:

1. A person runs `zeus cloud accounts configure` at a verified terminal. Native access credentials go into the OS keyring with hidden input; there are no secret flags/stdin credential inputs. Existing credentials must be obtained separately until native OAuth onboarding is implemented.
2. `zeus cloud plan` binds the account revision, source, explicit mode, unique destination, exact size, optional hash and time/byte limits. Public URL sources require an exact length; a completed, owned, nonquarantined job supports staged upload only.
3. `zeus cloud submit` uses the returned plan ID/hash and creates a human approval request. It does not approve or immediately authorize the transfer.
4. Review through `zeus approvals`; observe with `zeus cloud list`, `get` or `status`; use `control ... pause|resume|cancel` for the delivery.
5. Preserve the private checkpoint if dispatch/completion is uncertain. Reconcile provider evidence rather than submitting another upload under a fresh key.

Only Yandex currently imports URLs remotely. An accepted/pending provider operation is not completion. Local cancellation/stop cannot guarantee remote termination or a hard remote byte cap. Public source URLs are disclosed to the provider only through the reviewed mode; browser-session cookies are never forwarded to clouds.

Rclone requires a trusted executable and dedicated protected config. Their SHA-256 pins are checked per launch. Legitimate OAuth config rewrites or executable upgrades require human reconfiguration, a new account revision and new reviewed plans. Native credential replacement also changes reviewed configuration. No automatic native token refresh exists.

Verified Zeus-owned staging survives restart without re-fetching an expired source. Drive/Baidu can resume supported staged checkpoints; Yandex/rclone reconcile the existing target and return `cloud.resume_unavailable` when replay is unsupported. Partially downloaded URL staging does not have engine-style ranged recovery. Existing downloaded user files are never deleted by cloud cleanup. `cleanup_pending` preserves remote success while reporting failed local staging removal. Without sufficient remote identity/hash evidence, cleanup is refused rather than risking a different object.

`max_bytes` bounds object size on each local transfer leg, not total traffic/billing. Remote receipts distinguish local SHA-256, actual provider hash and size-only evidence. Status has provider-confirmed bytes; the preview's separate source/upload meters are not wired.

Current cloud capacity limits include 64 accounts, 128 nonterminal deliveries, 128 unexpired plans per requester, 1,024 plans total and 4,096 private records. Serialized state is bounded to 256 MiB with a 64 MiB update reserve and 2 MiB per record; these are not physical redb/disk-size limits. Since 2026-10-18, retention runs automatically (finished deliveries after 30 days, expired unused plans, receipts a week after their delivery finished; uncertain operations are kept). **Do not delete `cloud.redb` to clear a limit or an uncertain operation.** Preserve receipts and implement/use reviewed retention/reconciliation. See the [daemon guide](../crates/zeus-daemon/src/cloud/README.md).

## State and recovery

[ZeusPaths](../crates/zeus-api/src/endpoint.rs) resolves platform paths. Explicit overrides are `ZEUS_STATE_DIR`, `ZEUS_CONFIG_DIR` and `ZEUS_RUNTIME_DIR`; keep them consistent across daemon/clients. `--profile` / `ZEUS_PROFILE` selects separate state, configuration and runtime paths. Windows named pipes remain per-user even with separate state paths.

| State | Purpose and handling |
| --- | --- |
| `config.toml` in the config directory | Write roots and settings. `config.set` persists restart-only settings; `config.get` reports the startup snapshot. Some settings are not wired yet; see below. |
| `credentials.json`, `credentials/`, `ipc.secret`, `trusted-programs.json` | Private credential/verifier material and executable pins. Never include these in public diagnostics or commits. |
| `zeusd.lock`, `zeusd.json` | Process ownership/discovery. Do not delete a live owner's lock/discovery to force another instance. |
| `journal.redb`, `api-receipts.redb` | Download/consent/stop state and durable mutation bindings. Preserve together with related files and audit data. |
| `audit/`, destination manifests, `artifacts/` | Audit chain, per-file provenance and artifact metadata/content. Record-only artifacts do not imply a second copy of a downloaded file. |
| `workflows.redb` | Plans, checkpoints and job/audit intents. Preserve to avoid replaying work as a new run. |
| `models.redb`, `chatgpt.redb` and OS keyring | Model configuration/spend/account metadata and separate secrets. Copying the database alone does not restore keyring credentials. |
| `cloud.redb`, `cloud-staging/` | Private source/provider checkpoints, idempotency and retained staging; protect like credentials. |
| `a2a-index.redb` and explicit peer token file | Bridge task ownership/retry binding and separate peer authentication. Stop the bridge before any offline state operation. |

For developer backup, stop the daemon and bridge, wait for process/lock release, and preserve the consistent state/config plus destination files/manifests with private permissions. Keyring backup/recovery is platform-specific and still requires acceptance. Do not promise a portable backup/migration command that does not exist. Shutdown may retain the lock beyond its requested grace period while synchronization finishes; do not force-close it merely because that period elapsed.

After a crash, restart with the same reviewed binaries, configuration and state, inspect status and verify audit/files. Recovery must validate file identity and origin validators. A changed or missing verified staging file fails closed. Signed URLs/private upload URLs can exist in protected persistent state; local ACLs are not encryption at rest. Spec 03's journal URL encryption remains an open design/implementation item.

## Recorded validation evidence

Rows dated 2026-10-18 were rerun after the continuation work; the others are the **October 16 implementation results**, reviewed for this handoff. Synthetic credentials and loopback fixtures were used; live browser/cloud/model accounts were not exercised.

| Check | Result and scope |
| --- | --- |
| App workspace, all features | **437 passed** on 2026-10-18 (400 on October 16), zero failed. Includes cloud tests below; do not add them again to the total. |
| App workspace on the declared MSRV | `cargo +1.90.0 check --workspace --all-features --locked` passed on 2026-10-18. |
| Cloud provider library | **25 tests**, including native HTTP and real child-process fixtures; no live providers. |
| Daemon cloud service | **14 regressions** on 2026-10-18, including authorization, recovery, cancellation, staging, cleanup and the automatic retention pass. |
| Core full runtime | **284 passed** on 2026-10-18 after the `zeus-control` algorithms, one intentionally ignored documentation example; includes 20 engine integration tests. Strict Clippy and formatting passed. |
| Core cloud-related types/session/conformance | **123 passed**; generated schema and 195 fixture files synchronized. This is the focused rerun, not another full core baseline. |
| Rust style/static checks | App and core all-target/all-feature strict Clippy and formatting passed on 2026-10-18. |
| Extension | **321 passed across 22 files** on 2026-10-18; TypeScript, lint/format, Chrome/Firefox development/store bundles and repository checks passed. |
| Contracts | 244 JSON documents checked, four partial fragments skipped, 15 specs scanned; cloud drift check passed; ten-tool MCP catalogue checked at 23,991 bytes. |
| Windows actual processes | **14 smoke checkpoints passed**, including real download, verification, idempotency, audit, restart and abrupt owned-child recovery. |
| Site and UI | Recorded 25-page site build and 21 browser interaction groups; desktop/390px mobile, no external requests/CSP errors in that run. Preview behavior only. |
| Loopback benchmark | Release build, `tools/bench-daemon.mjs`, 2026-10-18, Ryzen 7 5800X, Windows 11: 512 MiB at 1/4/16 connections **34.1/45.7/39.4 MiB/s** on NVMe, **12.3/10.7/7.0 MiB/s** on a SATA HDD; about 15 MiB working set idle and 26 MiB with ten active jobs. Reports in `target/bench-runs/` (machine-local). |
| Dependency/license/advisory gate | `cargo deny check` **passed** for app and core on 2026-10-18 (cargo-deny 0.20.2) after updating rustls to 0.23.46; two documented ignores in the app `deny.toml`. |

The crash smoke interrupted an 8,388,608-byte file after 4,194,304 committed bytes, resumed from that offset with the original validator, and matched the final SHA-256. It does not prove every power-loss point or filesystem.

Local evidence includes `.implementation/cloud-workspace-tests.log`, `.implementation/cloud-workspace-clippy.log`, `.implementation/cloud-smoke.log`, `target/smoke-runs/daemon-b81f8143-b30e-442e-8802-298d1769f40c/report.json`, and `.implementation/site-preview-9a74d40f-5829-4f18-bafb-05fc77d80371/report.json`. These are ignored machine-local artifacts, **not guaranteed to be present in a fresh clone**. The permanent [smoke harness and guide](../tools/README.md) are committed. Preserve redacted evidence as CI/release artifacts when establishing the release pipeline.

## Reproduce validation

From each Rust workspace, sequentially:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

Install `cargo-deny` before treating the last check as a release gate. The existing check scripts skip it if absent; a skipped gate is not a pass. Recorded builds used `--offline` after dependencies were cached; fresh machines need an initial dependency fetch.

From `zeus`:

```sh
cd docs/spec/tools
npm ci
npm run validate
cd ../../..
cargo test -p zeus-api schema
node docs/spec/tools/sync-cloud-schema.mjs --check
node docs/spec/tools/mcp-catalogue.mjs --check
```

From `zeus-core`:

```sh
cargo run -p zeus-types --example gen_schemas
git diff --exit-code -- schemas
node scripts/sync-spec-fixtures.mjs --check ../zeus
```

Generation commands can change tracked files; inspect drift rather than automatically accepting it. Change normative prose/contracts first, regenerate Rust/spec/core/extension copies, then run validators in normal check mode. `UPDATE_SCHEMAS=1` is for intentional API regeneration, not proving a clean check.

From `zeus-extension`:

```sh
npm ci
npm run typecheck
npm run lint
npm test
npm run build
npm run build:store
npm run check
```

From `zeus-site`, use `node build.mjs`, then `node serve.mjs 4321` for local review. Visit `/preview/` and `/preview/console/`; sample actions reset on reload. Site build success is separate from interactive/accessibility acceptance.

For actual daemon processes, from `zeus` with Windows Node 22+:

```powershell
cargo build -p zeus-cli -p zeus-daemon -p zeus-native-host --bins --all-features --target-dir target
node tools/smoke-daemon.mjs
```

The harness requires all three binaries in `target/debug`, accepts no state/binary overrides, and refuses a competing per-user daemon. It creates isolated synthetic state and kills only its own child for crash testing. Do not rebuild during the run. It does not configure providers, register a browser, approve anything, or test a real keyring/physical terminal.

## Known gaps and documentation discrepancies

Treat these as explicit follow-up work. Normative prose still governs contracts; this register describes differences that need review, not permission to silently redefine behavior.

| Priority | Finding | Source and next action |
| --- | --- | --- |
| Resolved 2026-10-18 | Emergency stop removes authority even if audit append fails; the record is now retained and replayed. | [deferred_audit.rs](../crates/zeus-daemon/src/deferred_audit.rs), [spec 12](spec/12-security-invariants.md). Failure and restart tests added. |
| Before alpha | Parent-directory/symlink/reparse races and platform trust/IPC/keyring acceptance remain. | [security invariants](spec/12-security-invariants.md). Publish supported combinations only after adversarial and installed-platform tests. |
| Before alpha | Real browser rescue, full fault matrix, rescue corpus, performance and cargo-deny gates remain unrecorded. | [roadmap phase 1](../ROADMAP.md#phase-1-working-mvp-and-public-alpha). Unit tests cannot replace these results. |
| Mostly resolved 2026-10-18 | `approvals.max_pending`, `approvals.decisions_per_minute`, `log_filter` and `--profile` now take effect. `scheduler.segment_size_mib` is still not used by the engine; `config.set` refuses it and startup warns. | [config.rs](../crates/zeus-daemon/src/config.rs), [endpoint.rs](../crates/zeus-api/src/endpoint.rs). Real segment planning is an engine change (core `engine.rs`). |
| Contract review | The engine's nominal per-host connection bound uses a shared semaphore; concurrency is static and commit units are submitted individually. Authenticated standalone probe and URL/auth refresh integration remain incomplete. | Core `crates/zeus-engine/src/engine.rs`. Do not present daemon-wide batching, true host fairness or authenticated probes as complete. |
| Resolved 2026-10-18 | The 17 `NotImplemented` functions of the core control modules are implemented and tested (windowed rate, Kalman, ETA band, P-squared quantile and hedge trigger, UCB1, Thompson and sliding-window UCB, reliability formulas, CUSUM). The engine scheduler does not call the advanced selectors yet. | Core `estimate.rs`, `mirror.rs`, `hedge.rs`, `math.rs`, `cusum.rs`. Linux reflink exists; other platforms/acceptance remain partial. |
| Resolved 2026-10-18 | Spec 10 and ARCHITECTURE now describe the running bridge, the app-owned stores and the A2A index. | [spec 10](spec/10-a2a.md), [architecture](ARCHITECTURE.md), [ADR 0012](adr/0012-app-owned-stores-beside-the-journal.md) (Proposed; needs review). |
| Partly resolved 2026-10-18 | Workflow selector drafting now calls the model (human terminal only, structure-only prompt). Classification has no caller yet. | [workflow_draft.rs](../crates/zeus-daemon/src/workflow_draft.rs). |
| Resolved subset 2026-10-21 | Workflow/file/artifact grouped MCP tools and CLI commands exist; ADR 0013 resolves the catalogue budget. Lifecycle RPC/CLI supports tombstones, retention and copy materialization. `files.convert`, lifecycle MCP mutations, import/export/events and GC administration remain open. | [routes](../crates/zeus-daemon/src/routes.rs), [MCP catalogue](../crates/zeus-mcp/src/catalogue.rs), [artifact lifecycle](../crates/zeus-daemon/src/artifact_lifecycle.rs). |
| Resolved subset 2026-10-21 | Enqueue handoff batching and S32 cancellation amendment/last-member/overflow behavior are tested. Native host has only relay/status permissions; legacy credentials keep their identities/tokens. | [consent](../crates/zeus-daemon/src/consent.rs), [policy](../crates/zeus-api/src/policy.rs). Non-enqueue joining, other need disappearance and renewal remain open. |
| Phase 2 | Native cloud OAuth, live provider validation, policy-signing distribution and comprehensive artifact/cloud retention UX remain. | [cloud guide](../crates/zeus-daemon/src/cloud/README.md), [model spec](spec/09-llm-provider.md). Obtain real operational inputs and record acceptance separately from fixtures. |
| Phase 3 | Restricted real-state Tauri client exists. Durable event replay, persistent inbox/packages, scheduling, filesystem index, trusted GUI decisions and MCP App views remain open. Web/site previews still use sample state. | [desktop boundaries](../apps/desktop/README.md), [design](design/client-workflows-and-platforms.md). Installed GUI/platform/accessibility acceptance remains pending. |

Other spec open issues include journal URL encryption, audit anchoring/signatures, per-client visibility/credentials, large-file verification timeouts, requester-scoped grants, revised-plan continuation, artifact events/exports/privacy and cross-product integration. Use [the spec index](spec/README.md) and each document's open issues alongside the roadmap; stale entries require explicit resolution.

## Release inputs and next handoff

The maintainer must assign owners for API/security review, platform QA, provider registrations, signing-key custody and releases. No people or delivery dates are assigned by this document. Required external inputs include production Chrome/Edge identities and allowlists, Firefox publication details, cloud OAuth registrations/test accounts, model policy keys, code-signing/distribution credentials and verified website/contact/legal facts.

Recommended review order: core first, app with matching core, extension with matching app contracts; site preview can be reviewed independently. Merge only after review and relevant checks; do not deploy a preview as a supported product or merge just because a draft has a green bot status. Commits require DCO sign-off; irreversible contract/persistence/trust changes require the repository's ADR process.

The next implementation batch should close contract/CI and urgent safety gaps, reproduce the matched baseline, then run installed browser and alpha acceptance. Cloud/model live acceptance can proceed in parallel with their assigned human-controlled inputs. The roadmap identifies later workflow, GUI and distribution work without treating it as already delivered.

When handing off again, record exact companion SHAs, clean/dirty state, PR/check status, commands/toolchains/results, unexecuted gates, schema migrations, credentials needed by role (never secret values), and the next reviewable work package. Update this file and the [implementation ledger](IMPLEMENTATION_STATUS.md) together when claims change.
