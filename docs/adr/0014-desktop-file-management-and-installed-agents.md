# 0014: Desktop file management, local uploads and installed agent connections

Status: implemented in the 0.0.1c development branch; acceptance recorded with the increment.
Owner direction: 27 October 2026.

Zeus is a desktop file manager as well as a download and upload manager. Folders
added for search remain read-only until the person separately enables file
management. Download folders preserve their existing permissions. Native operations
recheck canonical containment, refuse collisions and quarantined files, and run
large copies off the UI executor. Agents cannot call these native UI routes.

Local upload sources extend `cloud.plan` with `local_file`. The native file-index
settings publish included read roots in the private state file
`cloud-local-roots.json`; absence, invalid data and revoked roots fail closed.
The daemon canonicalizes the path, hashes the bounded regular file, binds the
private source identity and digest to the immutable plan, and requires the existing
human cloud-delivery approval. It stages a separate copy and verifies its digest
before a provider receives bytes. The original is never moved or removed. Scope
revocation is checked again when staging. Existing cleanup owns only staging.

Native Drive/Yandex/Baidu adapters and explicitly configured rclone remotes supply
capabilities. A configured remote must implement uploading. There is no claim that
every file host supports every transfer mode. Account registration, revocation and
approval decisions remain on the established human terminal surfaces. The desktop
adds plan review, requests for approval, status and pause/resume/cancel controls.

All four products can obtain completions through installed agents' own sign-ins.
Reserved `*.local-agent.invalid` HTTPS endpoints are internal adapter identifiers,
never HTTP destinations. The backend starts the selected executable, sends prompt
data through stdin, keeps existing consent/offline/tool gates, and enforces output
and time limits. Windows children belong to a kill-on-close job so a command
wrapper cannot leave the real agent running after cancellation. Account detection
is performed by the owning CLI; credential databases and browser profiles are not
read. API billing and subscription billing remain distinct. No direct Grok account
adapter is claimed; see INSTALLED-AGENTS.md for verified provider routes and limits.

No change to zeus-core, no new approval-deciding API, no public file upload proxy.

Local-path upload sources also check the journal’s quarantined job paths, both when planning and again when staging. A local path cannot be used to bypass the completed-job quarantine restriction.
