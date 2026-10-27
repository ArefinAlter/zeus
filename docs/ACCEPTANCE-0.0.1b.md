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

The output of the final run of each script is stored in [acceptance/logs/](acceptance/logs/) (one text file per script). Packaging, installation and release checks are at the end.

## How the scripts run

The scripts run on Windows 11 against the real thing: the real `zeusd` (the app's `zeus-app-http` harness binary wraps the same `zeus-app` core that the Tauri shell wraps), the real interface in Chromium (Playwright), and a local fixture server that serves files of any size with ranges, redirects, `Content-Disposition`, `Retry-After`, `403` and `410`, no-range and slow-rate behaviour. Every script uses a scratch folder through `ZEUS_APP_HOME` and never touches a person's own data. They start and stop their own processes.

What is real in them and what is not:

| Real | Stand-in |
|---|---|
| `zeusd`, its journal, queue file, limiter, hashing, quarantine; the app's supervisor, routes, index, watcher, grabbers; the file system; the Windows registry for the native host; a real Microsoft Edge (scenario 8); the OS credential store (scenario 10) | The fixture server stands in for web servers; local providers that speak the Anthropic and OpenAI wire formats stand in for model providers (scenario 10); a small forward proxy stands in for a corporate proxy (scenario 11) |

The harness is a loopback HTTP wrapper around the app's route table, used only by tests. It is not part of any release asset. The installed app (NSIS build, empty profile) was also driven by hand and with screenshots; see "Installed app".

## 1. Real download on a fresh profile: passed

`s01-download.mjs` starts from an empty app folder with no daemon running. The app provisions its installation and starts `zeusd` (engine ready in 1.9 s in the final run; the first run on a cold, busy disk took 18.9 s), then a 96 MiB file (served at 12 MiB/s per connection) is added through Add URL.

Observed: probe with name, type and verified range support; states `queued, downloading, verifying, completed`; 12 progress samples with 7 distinct byte counts, speed up to 35.1 MB/s and a time left; `zeusd`'s SHA-256 equals the server's and the file on disk has all 100,663,296 bytes; the file appears in the app's index and in `zeusd`'s file list; the folder opens. The credential the page's engine uses has twelve permissions and no `approvals.decide`.

Limits: the transfer is from a local server. The same script takes `--url` for a public address; see the real-internet run below.

Real internet: `node tests/acceptance/s01-download.mjs --url https://proof.ovh.net/files/10Mb.dat --sha256 e5b844cc57f57094ea4585e235f36c78c1cd222262bb89d53c94dcb4d6b3e55d` downloads a public 10 MiB test file over https (probe: status 206, range support verified). The states are `queued, probing, retry_scheduled, downloading, completed` (the engine scheduled one retry after a transient network error and went on), and the SHA-256 reported by `zeusd` and the SHA-256 of the file on disk both equal the one computed independently with `curl` and `sha256sum`. The file appears in the app's index and in `zeusd`'s file list.

## 2. Kill and resume, plus failure modes: passed

`s02-resume-failures.mjs`: the app and `zeusd` are killed at 44 MiB committed; on relaunch the engine is back in 2.0 s, the job keeps its 44 MiB, completes with the right SHA-256, and the server saw no request for bytes below 44 MiB (37 ranged requests after the restart, lowest start 44 MiB). A server without ranges downloads in one piece and says "unsupported"; three redirects are followed; the `Content-Disposition` name is used; `403` (an expired signed link) fails with `http.status_403` and `410` fails without retry; `503` with `Retry-After` schedules a retry and then succeeds (gaps 0.0, 3.0, 0.1 s).

Limit: `zeusd` itself names a file from the URL path. The app probes first and uses the server's `Content-Disposition` name when it enqueues, so the name is right in this app; a client that enqueues without probing gets the URL name.

## 3. Link grabber, categories, duplicates: passed

`s03-grabber.mjs`: a pasted list of mixed good and bad addresses is checked online (`jobs.probe`), reviewed as a batch (a dry-run enqueue) and started exactly once; categories decide the folder; per-category folders and "ask each time" and "remember the last folder" work; name clashes follow the policy (fail, rename with `(1)`, replace); duplicates are flagged and Start waits for a choice; the folder rule for a category survives a restart.

Limits: "Replace" is done by the app, which moves the old file to the recycle bin at the person's click and then downloads. `zeusd`'s own `overwrite: replace` needs a human-approved destructive operation, and the test shows `zeusd` refuses it for the app's credential (`policy.approval_denied`) and leaves the file alone.

## 4. Queues, scheduler and speed limiter: passed with limits

`s04-queues.mjs`: queues live in `zeusd` (`queues.json`, spec 05 section 9.14). A queue with a start time holds its three downloads (no byte requested), starts by itself at the time (a 38 s wait in the final run, started within a second of its start time), runs one at a time when set to one and two at a time when set to two, stops and asks for its on-finish action when the last file ends, and stopping the queue pauses its downloads and keeps their bytes. The Queues page and the Speed limiter page show what `zeusd` stores.

The limiter is a throttle in `zeusd`'s HTTP transport (global, per queue, with a schedule window). Measured against the limit, with a receiver-side measurement in `crates/zeus-daemon/tests/limiter_throughput.rs`:

| Case | Limit | Measured |
|---|---|---|
| One global limit, 1 file | 1024 KiB/s | 1024 KiB/s (0.0 %) |
| Two downloads share one limit | 1024 KiB/s | 1024 KiB/s (0.0 %) |
| A lower queue limit under a higher global one | 512 KiB/s | 512 KiB/s (0.0 %) |
| Inside the schedule window | 1024 KiB/s | 1019 KiB/s (-0.5 %) |
| Outside the schedule window | none | not slowed (672,313 KiB/s on loopback) |

All inside 10 percent of the limit. Evidence: [acceptance/logs/limiter-throughput.txt](acceptance/logs/limiter-throughput.txt).

Limits: the end-to-end run in `s04-queues.mjs` measures at the server and reads high (1306 KiB/s for a 1024 limit, 644 for 512, because the sockets' buffers fill ahead of the receiver); it proves the limit is effective (outside the schedule window the same transfer ran at about 49,000 KiB/s, with the limit on about 1,300), not its precision. The precise number is the receiver-side test above. Both were measured on a debug build.

## 5. Files: passed

`s05-files.mjs`: 2,001 files made on disk are all in the index within seconds of the last write (3.6 s in the final run), and the watcher and scanner agree (2,037 entries counting folders). Partial files, `node_modules`, `Thumbs.db`, hidden files and `.zeus` folders are skipped. Search operators (`ext:`, `size:`, `dm:`, `path:`, `folder:`, `file:`, `regex:`, wildcards, `!`, `;` lists) and the case, path and regex toggles give the counts the disk gives. The Picture filter and the folders pane work. A text file previews its start, a picture previews as an image with its real size, an archive shows a facts card, not its bytes. Files created, renamed and deleted while the page is open appear within seconds. Rename (including a refusal onto an existing name), move (creating the folder) and delete (to the recycle bin; permanent delete only after a second step) work through the page. The engine refuses a path outside the download folders and any path with `..`; quarantined files are never previewed or changed. Bookmarks and history survive a restart.

Scale (the script's last section): 50,000 more files made on disk are indexed in 1.7 s (29,377 files per second, first results after 0.9 s); a rescan of the resulting 52,037 unchanged entries takes 0.7 s; the engine process holds 34.8 MB with all of them; a search in the page answers in 11 to 30 ms wall time (the engine reports 2 to 17 ms). After a restart the saved index serves at once and is verified in the background.

Limits: previews cover text and pictures only; the index is names, sizes and dates, never contents.

## 6. Site grabber: passed

`s06-sitegrab.mjs` against a local fixture site: a same-host walk of 7 pages and 7 files in 16 s; depth 2 reads the pages within depth and not deeper ones; exactly the allowed files are found with their sizes and where each was found; `robots.txt` is read first, the private area is never requested, and the `Crawl-delay` spaces the requests (smallest gap 1,001 ms over 17 requests although 250 ms was asked); the walk identifies itself as `Zeus/0.0.1-beta (+site grabber)`; type filters, include and exclude rules, the file limit and depth 0 work; addresses that are not http or https, or carry a password, are refused; a walk cancels in 113 ms and no request follows. Through the dialog, the plan approval shows its facts and who holds it ("Zeus app (this window)", a human click, labelled as app-held and not a daemon approval); after it the four files download with exactly the bytes the site serves.

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

The final Windows installer was run on the build machine with a scratch data folder (`ZEUS_APP_HOME`), so none of the person's own data was touched. Screenshots: `docs/images/zeus-0.0.1b-first-launch.png`, `-downloading.png`, `-completed.png`, `-settings.png`.

1. `Zeus_0.0.1-beta_x64-setup.exe /S` installs for the current user into `%LOCALAPPDATA%\Programs\Zeus` (not into `%LOCALAPPDATA%\Zeus`, where `zeusd` keeps its own state when it runs alone) with `zeus-client.exe`, `zeusd.exe`, `zeus.exe`, `zeus-native-host.exe` and `uninstall.exe`; no administrator prompt.
2. Starting `zeus-client.exe` starts its engine without a command line: an empty download list appears with no error, and the engine runs from the install folder.
3. Add URL on a local fixture file of 120 MiB: the check answers `206 · 120 MiB · application/octet-stream · online`, "the server honours byte ranges", and the file name comes from `Content-Disposition` ("Annual Report.pdf"). Start: progress 26.7 % at 6.27 MiB/s with 14 s left, then `completed · hashed`. The file on disk has 125,829,120 bytes and its SHA-256 (`039df70f…24a8`) equals the server's.
4. Killing the app leaves `zeusd` running (same process id); starting the app again adopts it. The download list is intact after a restart.
5. With "Closing the window hides it to the notification area" on (the default), closing the window leaves the app and `zeusd` running; with it off, closing the window ends both. The switch is saved in the app's `shell.json`.
6. `uninstall.exe /S` removes the install folder and the uninstall entry. The installer's own location key under `HKCU\Software\Verne Ltd.\Zeus` stays (a reinstall goes to the same folder, as NSIS installers do).

The checks above were made with the first build of the installer; the installer that is released (SHA-256 `d8d6b8e3…76b0`, built after the lockfile change described under "Source changes behind the release") was installed, started, used and uninstalled again in a scripted run: it installs into `%LOCALAPPDATA%\Programs\Zeus`; `zeus --version` and `zeusd --version` print `0.0.1-beta`; the engine is ready after 3 s; a 20 MiB download made with the bundled `zeus get --wait --sha256 …` and the environment the Connect page gives completes in the app's engine with the server's SHA-256; closing the window (with hide-to-tray off) ends the app and `zeusd` within 11 s; `uninstall.exe /S` removes the folder and the uninstall entry.

Limit: the installed app was driven on Windows 11 only. Linux is below.

## Packaging and release checks

All assets are built from the commit that is tagged `v0.0.1b` by `apps/client/scripts/build-release.mjs`. Sizes and SHA-256 are also in the release's `SHA256SUMS.txt`.

| File | Size | SHA-256 |
|---|---|---|
| `Zeus_0.0.1-beta_amd64.AppImage` | 102.1 MiB (107,055,608 bytes) | `be01adfaf43d7f6dbff9ea12ca1d02031d78229f57974a510285fcfff35d2d31` |
| `Zeus_0.0.1-beta_amd64.deb` | 30.5 MiB (31,943,150 bytes) | `deaecb7c7d1fc12181c8eb412dd06469d4ffdec11a194c5a3605b327efc5dbc3` |
| `Zeus_0.0.1-beta_x64-setup.exe` | 19.6 MiB (20,524,333 bytes) | `d8d6b8e300cb2d4524d5c27b2628513bcbbae4e744cba5e207bba9c6202976b0` |
| `zeus-cli-0.0.1b-linux-x64.tar.gz` | 21.9 MiB (22,977,680 bytes) | `b65f3658356c4166594e2f7cd5cdd3ca7bb59a901a99d097329c73b007aac969` |
| `zeus-cli-0.0.1b-windows-x64.zip` | 21.0 MiB (22,035,989 bytes) | `b5cea46972d078b8bbf50eb29c6b239d5de8ed65c6c778eb7dee60fc13bc755f` |
| `zeus-client-0.0.1b-windows-x64-portable.zip` | 28.1 MiB (29,476,422 bytes) | `2244e6c01cd5641bee6f744d867f917d455235684fd0d2f4a4da7459fa7c14ec` |
| `zeus-extension-chromium-0.0.1b.zip` | 0.4 MiB (382,885 bytes) | `edd36916c8e91995e39cc45065ed28d4a5a68b69c178a359095c011907310914` |
| `zeus-extension-firefox-0.0.1b.zip` | 0.4 MiB (382,599 bytes) | `109cdd3f4ce74a2c4db9f61f33cd47aa12a41e59d7e2025b8deca6defc281433` |

**Windows (built on Windows 11, Rust 1.99, Node 22).** The NSIS installer installs per user, without an administrator prompt, into `%LOCALAPPDATA%\Programs\Zeus`; the generated NSIS script is changed by the build script because Tauri's own default folder is the one `zeusd` uses for its state when it runs alone. The portable zip carries `zeus-client.exe`, its three sidecars and an empty `ZeusData` folder that selects portable mode. The installed app, its restart behaviour and the uninstall were checked (see "Installed app"). Both installers are unsigned.

**Linux (built in a private WSL 2 distribution, `zeus-build`, a clone of an Ubuntu 22.04 build image with Rust 1.99.1, Node 22 and WebKitGTK 4.1).** The binaries need glibc 2.34 or newer (Ubuntu 22.04, Debian 12). The `.deb` is the package `zeus` with `zeus-client`, `zeus`, `zeusd` and `zeus-native-host` in `/usr/bin` and the dependencies `libayatana-appindicator3-1`, `libwebkit2gtk-4.1-0` and `libgtk-3-0`. The AppImage stages its three sidecars into `<data folder>/bin` at start, because an AppImage is mounted at a path that changes with every run and the native-messaging host and the MCP configuration need a stable one.

Smoke test, headless, in the same distribution (a clone of the build image; Xvfb with a D-Bus session, `WEBKIT_DISABLE_COMPOSITING_MODE=1`, `WEBKIT_DISABLE_DMABUF_RENDERER=1`, and `GDK_BACKEND=x11` with `WAYLAND_DISPLAY` unset: WSLg exports a Wayland display, and without these the window opens on the Windows desktop and the Xvfb screen stays black):

| Check | AppImage | .deb |
|---|---|---|
| Starts without a command line; engine ready | after 5 s (first run, sidecars staged) | after 2 s |
| Window "Zeus", 1360 x 880, and a tray icon on the X server | yes | yes |
| A 48 MiB download with the bundled `zeus get --wait --sha256 …` into the app's own engine (the profile and folders that the Connect page gives) | completed, SHA-256 equal to the source | completed, SHA-256 equal to the source |
| The app's list shows it as completed and hashed (screenshot `docs/images/zeus-0.0.1b-linux-appimage.png`) | yes | yes |
| `apt-get install ./Zeus_0.0.1-beta_amd64.deb` resolves its dependencies and `apt-get remove zeus` removes it | | yes |
| `zeus`, `zeusd`, `zeus-web-bridge` from the CLI archive print `0.0.1-beta` | yes | |

Limits: one distribution and one environment (WSL 2); no display server of a real desktop, no window manager, no clicks (the interface itself was driven on Windows, and the page is the same); no browser integration was run on Linux; the tray icon is created but its menu was not used; Wayland, Fedora, Arch and other desktops were not tried.

## What is not proved in 0.0.1b

- Deciding an approval from the browser extension popup (development extension id). Decisions are at the terminal.
- A live model provider call. A live Gemini or Ollama call.
- Chrome, Firefox, macOS. Linux was built and smoke-tested headless on one distribution; no browser integration run on Linux.
- Signed installers, automatic update, a store version of the extension.
- Long-running behaviour (days), very large queues, and downloads from sites that need a browser session (the session bridge is in `zeusd`, not exercised by the app's acceptance).
- Accessibility with a screen reader and 200 percent zoom.

## Source changes behind the release

No change to `zeus-core`. The client's lockfile moved `tokio` from 1.53.2 to 1.53.3 (the Linux build of `interprocess` 2.4.7 needs it; the daemon workspace already used 1.53.3), after which every script above was run again. No new ADR file: ADR 0012 gained one row (the `queues.json` store) and spec 05 gained section 9.14 (queues, the scheduler and the speed limiter). `zeus`, `zeusd`, `zeus-native-host` and `zeus-web-bridge` report version `0.0.1-beta`.
