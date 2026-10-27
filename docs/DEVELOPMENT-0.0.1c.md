# 0.0.1c development increment

27 October 2026. Internal version `0.0.1-beta.1`. The published Latest installers remain 0.0.1b until a separate packaging and platform-acceptance run.

## Existing agent subscriptions

Providers includes Codex, Claude Code, Cursor and Antigravity installed-agent connections alongside API-key providers. Sign in using the owning CLI; the product does not extract IDE tokens or browser credentials. Claude and Codex subscription authentication is checked through their own status command. Cursor and Antigravity rely on their own cached CLI sign-in and model entitlement. User consent and offline controls still apply because a locally started agent may use a cloud model.

Requests use stdin and a fresh temporary working folder, bounded output and deadlines. Windows descendants are supervised with kill-on-close jobs. The adapter accepts final responses only and validates requested product tools against the declared table. Claude built-in tools are disabled; Codex shell and web tools are disabled in read-only mode. Cursor Ask runs with an app-created deny policy; Antigravity Plan is refused unless the owning CLI has toolPermission=strict. These are provider controls, not an OS isolation claim. CLI tools/configuration behavior for those two has protocol coverage but no live account acceptance on this machine.

Live acceptance: the common Windows transport completed a bounded Claude subscription request returning the expected constant, without project files. Codex is installed but the subscription sign-in check did not pass; no Codex inference was claimed. Grok has no verified direct consumer-account adapter. A Grok model may be selected through Cursor if that account exposes it. See [installed agents](INSTALLED-AGENTS.md).

Subscription prices are not API prices. Provider quotas, extra-usage billing and model availability remain controlled by the provider; app token accounting uses reported usage when available. No silent fallback from an installed-agent error to an API-key provider occurs.

## Zeus

Files can manage download folders and other folders separately granted Manage files access in Search and index settings. Search-only roots stay read-only for file operations. Added live paged folder browsing, file/folder copy jobs with progress and cancellation, and new folder, with canonical containment, collision and quarantine checks. Existing rename, move, trash and preview remain. Folder browsing reads disk directly and is independent of index exclusions and the search cap. Copies preflight at most 100,000 entries and 128 levels, refuse links/junctions, never overwrite destinations and leave sources unchanged. Completed destination files remain after a failed/cancelled copy; its incomplete current file is removed.

Uploads reviews local files, completed downloads and bounded public URLs against configured cloud accounts, requests the existing human approval, and shows engine progress plus pause/resume/cancel. Local files are hashed before review; execution snapshots a separate staged copy and checks that exact digest before provider disclosure. Scoped sources, changed bytes, approval requirements and original-file preservation have regression tests. Desktop account setup, disconnect and approval-review controls open a visible terminal running the bundled CLI against this installation. The GUI keeps its restricted identity and never supplies a confirmation, credential or decision; the person reviews and types at the established terminal trust surface.

Connectors use actual capabilities of Google Drive, Yandex Disk, Baidu Netdisk and explicitly configured rclone remotes. Coverage is extensible and is not a claim that every file host is supported. No changes to zeus-core. No live cloud-account upload was performed in this session; regression tests use a loopback mock destination.

The subscription connection test now bypasses the API-key store, including an uninitialized/locked encrypted fallback. Frontend type checking passes. Additional full-release checks are in progress.

Earlier increment: frontend tests and hosted build pass. Native daemon workspace Clippy and tests pass; all 257 specification examples validate. Browser acceptance with mock routes verifies the local-file review payload, approval request, and absence of any approval-deciding call. Native desktop workspace Clippy passes and its 129 tests pass (one subprocess fixture is ignored and exercised by its parent). The final installed-agent policy revision also has focused native regression coverage recorded in the operation report.

Local-path upload sources also check the journal’s quarantined job paths, both when planning and again when staging. A local path cannot be used to bypass the completed-job quarantine restriction.
