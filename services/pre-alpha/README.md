# Zeus pre-alpha backend (hosted)

What runs behind https://zeus.103.191.50.32.sslip.io, the hosted Zeus pre-alpha client. None of this is a product
crate: it is deployment and release tooling for the pre-alpha, kept out of the daemon workspace.

| Part | Path | What it is |
|---|---|---|
| zeusd | the real daemon (`crates/zeus-daemon`), Linux x64 build | Downloads, files, artifacts, approvals, workflow reports. One listener: its local IPC socket. |
| zeus-web-bridge | `gateway/` (own Cargo workspace) | The HTTP bridge process (ADR 0010). Anonymous visitor sessions, one download folder and job namespace per session, quotas, the fixture-host allow-list. Calls zeusd with the installation's `gui.token` (kind `agent`, role `gui`, the seven permissions of apps/desktop and no more). Also ships in the release for local use. |
| pre-alpha service | `svc/server.mjs` (Node 24, `node:sqlite`) | What zeusd does not implement yet, stored per sandbox: queues and schedules, limiter settings, every Settings section, link-grabber lists and online checks, site-grabber walks over the sample site, held approvals for plans and file moves, agent threads, skills, connectors, bookmarks, history, and an index of the sandbox's real files. |
| fixtures | `fixtures/` (nginx + `gen.mjs`) | The only download source: `files.example.org`, `mirror.example.net`, `docs.example.org` inside the compose network. Slow (`/slow/`, 512 KiB/s), no-range (`/no-range/`), flaky (`/flaky/`, 503 half the time), expired (`/expired/`, 403), gone (`/gone/`, 410) and broken (`/broken/`, 500) paths. |
| edge | `edge/nginx.conf` | One origin: the static client, `/api/` to the bridge, `/svc/` to the service after `auth_request` to the bridge. The only published port, 127.0.0.1:18540. |
| janitor | `runtime/janitor.sh` | Deletes sandbox files 24 hours after they were written. |

`zeusd`, the bridge, the service and the fixtures share an `internal: true` network: nothing that handles visitor
input can reach the internet. A pasted address whose host is not a fixture host is refused by the bridge before zeusd
sees it, and the service's site walks and online checks are limited to the same hosts.

## Endpoints

zeus-web-bridge (`/api/v1`): `POST session`, `GET status`, `GET jobs`, `POST links/preview` (dry-run enqueue; the
exact inputs stay in the bridge for ten minutes), `POST links/enqueue` (that review, once, with a retry key),
`POST jobs/{id}/control` (`pause`, `resume`, `cancel`, `retry`), `GET files`, `GET artifacts`, `GET approvals`
(listing only), `GET runs`, `GET fixtures`. `GET auth` is for the edge proxy only.

Pre-alpha service (`/svc/v1`): `GET state`, `PUT state/{slice}`, `DELETE state`, `GET index`, `POST index/rescan`,
`POST links/check`, `POST sitegrab/preview`, `GET approvals`, `POST approvals`, `POST approvals/{id}/decide`.

## Visitors

A visitor gets an anonymous random session (cookie `zeus_sid`; the desktop app sends it as a bearer token) on first
visit: a folder `/data/downloads/s/<session>/`, its own job list, 40 files and 1 GiB per 24 hours. Sessions, their
stored settings and their files expire 24 hours after creation. Approvals held by zeusd are only listed; zeusd's
approvals are decided by a person on zeusd's trust surfaces, never by a web page. Plans and file moves proposed in the
client are held by the service, labelled as such, and checked there (request hash, arming delay, offered scopes).

## Deploy

```sh
# Linux x64 binaries (zeusd, zeus, zeus-web-bridge) into runtime/bin/, then:
docker build -t zeus-prealpha-runtime:0.0.1-alpha runtime
docker compose -p zeus-prealpha up -d
docker run --rm -v zeus-prealpha_fixtures:/srv -v "$PWD/fixtures:/gen:ro" node:24-alpine node /gen/gen.mjs
docker compose -p zeus-prealpha restart bridge svc
```

The web client is `apps/client` built with `npm run build:hosted` and copied to `web/`. The host's nginx site is
`host/zeus-prealpha.nginx`.

## Operate (on the VPS, /opt/zeus-prealpha)

- Compose project `zeus-prealpha`; containers `zeus-prealpha-{init,zeusd,bridge,svc,fixtures,edge,janitor}-1`;
  networks `zeus-prealpha_internal` (internal) and `zeus-prealpha_edge`; only `127.0.0.1:18540` is published.
- Status and logs: `docker compose -p zeus-prealpha ps`, `docker compose -p zeus-prealpha logs --tail 50 bridge`.
- Restart: `docker compose -p zeus-prealpha restart` (sessions, settings and files survive; they live in volumes).
- Expiry instead of a nightly reset: sessions end 24 hours after creation (bridge and service), the janitor deletes
  sandbox files 24 hours after they were written. A full reset is `docker compose -p zeus-prealpha down -v`, then the
  deploy steps above.
- nginx: `/etc/nginx/sites-available/zeus-prealpha` (TLS by certbot, rate limits, headers), edited only under
  `flock /tmp/verne-nginx.lock`.

## Local bridge

```sh
zeus daemon install && zeusd run      # once, and keep zeusd running
zeus-web-bridge                       # http://127.0.0.1:18545, token in <state>/bridge/bridge.token
```

The desktop app's Settings, Backend switches to it. Local mode listens on loopback only, requires the token on every
request and allows any http(s) host, as your own zeusd does.
