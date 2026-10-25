# Zeus client (pre-alpha release)

A download manager and file search tool driven by agents, in one window: downloads with categories, queues and a
properties panel; a link grabber with packages and a two-step review; a site grabber; queues, a scheduler and a speed
limiter; instant file search with Everything-style syntax; agent threads on Zeus's thirteen MCP tools; approvals as a
trust surface; connectors; skills; and extensive settings.

**This release runs on sample data with no backend.** It does not connect to `zeusd`, makes no network request and
downloads nothing. The real, restricted desktop client for a running daemon is [`apps/desktop`](../desktop/README.md).

| Real (follows Zeus's specifications and tests) | Prepared (scripted in this release) |
|---|---|
| Job states and pause, resume, retry, cancel (spec 01) | Sample jobs on reserved `example.*` domains, speeds, one expired link, one 503 retry |
| Link review: Preview downloads, then Start reviewed downloads; 50 links, 64 KiB | Online checks and sizes |
| Approval facts in a fixed order, scopes, request hash binding, bind code, 750 ms arming, typed codes (spec 07, ADR 0007) | The hash is SHA-256 over the canonical facts, standing in for zeusd's BLAKE3 over JCS |
| The agent's closed tool table: the 13 tools of `zeus-mcp`, access levels, refusals | The agent's replies (no model is called) |
| Search syntax, queue schedules and limiter rules (tested in `src/model`) | The file index (zeusd has none yet) and queue scheduling (open in zeusd) |

Interface conventions follow T3 Code (as Ogun Studio does); the accent and state colours come from
`zeus-site/brand/BRAND.md`. Fonts are the system fonts; nothing is fetched.

## Develop and test

```sh
cd apps/client
npm ci
npm run dev            # http://localhost:5174
npm run typecheck
npm test               # node --test: search syntax, schedules, limiter, link intake, approvals, tool access
npm run build          # dist/web, the desktop frontend (preferences kept on the device)
npm run build:site     # dist/site, the website copy (no browser storage at all)
```

## Desktop app

`src-tauri` is an independent Cargo workspace (ADR 0009 pattern), excluded from the daemon workspace. It has no app
commands and no plugins; the capability grants the main window only its own title-bar controls, and navigation away
from the bundled pages is refused. The window is frameless on Windows and the page draws the title bar.

```sh
node scripts/build-desktop.mjs    # builds dist/web, then the shell; copies the exe to dist/desktop/zeus.exe
```

## Website copy

```sh
npm run build:site && node scripts/publish-site.mjs ../../../zeus-site   # writes zeus-site/public/client/
```

The copy is marked `noindex`, keeps preferences for the visit only, holds any provider key typed into Settings in
memory only, and loads only its own files, so it works under the site's Content Security Policy.
