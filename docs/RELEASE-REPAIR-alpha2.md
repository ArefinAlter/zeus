# Zeus alpha.2 repair candidate

Status: local candidate, not a published or installed-release-validated replacement. No GitHub Actions are used. Source revisions and final asset digests will be emitted by `apps/client/scripts/build-release.mjs`; an installer must not be published solely because compilation succeeded.

## Findings and implemented repairs

| Severity | Failure / threat | Repair location | Evidence / remaining gate |
|---|---|---|---|
| HIGH | Extension canceled/erased a browser download before the handoff existed, losing the transfer. | Sibling `zeus-extension/src/background/downloads.ts`, commit `dd5ccff` | Regression failed before repair; 318 extension tests pass. Real Edge reaches pending approval; development IDs cannot approve. Terminal-completion acceptance remains required. |
| HIGH | Desktop agent writes could execute without an exact per-call human decision; encoded arrays bypassed batch checks. | `apps/client/src-tauri/crates/zeus-app-agent/src/{review,runs,tools}.rs`; `apps/client/src/backend/agentNative.ts` | 74 agent + 14 provider tests; s10 45 checks; s30 review/reload/replay acceptance. Server retains immutable normalized arguments, a random one-use review ID and expiry. |
| HIGH | Installed CLI discovery and send used different registries; automatic first-use consent was too permissive. | `apps/client/src-tauri/crates/zeus-app-agent/src/lib.rs`; `shared/local_agent.rs` | Shared registry, explicit consent and preserved saved preferences. No inherited API billing fallback. Installed Claude subscription sign-in succeeds; its quota is exhausted, so successful live inference is unverified. |
| MEDIUM | Rejected initial range probes left otherwise downloadable links unusable; document filenames lost their extension. | Sibling `zeus-core/crates/zeus-http/src/probe.rs`, commit `b4b66da`; desktop `src/backend/live.ts` | Nine real daemon-to-disk HTTP cases and visible 403 failure pass. One ordinary GET retry only for an initial 400/416; no authentication bypass. Exact owner Libgen URL remains unavailable. |
| MEDIUM | Progress/error display hid actual transfer state. | `apps/client/src/model/{segments,downloadErrors}.ts`; `src/views/downloads/DownloadProgressDialog.tsx` | Real aggregate bytes, range support and actionable daemon errors replace simulated worker figures. Large-transfer and crash/resume hashes pass. |
| MEDIUM | Folder navigation depended on an obsolete full-index snapshot; refresh reset match counts. | `apps/client/src-tauri/crates/zeus-app-index/src/{browse,lib}.rs`; `src/views/files/FolderTree.tsx`; `src/views/Files.tsx` | Bounded directory-only pages and stable refresh counts. 71 index tests and file-manager GUI acceptance pass; final regression rerun pending. |
| MEDIUM | Windows path cleanup modified already-hashed maintenance plans, causing valid exports/imports to fail as stale. | `apps/client/src-tauri/crates/zeus-app/src/routes.rs` | Exact protocol values preserved; s29 export/import between distinct stores passes. Self-import still refuses conflicting source identity; metadata import does not transfer object bytes. Ingest UI now accurately states that a verified local content copy is stored. |
| MEDIUM | Undo could fail when an index reader briefly held the folder. | Sibling `zeus-core/crates/zeus-fs/src/directory.rs`, commit `958efdb`; native index `src/paths.rs` | Deterministic Windows regression fails before repair and passes after. 39 filesystem tests pass. At most 500 ms of sharing-violation acquisition retries; no mutation replay or weaker delete sharing. A lasting lock is still refused. Full GUI rerun pending. |
| MEDIUM | TUI used incompatible HTTP/mock behavior instead of the desktop engine. | Sibling `zeus-tui/internal/client/local.go`; native app `src/terminal.rs`; Settings Backend view | Authenticated CLI IPC, same profile, real controls, bounded output and explicit URL review. Go tests, shared-profile download/pause/resume and real desktop console launch pass. |
| HIGH | Cached sidecars and stale bundle selection could put unverified binaries into a release. | `apps/client/scripts/{release-proof,build-release}.mjs` | Four release-guard tests pass. Clean commits, exact versions, binary hashes, freshly built extension/TUI and an exact requested installer inventory are required. Packaging and installed-artifact acceptance remain pending. |

## Scope and limits

- HTTP(S), local file/index/archive/organizer/storage features and reviewed agent actions are exercised with controlled fixtures. External cloud account transfers and live provider inference are separate gates, not implied by discovery tests.
- The TUI shares jobs/files/cloud status and controls with the desktop. Full indexed search, advanced file operations and natural-language agent tasks remain desktop functions.
- FTP and BITS are not implemented transports. Existing torrent/source paths require their own acceptance. Browser handoff cannot promise interception of every application or protocol.
- External MCP remains bounded and read-only until a separately trusted human review channel is implemented. Desktop review does not approve daemon/browser terminal requests.
- Development browser extensions still need manual installation and terminal decisions; no store publication or signing is claimed.
- Zeus's public demo was retired with HTTPS 410. Dynodoc signed-in services were preserved. Earlier SSH/HSTS changes remain staged.

## Verification locations

The current authoritative ledger is `A:\ogun\audit\2026-10-30\remediation\STATUS.md`, with central handoff `A:\verne-handoff\2026-10-26\STATE-zeus.md`. Logs and source worktrees live in `D:\verne-build\security-2026-10-30`; GUI evidence is in `D:\verne-build\audit-zeus-desktop\evidence`.

Windows checks: `A:\ogun\audit\2026-10-30\remediation\zeus-windows-checks.ps1`. They use locked dependencies, two compile jobs, D: targets/caches and a C: headroom guard. Failed attempts are retained. Linux core evidence for the preceding `b4b66da` revision contains 340 passing tests. The Ubuntu 22.04 builder image is `sha256:ea1f6f0db02114569240c1c7530be7cee5dbf64a201768392517198313737371`; current app validation has not yet completed. No result is posted as a GitHub check until its exact source revision has been verified.
