# Native desktop integration · 0.1.0-alpha.1

29 October2026. This checkpoint connects the desktop to its own daemon for workflow runs, connected sources and the artifact library. It retains human approvals and uses the app credential rather than terminal credentials. It is an implementation/acceptance record; final installer publication and VPS rollout are separate receipts.

## Executable paths

- **Workflow runs:** create and edit a bounded public-page plan, validate selectors against real page samples, save the exact normalized plan, dry-run it, request human approval, inspect reports, pause/resume/cancel valid states and export a manifest. Failed list requests retain loaded rows and show the error. Reopening a saved plan requires validation and a matching dry-run again. Existing authenticated rendering, scroll and conversion exclusions remain explicit.
- **Connected sources:** select an account explicitly granted to this installation, inspect an immutable S3/Hub/torrent target, review limits and submit/control its operation. Account setup/revocation opens a closed verified-human terminal action; there is no webview grant, secret-entry or arbitrary-command route. Source implementation, fixtures and limits are in SOURCE-ADAPTERS-PLAN.md.
- **Library and rules:** page artifact records, read metadata and lineage, hash/register a local file within daemon roots, preview retention/materialization/tombstone effects and follow approval-bound deterministic organization/undo. Read-only operation status is owner/root scoped and survives restart. Current roots are canonicalized before comparing stored Windows operation paths, preventing false missing-operation errors caused by verbatim prefixes.
- **Installed agents:** five official CLI presets, including Grok ACP, expose discovery and an explicit refresh. Detection does not assert sign-in or subscription entitlement. Authentication remains in each official CLI; see ../acceptance/grok-subscription.md.

ADRs0017/0018 record the native workflow/file capability additions and narrow migration of only unchanged default app credentials. Direct approval decisions, account grants and root registration remain unavailable through these desktop route tables. The organizer preview always forces a dry run even if caller input asks otherwise.

## Validation

The combined native daemon suite passes208 tests; API90, MCP49, focused source CLI2 and source-library13 tests pass. Source all-feature/all-target and combined daemon/CLI/MCP strict Clippy pass. Desktop app29, agent65 and integration14 tests pass (two environment-dependent agent cases remain ignored), with strict desktop checks. The frontend passes59 tests, TypeScript and desktop/harness builds. Browser extension321 tests and Chromium/Firefox packages pass at the alpha version.

Actual native daemon plus Chromium acceptance passes24 workflow,18 source and26 library checks in isolated fresh profiles. Evidence is `D:/zeus-alpha/evidence/workflow-runs/20261029-052708-544848/{native-ui,sources-ui,library-ui}/`. The workflow samples Zeus's own public download page; an earlier attempt against example.com correctly refused its changed page, which no longer contained links. Sources acceptance makes no account grants or live provider requests. Library acceptance registers known fixture bytes and leaves every destructive/organization approval pending. Restart checks verify no unapproved effect executes.

Screenshot review then found a compact library form extending beyond its card despite document-level containment. Grid columns now explicitly permit shrinking, direct children have zero minimum width, and controls/buttons remain within their panel. The strengthened final run passes individual control-bound checks at1440,960 and390 pixels. The390-pixel library screenshot was inspected after the correction.

The optional Team page/service has its own15-check native policy/UI acceptance and10 native verifier/10 daemon guard tests, plus8 Node24 service fixtures. See TEAM-SERVICE.md. The actual fifteen-tool stdio MCP scenario passes20 checks; the local synthetic-provider/native UI scenario passes43 checks across both wire formats, including consent, access controls, cancellation and key non-disclosure. Evidence is `D:/verne-build/0.1/evidence/zeus/mcp-agent/20261029-054002/` (MCP) and `20261029-054412/` (agent). The latter uses a unique scratch profile and a corrected explicit fifteen-tool allowlist. The test harness now registers its shutdown completion listener before requesting shutdown, eliminating an exit-response race.

The live team HTTPS/PKCE redirect reaches the configured Dex login page; the test enrollment was canceled without sign-in. Installed GUI checks, actual owner sign-in/provider inference and Linux desktop acceptance are not inferred from these fixtures.


## Windows package acceptance

The optimized runtime, HTTP bridge and desktop installer build successfully from executable source `299d910c4018218928845cb0144088b6e46fd65a` and shared core `a944e43035e9de9f5871ec6afd10c036e13457ad`. The final portable executable SHA256 is `4f8192faca237cab194c086a732252d7258d97b66092f182095c1fd4ced3d32c`.

Actual Windows/WebView2 acceptance passes41 checks: bundled daemon supervision, exact2 MiB transfer bytes/SHA256,15-tool MCP handshake, Runs/Sources/Library/Team routes, local default mode, five CLI presets, seven views through native1280→800→390→1280 window resizing, and cancellation of a live column drag. No page exceptions or human approvals occurred. Evidence and screenshots: `D:/verne-build/0.1/evidence/zeus/packaged-1793231955642/`. The first two harness attempts could not start WebView2 debugging while overriding the process's generic HOME/USERPROFILE; using the proven app-specific profile, APPDATA/LOCALAPPDATA and fresh WebView2 folder succeeded with the same unchanged executable.

The generated NSIS manifest matches all six executable/license payload entries and its compiler output hash matches the distributed installer. Installation defaults to the per-user Programs folder, separate from CLI state. The installer was neither executed into the owner's profile nor independently unpacked; portable execution and compiler-manifest parity are the acceptance boundaries. Receipt: `D:/verne-build/0.1/evidence/zeus/installer/payload-hashes.json`.
