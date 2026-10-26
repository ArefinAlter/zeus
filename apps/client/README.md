# Zeus client (pre-alpha release)

A download manager and file search tool driven by agents, in one window: downloads with categories, queues and a
properties panel; a link grabber with packages and a two-step review; a site grabber; queues, a scheduler and a speed
limiter; instant file search with Everything-style syntax; agent threads on Zeus's thirteen MCP tools; approvals as a
trust surface; connectors; skills; and extensive settings.

**It talks to a real zeusd.** The website build and the desktop app's default use the hosted pre-alpha
(https://zeus.103.191.50.32.sslip.io): a real zeusd behind `zeus-web-bridge`, the HTTP bridge in
[`services/pre-alpha`](../../services/pre-alpha/README.md), in a private 24-hour sandbox that downloads only from sample
file hosts on that server. In the desktop app, Settings, Backend switches to your own zeusd (through `zeus-web-bridge` on
127.0.0.1) or to the offline sample data. The scripted backend stays as a build mode for offline builds and tests.

| From zeusd (through the bridge, gui credential) | From the pre-alpha service (hosted) or this device (local) |
|---|---|
| Jobs, states, sizes, SHA-256, quarantine; pause, resume, retry, cancel | Queues and schedules (queued downloads go to zeusd when their queue starts), limiter settings |
| Link review: a dry-run enqueue, then exactly that batch once | Every Settings section, categories and file types; provider keys never leave the window |
| Files under the download folder, artifact integrity facts (BLAKE3) | Link-grabber lists and online checks, site-grabber walks and the plans they need approved |
| Approvals listing (decided only on zeusd's trust surfaces) and workflow reports | Search index of the sandbox's files, agent threads, skills, connectors, bookmarks, history |

Approvals for site-grabber plans and file moves proposed in the client are held by the pre-alpha service, labelled as
such, and checked there (request hash, arming delay, offered scopes). The agent's replies are prepared; no model is
called. The real, restricted desktop client for a running daemon remains [`apps/desktop`](../desktop/README.md).

| Rules the client follows (tested in `src/model`) | Prepared |
|---|---|
| Job states and controls (spec 01), the 50-link and 64 KiB review limits | The agent's replies |
| Approval facts in a fixed order, scopes, request hash binding, bind code, 750 ms arming, typed codes (spec 07, ADR 0007) | The connector catalogue's statuses |
| The agent's closed tool table: the 13 tools of `zeus-mcp`, access levels, refusals | The sample data of the offline build |
| Search syntax, queue schedules and limiter rules | |

Interface conventions follow T3 Code (as Ogun Studio does); the accent and state colours come from
`zeus-site/brand/BRAND.md`. Fonts are the system fonts; nothing is fetched from a third party.

## Develop and test

```sh
cd apps/client
npm ci
npm run dev            # http://localhost:5174
npm run typecheck
npm test               # node --test: search syntax, schedules, limiter, link intake, approvals, tool access
npm run build          # dist/web, the desktop frontend (hosted by default, switchable in Settings, Backend)
npm run build:hosted   # dist/hosted, served by the hosted pre-alpha (services/pre-alpha, web/)
npm run build:scripted # dist/scripted, offline on the sample backend (no network request)
```

## Desktop app

`src-tauri` is an independent Cargo workspace (ADR 0009 pattern), excluded from the daemon workspace. It has no app
commands and no plugins; the capability grants the main window only its own title-bar controls, and navigation away
from the bundled pages is refused. Its CSP allows connections only to the hosted pre-alpha and to loopback (your own
`zeus-web-bridge`). The window is frameless on Windows and the page draws the title bar.

```sh
node scripts/build-desktop.mjs    # builds dist/web, then the shell; copies the exe to dist/desktop/zeus-client.exe
npm run build && npx tauri build --bundles nsis          # Windows installer
npm run build && npx tauri build --bundles deb,appimage  # Linux packages
```

## Website copy

The Zeus website no longer serves a static copy: `/client/` there redirects to the hosted pre-alpha. The script below
stays for an offline static copy of the scripted build in any site checkout:

```sh
npm run build:site && node scripts/publish-site.mjs <path to a site checkout>   # writes <site>/public/client/
```

The copy is marked `noindex`, keeps preferences for the visit only, holds any provider key typed into Settings in
memory only, and loads only its own files, so it works under the site's Content Security Policy.
