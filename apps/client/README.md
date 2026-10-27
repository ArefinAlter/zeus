# Zeus client (pre-alpha release 0.0.1b)

A download manager and file search tool driven by agents, in one window: downloads with categories, queues and a
properties panel; a link grabber with a two-step review; a site grabber; queues, a scheduler and a speed limiter;
instant file search with Everything-style syntax; agent threads on Zeus's thirteen MCP tools with your own model keys;
approvals shown as a trust surface; a page that connects Claude Code, Cursor, Codex and your browser; and settings that
are saved and applied, or marked planned and switched off.

**The desktop app is standalone.** It provisions its own Zeus installation in the per-user data folder
(`%LOCALAPPDATA%\dev.verne.zeus.client`, `~/.local/share/dev.verne.zeus.client`), runs the bundled `zeusd` as a
supervised child process, adopts a daemon left by a crash, restarts a dead one and stops it when you quit. No account,
no sign-in, no daemon to start by hand. `docs/ACCEPTANCE-0.0.1b.md` lists what was proved and what was not.

| Where | What |
|---|---|
| `src-tauri/` | The `zeus-client` shell (Tauri 2, its own Cargo workspace). One closed command, `backend_call(route, body)`, that dispatches into `zeus-app::routes`. The page never sees the daemon credential: the app holds its own `app` credential (agent kind, role `gui`, no `approvals.decide`). |
| `src-tauri/crates/zeus-app` | The core: home folders, supervisor, daemon client, engine settings (`config.toml`), shell settings, held approvals, link review, state slices, OS integration. |
| `src-tauri/crates/zeus-app-index` | The file index: scan, watcher, search, preview, file operations, bookmarks, history. |
| `src-tauri/crates/zeus-app-grab` | The site grabber: crawl, `robots.txt`, HTML link extraction, plans. |
| `src-tauri/crates/zeus-app-agent` | The in-app agent: provider clients (Anthropic, OpenAI-compatible, Gemini, Ollama), keys in the OS credential store, the closed table of the 13 tools, spend ledger. |
| `src-tauri/crates/zeus-app-integration` | Browser and MCP integration: the packaged extension, the native-messaging host registration, MCP configuration and a real handshake test. |
| `src-tauri/crates/zeus-app-ext` | Registers the extension crates on the route table; also builds `zeus-app-http`, a loopback harness (feature `http-harness`) that the acceptance scripts drive. Never part of a release. |
| `src/` | The Preact interface. `backend/live.ts` is the engine-backed store; the other backends (hosted pre-alpha, scripted sample data) remain for the website build. |
| `tests/acceptance/` | One script per scenario (`s01` to `s11`), run against the real engine and a real browser engine. See below. |
| `scripts/build-release.mjs` | Builds every release asset. |

Approvals are human-only (Zeus ADR 0007). A login lent from your browser, a handoff and a destructive file operation are
decided at the Zeus command line (`zeus approvals`) or in the browser extension. The window shows them. Approvals for
site-grabber plans are held by the app and labelled as such.

## Develop and test

```sh
cd apps/client
npm ci
npm run dev            # the interface on http://localhost:5174 against the hosted pre-alpha or sample data
npm run typecheck
npm test               # node --test: search syntax, schedules, limiter, link intake, approvals, tool access
npm run build          # dist/web, the desktop frontend
npm run build:harness  # dist/harness, the interface for the loopback harness
```

Acceptance scripts (Windows or Linux; a scratch folder through `ZEUS_APP_HOME`, never your own data):

```sh
cd src-tauri && cargo build -p zeus-app-ext --features http-harness --bin zeus-app-http
cd .. && npm run build:harness
node tests/acceptance/s01-download.mjs      # ... through s11-settings.mjs
```

`ZEUS_CLIENT_TARGET` names the Cargo target directory that holds `zeus-app-http`. Scenario 8 starts a real Microsoft Edge
in a throw-away profile and registers a native-messaging host in `HKCU` for it.

## Build the app and its installers

```sh
node scripts/build-release.mjs --out <dir>      # Windows: NSIS installer, portable zip, CLI archive, extension zips, SHA256SUMS.txt
node scripts/build-release.mjs --out <dir>      # Linux: .deb, AppImage, CLI archive, extension zips, SHA256SUMS.txt
```

The script builds `zeusd`, `zeus`, `zeus-native-host` and `zeus-web-bridge` in release mode, copies the first three to
`src-tauri/binaries/` for Tauri's `externalBin` (merged from `tauri.bundle.conf.json`, so a plain `cargo build` of the
shell does not need them), then runs `tauri build`. On Windows the generated NSIS script is changed to install under
`%LOCALAPPDATA%\Programs\Zeus` (Tauri's default, `%LOCALAPPDATA%\Zeus`, is where `zeusd` keeps its own state when it runs
on its own). Caches follow `CARGO_TARGET_DIR` and `npm_config_cache`. Nothing is uploaded or committed by the script.

The installers are unsigned and there is no automatic update.

## Environment

| Variable | Effect |
|---|---|
| `ZEUS_APP_HOME` | An absolute folder that holds everything the app keeps, for tests and second profiles. Downloads stay inside it. |
| A `ZeusData` folder beside the executable | Portable mode (the portable zip ships one). |

## Website build

The hosted pre-alpha (`https://zeus.103.191.50.32.sslip.io`, a real `zeusd` behind `zeus-web-bridge` that fetches only
sample fixtures) uses the same interface built with `npm run build:hosted`. The scripted build (`npm run build:scripted`)
runs on sample data with no network request.
