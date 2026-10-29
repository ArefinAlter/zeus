# Optional Zeus team service · 0.1 alpha

This separate service implements OIDC code/PKCE/nonce sign-in with one-use desktop enrollment, tenant/role-scoped metadata, optimistic revision checks, exact idempotent retries, tombstones, signed restriction leases, signed client-reported artifact provenance and a chained audit trail. It never receives file contents, browser sessions, provider keys, local paths, transfer URLs or transfer approvals. Unknown metadata fields are rejected. Three metadata types are supported: tags, queue presets and workspace labels. Copying a preset does not execute it.

The service is optional. An installation with no `team/pin.json` keeps its current behavior. An installation started with an invalid pin or without a current verified lease fails closed for covered new effects. This is an installation policy boundary, not protection against a local account administrator replacing binaries/configuration.

## Desktop and daemon

An administrator distributes `pin.json` through an independently trusted channel to `<ZEUS_STATE_DIR>/team/pin.json`. Desktop state is `<ZEUS_APP_HOME>/zeus/state`; the installed default is the app's per-user data folder. Restart both desktop and its daemon after provisioning or changing pins. The page has no pin-edit/remove route and never trusts keys merely because `/keys` served them.

Open **Team → Sign in with your team**. Compare the displayed code in the external browser, choose the configured tenant and explicitly approve. **Check sign-in** completes the connection. The native process keeps the service bearer in memory; no bearer/OIDC token goes to the page, settings, logs or server database (the service stores only a token hash). Restart requires a fresh sign-in. Sessions last at most 15 minutes and may be renewed only within the IdP token lifetime. Sign out attempts server revocation and drops the local bearer even when the network fails. It does not remove team restrictions.

The desktop verifies DSSE Ed25519 signatures, exact issuer/tenant/domain, bounded fields, supported policy level, issue/expiry times and a durable sequence/hash watermark. A same-sequence changed payload, rollback, invalid signature or clock rollback is refused. Each covered daemon request re-reads the lease; the desktop publishes only a verified envelope. Leases last 15 minutes; disconnected clients honor the last verified lease only until expiry. No background sign-in or unrequested network is performed. Select **Refresh policy** to receive current restrictions.

Restrictions are additive to all existing local identity, path, network and human-approval controls:

| Feature | New effects checked |
| --- | --- |
| downloads | API probe/enqueue, browser handoff, inbox submit/retry, source inspect/submit/resume, queue start/configuration and background queue sync/new admission, desktop site/media inspect/select/start/recover |
| cloud_delivery | Cloud plan/submit/resume |
| workflows | Workflow run/resume and each new workflow document fetch/job admission |
| metadata_sync | Explicit metadata pull/write/delete in the desktop and service |
| provenance | Explicit client-reported assertion minting |

Approvals are classified from their actual stored kind: browser session/handoff/retry approvals require downloads; cloud delivery requires cloud_delivery; current local-sink workflows require workflows and downloads. Local artifact/organizer destructive approvals require no transfer entitlement. Unknown policy exceptions fail closed. New workflow run/resume requires workflows and downloads; its current Plan schema has no cloud sink. Denial, pause, stop, cancel, read and revoke stay available. Running transfers are not retroactively canceled; already-accepted provider-side imports cannot promise remote cancellation. Team true values never grant permission or bypass a local approval. CLI/MCP/API routes pass the daemon gate, while native browser handoff and background queue/workflow admission have explicit checks.

Shared metadata is an explicit pull/edit/push workflow. It is not a file sync service. Revision mismatch requires pulling and reviewing current data. Tombstones are durable and retries preserve the original idempotency key. The change view loads 100 entries at a time; selecting a record populates its current revision. Signed artifact reports are `client_reported`: a valid signature proves the team's assertion, not file safety or server-side inspection.

## Deployment

Use Node **24.22.1 or newer 24.x**. The runtime checks this requirement and uses Node's built-in SQLite. Install the exact lockfile with `npm ci --omit=dev --ignore-scripts`; `jose` is the only dependency. Keep this service separate from existing Zeus/Ogun stacks, their databases, native IPC and browser/credential paths. No production IdP or remote deployment is implied by the fixture tests.

1. Register a confidential OIDC application at your chosen identity provider. Enable authorization code, S256 PKCE and RS256 or EdDSA ID tokens. Register exactly `https://YOUR-TEAM-ORIGIN/auth/callback`. Obtain the exact issuer, client ID, supported client-secret authentication method and administrator subject/group mapping. The audience must equal the client ID. Pin all discovery/token/JWKS origins explicitly; redirects and arbitrary key URLs are refused.
2. Create a `zeus-team` system account, `/etc/zeus-team` (0700, owned by it), `/var/lib/zeus-team` (0700) and versioned source under `/opt/zeus-team/releases/`. Put the official Linux Node24 runtime under `/opt/zeus-team/runtime/` and point `/opt/zeus-team/current` at the reviewed version. Do not reuse another app's database or secrets.
3. Run `node scripts/provision-signing.mjs /etc/zeus-team https://YOUR-TEAM-ORIGIN TENANT KEY_ID` once. The generated private PEM remains on the server; distribute only the public `pin.json` after independently verifying its digest. For rotation, pre-provision the new public key alongside the old one on clients before switching the active service key. Keep old public pins until old leases have expired.
4. Copy `deploy/config.example.json` to `/etc/zeus-team/config.json`, replace placeholders and private-key paths, and set mode0600. Put `ZEUS_TEAM_OIDC_CLIENT_SECRET=...` in `/etc/zeus-team/oidc.env` with mode0600; do not put the secret in the JSON or command line. Do not commit this file. All members use issuer+subject identity; an email/label is not identity.
5. Validate with `ZEUS_TEAM_CONFIG=/etc/zeus-team/config.json` and the secret environment set, then `node scripts/check-config.mjs`. This parses keys/configuration without networking or mutating the database. Install `deploy/zeus-team.service`; start it only after configuration validates. Its listener is loopback18560. Check `/health` with the configured `Host` header. The service returns only its name/protocol version and logs no request content.
6. Under the VPS owner's existing nginx lock, add a dedicated HTTPS vhost and the location snippet in `deploy/nginx-location.conf`. Preserve the exact Host and never log authentication query strings, Authorization or Cookie headers. Validate `nginx -t` before reload. Configure certificates through the existing owner workflow. Do not expose18560 directly. There are no cross-origin browser API calls; OIDC uses the normal external browser.
7. Perform a real IdP acceptance: two tenants and roles, code comparison/approval, exact configured tenant, metadata CAS conflict, logout/revocation, disabled feature, expiry, restart, and signing-key rotation. Keep the service disabled if no real OIDC registration is available.

`teams[].features` seeds a new team only; subsequent restrictions are changed by an authenticated admin with `PUT /v1/teams/{id}/entitlements`, `If-Match: "REVISION"`, a stable `Idempotency-Key`, and the closed `features` body. Membership changes use the matching admin API. There is no native agent-facing admin/approval endpoint. Member revocation rejects sessions immediately. A disconnected client's restriction lease can remain valid for up to15 minutes; urgent local stop uses the existing kill switch.

Back up SQLite using its backup API or stop the service before copying the database and WAL/SHM consistently. Back up private signing keys separately. A restored old database may issue sequences below client watermarks; do not disable client replay checks to work around that—recover the latest database or advance the service lease sequence administratively after reviewing the backup. Rollback the versioned code symlink only with the service stopped, preserving current data/configuration. Service data has explicit caps (10k metadata records,50k changes/receipts,16MiB live metadata per tenant); capacity errors require an operator migration/archive, not silent deletion.

## Validation

`npm test` uses only ephemeral loopback OIDC/team servers and temporary SQLite. Tests cover PKCE/nonce/key rotation, one-use enrollment, tenant/role isolation, CAS/retries/restart/tombstones, session revocation/rotation, DSSE domain separation, audit chaining and metadata/feature boundaries. Native tests cover pinned verification, expiry, issuer/tenant/domain, replay/equivocation, persistence, unconfigured/invalid trust and metadata validation. Real IdP, TLS and external membership/signing rotation require deployment acceptance; they are not claimed from local tests.
