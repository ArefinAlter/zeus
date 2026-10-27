# Acceptance of Zeus 0.0.1b

Release 0.0.1b (pre-alpha) makes the Zeus desktop app, `apps/client`, standalone software: it provisions and supervises its own `zeusd`, so there is no daemon to start by hand and no account. This page records, scenario by scenario, what was run, what it proved and what it did not. Where a claim is not proved, it says so.

Status words: **passed** means every check of the scenario's script passed on the final code; **passed with limits** means the scenario's core is proved and the limits are listed; **not done** means it is not claimed.

## Summary

| # | Scenario | Result | Evidence (script, in `apps/client/tests/acceptance/`) |
|---|---|---|---|
| 1 | Real download on a fresh profile | Passed | `s01-download.mjs`; installed-app run (below) |
| 2 | Kill and resume, plus failure modes | Passed | `s02-resume-failures.mjs` |
| 3 | Link grabber, categories, duplicates | Passed | `s03-grabber.mjs` |
| 4 | Queues, scheduler and speed limiter in `zeusd` | Passed with limits | `s04-queues.mjs`, `crates/zeus-daemon/tests/limiter_throughput.rs` |
| 5 | Files: index with watcher, search, preview, bookmarks, history | Passed | `s05-files.mjs` |
| 6 | Site grabber | Passed | `s06-sitegrab.mjs` |
| 7 | Quarantine and verification | Passed | `s07-verify.mjs` |
| 8 | Browser integration | Passed with limits | `s08-browser.mjs`, [s08-s09-integration.md](acceptance/s08-s09-integration.md) |
| 9 | MCP page and a real MCP client | Passed | `s09-mcp.mjs`, `mcp-client.mjs` |
| 10 | In-app BYOK agent with the 13 tools | Passed with limits | `s10-agent.mjs` |
| 11 | Settings persistence, nothing decorative | Passed | `s11-settings.mjs` |

Packaging, installation and release checks are at the end.

## How the scripts run

The scripts run on Windows 11 against the real thing: the real `zeusd` (the app's `zeus-app-http` harness binary wraps the same `zeus-app` core that the Tauri shell wraps), the real interface in Chromium (Playwright), and a local fixture server that serves files of any size with ranges, redirects, `Content-Disposition`, `Retry-After`, `403` and `410`, no-range and slow-rate behaviour. Every script uses a scratch folder through `ZEUS_APP_HOME` and never touches a person's own data. They start and stop their own processes.

What is real in them and what is not:

| Real | Stand-in |
|---|---|
| `zeusd`, its journal, queue file, limiter, hashing, quarantine; the app's supervisor, routes, index, watcher, grabbers; the file system; the Windows registry for the native host; a real Microsoft Edge (scenario 8); the OS credential store (scenario 10) | The fixture server stands in for web servers; local providers that speak the Anthropic and OpenAI wire formats stand in for model providers (scenario 10); a small forward proxy stands in for a corporate proxy (scenario 11) |

The harness is a loopback HTTP wrapper around the app's route table, used only by tests. It is not part of any release asset. The installed app (NSIS build, empty profile) was also driven by hand and with screenshots; see "Installed app".

## 1. Real download on a fresh profile: passed

`s01-download.mjs` starts from an empty app folder with no daemon running. The app provisions its installation and starts `zeusd` (engine ready in 18.9 s on a cold, busy disk; 2.9 s on a warm restart in scenario 2), then a 96 MiB file (served at 12 MiB/s per connection) is added through Add URL.

Observed: probe with name, type and verified range support; states `queued, probing, downloading, verifying, completed`; 29 progress samples with 10 distinct byte counts, speed up to 13.8 MB/s and a time left; `zeusd`'s SHA-256 equals the server's and the file on disk has all 100,663,296 bytes; the file appears in the app's index and in `zeusd`'s file list; the folder opens. The credential the page's engine uses has twelve permissions and no `approvals.decide`.

Limits: the transfer is from a local server. The same script takes `--url` for a public address; see the real-internet run below.

@@REAL_INTERNET@@

## 2. Kill and resume, plus failure modes: passed

`s02-resume-failures.mjs`: the app and `zeusd` are killed at 44 MiB committed; on relaunch the engine is back in 2.9 s, the job keeps its 44 MiB, completes with the right SHA-256, and the server saw no request for bytes below 44 MiB (37 ranged requests after the restart, lowest start 44 MiB). A server without ranges downloads in one piece and says "unsupported"; three redirects are followed; the `Content-Disposition` name is used; `403` (an expired signed link) fails with `http.status_403` and `410` fails without retry; `503` with `Retry-After` schedules a retry and then succeeds (gaps 0.2, 3.1, 0.1 s).

Limit: `zeusd` itself names a file from the URL path. The app probes first and uses the server's `Content-Disposition` name when it enqueues, so the name is right in this app; a client that enqueues without probing gets the URL name.

## 3. Link grabber, categories, duplicates: passed

`s03-grabber.mjs`: a pasted list of mixed good and bad addresses is checked online (`jobs.probe`), reviewed as a batch (a dry-run enqueue) and started exactly once; categories decide the folder; per-category folders and "ask each time" and "remember the last folder" work; name clashes follow the policy (fail, rename with `(1)`, replace); duplicates are flagged and Start waits for a choice; the folder rule for a category survives a restart.

Limits: "Replace" is done by the app, which moves the old file to the recycle bin at the person's click and then downloads. `zeusd`'s own `overwrite: replace` needs a human-approved destructive operation, and the test shows `zeusd` refuses it for the app's credential (`policy.approval_denied`) and leaves the file alone.

## 4. Queues, scheduler and speed limiter: passed with limits

`s04-queues.mjs`: queues live in `zeusd` (`queues.json`, spec 05 section 9.14). A queue with a start time holds its three downloads (no byte requested), starts by itself at the time (due 15:51, started 15:51:01), runs one at a time when set to one and two at a time when set to two, stops and asks for its on-finish action when the last file ends, and stopping the queue pauses its downloads and keeps their bytes. The Queues page and the Speed limiter page show what `zeusd` stores.

The limiter is a throttle in `zeusd`'s HTTP transport (global, per queue, with a schedule window). Measured against the limit, with a receiver-side measurement in `crates/zeus-daemon/tests/limiter_throughput.rs`:

| Case | Limit | Measured |
|---|---|---|
| One global limit, 1 file | 1024 KiB/s | 1024 KiB/s (0.0 %) |
| Two downloads share one limit | 1024 KiB/s | 1024 KiB/s (0.0 %) |
| A lower queue limit under a higher global one | 512 KiB/s | 512 KiB/s (0.0 %) |
| Inside the schedule window | 1024 KiB/s | 1019 KiB/s (-0.5 %) |
| Outside the schedule window | none | not slowed (672,313 KiB/s on loopback) |

All inside 10 percent of the limit. Evidence: `D:\verne-build\zeus\evidence\limiter-throughput-test.txt` (kept in the release notes' evidence bundle, not in the repository).

Limits: the end-to-end run in `s04-queues.mjs` measures at the server and reads high (1311 KiB/s for a 1024 limit, 669 for 512, because the sockets' buffers fill ahead of the receiver); it proves the limit is effective (the server was slowed from about 9,000 to about the limit), not its precision. The precise number is the receiver-side test above. Both were measured on a debug build.

## 5. Files: passed

`s05-files.mjs`: 2,001 files made on disk are all in the index within 1.6 s of the last write, and the watcher and scanner agree (2,037 entries counting folders). Partial files, `node_modules`, `Thumbs.db`, hidden files and `.zeus` folders are skipped. Search operators (`ext:`, `size:`, `dm:`, `path:`, `folder:`, `file:`, `regex:`, wildcards, `!`, `;` lists) and the case, path and regex toggles give the counts the disk gives. The Picture filter and the folders pane work. A text file previews its start, a picture previews as an image with its real size, an archive shows a facts card, not its bytes. Files created, renamed and deleted while the page is open appear within seconds. Rename (including a refusal onto an existing name), move (creating the folder) and delete (to the recycle bin; permanent delete only after a second step) work through the page. The engine refuses a path outside the download folders and any path with `..`; quarantined files are never previewed or changed. Bookmarks and history survive a restart.

Scale: with `--big 50000` the index holds 50,000 files in 1.3 s on the same machine (run on 2026-10-27; screenshot `08-big-index.png`).

Limits: previews cover text and pictures only; the index is names, sizes and dates, never contents.

## 6. Site grabber: passed

`s06-sitegrab.mjs` against a local fixture site: a same-host walk of 7 pages and 7 files in 16 s; depth 2 reads the pages within depth and not deeper ones; exactly the allowed files are found with their sizes and where each was found; `robots.txt` is read first, the private area is never requested, and the `Crawl-delay` spaces the requests (smallest gap 965 ms over 17 requests although 250 ms was asked); the walk identifies itself as `Zeus/0.0.1-beta (+site grabber)`; type filters, include and exclude rules, the file limit and depth 0 work; addresses that are not http or https, or carry a password, are refused; a walk cancels in 65 ms and no request follows. Through the dialog, the plan approval shows its facts and who holds it ("Zeus app (this window)", a human click, labelled as app-held and not a daemon approval); after it the four files download with exactly the bytes the site serves.

Limits: the walk reads HTML links only (no scripts); it never leaves the host you name.

## 7. Quarantine and verification: passed

`s07-verify.mjs`: SHA-256 and BLAKE3 equal an independent computation; a matching published hash is recorded as verified; a mismatching one fails the job with `integrity.hash_mismatch` and leaves no completed file (the received bytes stay only as a `.zpart`); executables (`.exe`, `.sh`, `.AppImage`, `.ps1`, `invoice.pdf.exe`, `.dll`, `.msi`, `.bat`) are quarantined with their reasons and the app refuses to open them; "Verify again" has `zeusd` re-read the file and agrees, and after one byte changes on disk it reports the mismatch; a malformed hash is refused; the properties panel shows these facts and the terminal route to release a quarantine.

## 8. Browser integration: passed with limits

Details and the run log are in [acceptance/s08-s09-integration.md](acceptance/s08-s09-integration.md). `s08-browser.mjs` runs the app's own install route (consent required), which registers the native-messaging host for Microsoft Edge in `HKCU`, starts a real Edge in a throw-away profile with the packaged extension loaded, and a fixture server whose download the browser cannot finish. The extension offers the failed download to Zeus, reaches `zeus-native-host`, which reaches the app's `zeusd`; a job exists in the engine, made from the browser, and an approval of kind `handoff` is pending (visible through the app's approvals route). Nothing downloads, and nothing an MCP, agent or app route does decides it.

Limits, stated plainly:

- The extension loads as a development extension with a fixed development id. `zeusd` refuses decisions from the extension popup for a development id ("This version of Zeus cannot take decisions from the browser"). So the run reaches the pending approval, and completion after approval was not demonstrated in an automated run: a person decides it at the terminal with `zeus approvals`.
- The browser was Microsoft Edge on Windows. Chrome, Firefox and Linux browsers were not run. The Firefox package exists and was not loaded in a browser.
- Approvals stay human-only: the window lists them and has no button that decides one.

## 9. MCP page and a real MCP client: passed

`s09-mcp.mjs`: the Connect page gives configuration for Claude Code (`claude mcp add ... zeus -- <zeus> mcp`), Cursor (`.mcp.json`), Codex (a `[mcp_servers.zeus]` table) and a generic entry; each parses and carries the four variables that select this app's engine. The app's connection test lists thirteen tools (zeus 0.0.1-beta, 89 ms). A separate MCP client (`tests/mcp-client.mjs`) started from the generated configuration over stdio initialises (protocol 2025-06-18), lists the thirteen tools, probes a URL, enqueues a download, waits for it, verifies it and gets the SHA-256 the server holds. No tool can decide an approval; a session request creates a pending approval and nothing more; a call to an approving tool is refused as unknown.

Limit: tested with this repository's MCP client, not with Claude Code, Cursor or Codex themselves.

## 10. In-app BYOK agent with the 13 tools: passed with limits

`s10-agent.mjs` (43 checks): a new installation has no model switched on, no consent and no key; a key is stored in the OS credential store and the page only learns "stored" and its last four characters; the configuration never contains it, and it is in no file of the app folder, engine state or logs and no response or process output. Before the first request the page lists what would be sent and what is never sent, and no request is made without consent. The model calls `zeus_enqueue` on a real `zeusd` and the download completes with the right SHA-256; the tool result goes back to the model labelled as data; usage and spend are recorded per call and per thread. Access levels refuse what a thread may not do (read only refuses a download); unknown tools and "approve" actions are refused; a switched-off tool is refused with its reason; a cap of zero and the offline switch mean no call; stop ends a streaming answer in 110 ms. A session request raised by the agent waits for a person and the card says how to decide it at a terminal; no route of the app decides it.

Limits: the providers were local servers that speak the real Anthropic and OpenAI wire formats (request shapes, headers, streaming, tool use), not live providers; no live key was used. Gemini and Ollama clients exist and were exercised by unit tests only.

## 11. Settings persistence, nothing decorative: passed

`s11-settings.mjs`: every Settings section is changed through the interface, the engine is applied and restarted, the app is stopped and started again on the same folder, and a fresh browser (empty local storage) shows every change: General (start page, time format, confirm before remove, start minimized), Appearance (density, content width, theme and colour scheme), Keybindings (a recorded shortcut), File types (an edited extension list), Save to (a folder per category, ask each time), Downloads (files at once, connections per file, the name policy, start at once), Connection (an HTTP proxy), Scheduler (time zone, missed start, stop behaviour), Notifications, Agent (largest batch, default access) and Storage (record retention).

Engine settings reach `zeusd`'s `config.toml` (`concurrent_jobs = 6`, `connections_per_job = 5`, `tombstone_days = 45`); the scheduler defaults live in `zeusd`; "start minimized" is in the app's shell settings file. The HTTP proxy is used: after the restart a download goes through a local forward proxy that records the request. Rows that do nothing yet (metered connections, IPv6, a SOCKS proxy, cleanup schedules) are marked "planned" and switched off, and the test checks that every planned row's control is inert.

## Installed app

@@INSTALLED@@

## Packaging and release checks

@@PACKAGING@@

## What is not proved in 0.0.1b

- Deciding an approval from the browser extension popup (development extension id). Decisions are at the terminal.
- A live model provider call. A live Gemini or Ollama call.
- Chrome, Firefox, macOS. Linux was built and smoke-tested headless on one distribution; no browser integration run on Linux.
- Signed installers, automatic update, a store version of the extension.
- Long-running behaviour (days), very large queues, and downloads from sites that need a browser session (the session bridge is in `zeusd`, not exercised by the app's acceptance).
- Accessibility with a screen reader and 200 percent zoom.

## Source changes behind the release

No change to `zeus-core`. No new ADR file: ADR 0012 gained one row (the `queues.json` store) and spec 05 gained section 9.14 (queues, the scheduler and the speed limiter). `zeus`, `zeusd`, `zeus-native-host` and `zeus-web-bridge` report version `0.0.1-beta`.
