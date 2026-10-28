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

Actual native daemon plus Chromium acceptance passes21 workflow,15 source and23 library checks in isolated fresh profiles. Evidence is `D:/zeus-alpha/evidence/workflow-runs/20261029-052248-479493/native-ui/` and `20261029-051858-724383/{sources-ui,library-ui}/`. The workflow samples Zeus's own public download page; an earlier attempt against example.com correctly refused its changed page, which no longer contained links. Sources acceptance makes no account grants or live provider requests. Library acceptance registers known fixture bytes and leaves every destructive/organization approval pending. Restart checks verify no unapproved effect executes.

Screenshot review then found a compact library form extending beyond its card despite document-level containment. Grid columns now explicitly permit shrinking, direct children have zero minimum width, and controls/buttons remain within their panel. The strengthened final run checks individual control bounds at1440,960 and390 pixels; its result must be recorded separately before claiming that layout gate passed.

The optional Team page/service has its own15-check native policy/UI acceptance and10 native verifier/10 daemon guard tests, plus8 Node24 service fixtures. See TEAM-SERVICE.md. Live Dex enrollment, installed GUI checks, actual provider inference and Linux desktop acceptance are not inferred from these fixtures.
