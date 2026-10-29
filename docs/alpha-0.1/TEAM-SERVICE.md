# R06 · optional team metadata, SSO and signed restrictions

The alpha implements a separate Node24 team service and a native Team page. Service deployment details and the exact restriction map are in [services/team-alpha/README.md](../../services/team-alpha/README.md). No browser-session reader, consumer-chat proxy or shared transfer credential path is involved.

The service provides OIDC authorization-code/PKCE/nonce sign-in with explicit one-use device approval; tenant and role isolation; metadata CAS, exact retry receipts, stable server snapshots and tombstones; session rotation/revocation; distinct Ed25519 DSSE restriction/provenance domains; and a hash-chained audit trail. Shared records are limited to tags, queue presets and workspace labels. Signed reports are explicitly client-reported digests. No file contents or arbitrary remote actions are synchronized.

Native trust is explicitly provisioned outside the webview as origin+tenant+public-key pins. Runtime tokens remain in native memory. Both desktop and daemon use the same strict verifier and durable replay floor; unconfigured installations retain existing behavior, while invalid/expired configured policy holds covered new effects. Browser handoff and background queue/workflow admission have explicit guards in addition to JSON-RPC dispatch. Approval classification uses stored approval kinds, so local organizer/file approvals do not require cloud entitlement. Human approval and all existing local authorization checks remain mandatory.

Running transfers are not retroactively terminated. Provider-side imports already accepted by a remote provider cannot promise remote cancellation. Key changes require trusted redistribution and process restart. Leases last15 minutes; revocation of a remote membership is immediate at the service, while an offline native installation can use its still-valid restriction lease until expiry. This is not an enterprise boundary against the local administrator replacing configuration or binaries.

Validation on29 October2026:

- Eight actual Node24 loopback tests pass: PKCE/nonce/key rotation, one-use enrollment, tenant/role isolation, CAS/retry/restart/tombstones, session rotation/revocation, policy/provenance separation, audit chaining, metadata allowlists, disabled-sync reads and absence of a remote approval endpoint.
- Ten native Windows desktop team tests pass: service-produced DSSE interoperability, issuer/tenant/domain/signature/time/replay checks, same-sequence equivocation, persisted restart behavior, frequent-check clock persistence, bounded no-follow reads, atomic publication, failure after watermark persistence, and unconfigured/metadata boundaries.
- Desktop TypeScript/build and59 frontend tests pass with the Team route integrated.
- Ten native Windows daemon team tests pass, including actual human/MCP router gating and stored approval-kind classification. Desktop strict Clippy passes; the coordinated daemon strict gate also covers R05 source changes.
- Fifteen fresh-profile native UI/CLI acceptance checks pass, including actual network denial before a request, accessible read paths, restart replay floor, expiry/newer lease, downloads allowed while cloud is restricted, no JavaScript errors and contained Team panels at1440×900,960×600 and390×740. Evidence: `D:/verne-build/0.1/evidence/zeus-team/native-20261029-051714` (test-only profile, never publish its generated IPC credentials).
- Offline provisioning/preflight verifies create-new signing keys, refusal to overwrite, and valid configuration without network or database mutation.

The source/deployment archive is `zeus-team_0.1.0-alpha.1.tar.gz` (SHA256 `4e5aca4f91fe7bad9b235aec097590a122b46066892c943ffd3c12899c6ecf0d`), containing service source, deployment templates, tests and pinned pure-JS jose. Node24 Linux runtime and actual deployment secrets are external. The owner selected the existing Ogun Dex issuer and owner account for deployment. The rollout coordinator owns the new confidential client, exact owner subject mapping, dedicated HTTPS origin and private keys; this acceptance run did not contact a live IdP or make a human approval. Live interactive sign-in and real tenant/signing-key rotation remain deployment acceptance. No production credentials are included in the archive.


## VPS deployment ·29 October2026

The service is active at `https://zeus-team.103.191.50.32.sslip.io`, backed by the existing `https://ogun.103.191.50.32.sslip.io/dex` issuer. A separate confidential `zeus-team` client has the exact `/auth/callback` redirect. The unique existing owner subject is the sole initial Verne administrator; the sample account has no team membership. Mapping uses Dex's version-pinned local subject encoding, not email or display-name matching.

The rollout installed the verified service archive and Node24.22.1 in dedicated directories, private server keys/configuration, a restricted service user, a loopback listener and a dedicated TLS vhost. nginx changes were serialized with the machine's existing lock and passed configuration validation. Dex was recreated with its previous configuration preserved for rollback. No unrelated stack or private desktop profile was changed.

Live checks pass public/private health, OIDC discovery, PKCE/client/callback redirect, the actual Dex login page and the original Ogun sign-in-provider endpoint. The temporary enrollment was canceled. No owner password, login, device approval, membership rotation or production two-tenant session was exercised.

Only `zeus-team-verne-public-pin.json` is distributed to clients; its SHA256 is `8fc1271c928aecbdd099959c1cd30677f4adafbeaab641d9bbb477f6936d470d`. Installing this optional pin at `<ZEUS_STATE_DIR>/team/pin.json` and restarting desktop/daemon enables team restrictions. Covered new operations wait for a valid signed lease until the owner completes **Team → Connect**. Unconfigured installations keep local mode. Server private keys and the OIDC client secret remain on the VPS.

Deployment receipt: `D:/verne-build/0.1/evidence/zeus/team-deployment/activation-receipt.json`; source `299d910c4018218928845cb0144088b6e46fd65a`. Existing Dynodoc API/database/web source is unchanged by its desktop alpha, so no Dynodoc server migration is necessary for this release.
