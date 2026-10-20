# Zeus download workspace

2026-10-16 · design proposal and interactive local prototype. Open [console/index.html](console/index.html) directly in a browser. All records and actions are simulated; this is not a running desktop app, daemon connection or approval authority.

The [2026-10-20 workflow research and platform design](client-workflows-and-platforms.md) extends this proposal with peer-to-peer clients, metadata search, disk analysis, native/terminal file managers, synchronization, processor targets, mobile execution and concrete designs for the remaining blockers. The live TUI now has search, stable selection and selected-row scrolling; the HTML prototype remains simulated.

## Product decision

Make the download queue the home screen. Keep the dense, sortable information people need: file, website, requester, state, saved bytes, total, speed, remaining time and queue. Put the agent's plan beside those files, with an explicit permission review between planning and execution. Chat is an optional way to propose work; it is not the primary monitor and cannot approve its own requests.

The [prototype](console/index.html) implements navigation, search and filters, per-file inspection, selection and batch pause/resume/cancel, URL import with deduplication, package assignment, queue settings previews, run-budget revisions, an approval review with an arming delay and typed code, and global stop/release. Every control updates only in-memory sample state. Reload resets it. Sample hostnames use reserved `example.org` domains.

Previews: [desktop queue](console/preview-desktop.png), [mobile queue](console/preview-mobile.png), [agent plan](console/preview-agent-run.png), and [approval review](console/preview-approval.png).

## Familiar conventions to keep

| Reference | Useful convention | Zeus decision |
|---|---|---|
| [IDM scheduler](https://www.internetdownloadmanager.com/support/schedulerD.html) and [queue controls](https://idm.internetdownloadmanager.com/support/idm-scheduler/idm_queues.html) | Named queues, start/stop windows and parallel-file limits. | Keep a visible queue on every row. Distinguish parallel downloads from per-file connections. A global stop remains separate from a schedule and never silently resumes work. |
| [JDownloader Packagizer](https://support.jdownloader.org/fr/knowledgebase/article/what-is-the-packagizer) | Rules associate imported links with package names, folders and properties. | Use a Link inbox before execution. Preview package/destination rules and collisions. Changing a rule does not silently rewrite existing queued jobs. |
| [GNU Wget overview](https://www.gnu.org/software/wget/manual/html_node/Overview.html) and [recursive download](https://www.gnu.org/software/wget/manual/html_node/Recursive-Download.html) | Scriptable URL intake and explicit traversal of linked resources. | Add URL-list input and bounded traversal to a future plan surface: origin allowlist, depth/page/file/byte/time limits, include/exclude rules and a dry-run preview. Keep resume/exit behavior consistent in CLI and GUI. Do not imply full wget compatibility. |

These sources support the conventions, not the proposed Zeus implementation. The agent-run layout and permission design below are product recommendations derived from Zeus specs 01, 05, 07 and 08.

## Workspace structure

- **Downloads:** all jobs across manual, browser, CLI and agent sources. Filters change the view, never authority. Selection does not change the active inspector's identity or automatically confirm destructive work.
- **Link inbox:** paste/import links, remove exact duplicates, inspect origins, group packages, pick a destination and preview collisions. No download starts merely because a link is discovered.
- **Agent runs:** the plan and its hash/revision, source sites, extraction method, expected and unknown sizes, stage progress, per-job ownership, retry reasons, limits and stop conditions. Show planned versus actual usage as separate values.
- **Approvals:** website, authenticated requester, exact files/operations, destination and duration in that order. State unknown sizes explicitly. Place the agent's text below the facts in an untrusted block. A changed revision clears confirmation input and restarts the input delay.
- **Queues and schedules:** named queues, priorities, file concurrency, speed ceilings, explicit timezone and start/stop windows. Show what happens at the stop boundary. Overnight windows and metered-network policy require daemon implementation before becoming live controls.
- **Inspector:** bytes durably saved, retry history and a concrete next action; on completion, hashes, source, requester, transforms, quarantine and provenance receipt. A completed state and a verified state must remain distinguishable.

## Agentic behavior

A run is a durable plan with bounded effects. Show `Discover → Preview → Review → Fetch → Verify → Organize`; unimplemented stages say so. Budget edits create a new plan revision and require new approval when effects widen. External-page text never becomes authority, a save path, a grant or an instruction by itself.

Prefer “The link expired. Refresh it from the website, then retry.” to a generic failed badge. For session access show exactly who asked and how long it lasts. For content changes show a before/after list and separate approval when required. For exhausted budgets show what was saved, what remains and which limit stopped the run. Do not turn a model's confidence score into a permission signal.

Packages are presentation/grouping objects, queues control scheduling, and runs carry execution/provenance. A file may belong to all three; these concepts should not be overloaded into one parent row. Use nested package rows and expandable run membership at larger scale, with virtualization and stable job IDs.

## Visual and interaction system

Follow the [brand product rules](../../../zeus-site/brand/BRAND.md): marble/ink base, ultramarine active progress, gold sparingly for ordinary product actions, olive success, terracotta error; text always accompanies state color. Use system UI fonts for a dense workspace, with restrained serif page titles. The approval dialog is neutral system typography, equal-size approve/deny controls, no artwork or promotional color.

Keyboard navigation, visible focus, responsive tables, readable empty/error states and reduced-motion support are mandatory. The prototype supports 390 px layouts through horizontal table overflow and stacked inspectors. Production tables should expose ARIA row selection without turning the entire table into an inaccessible custom widget. Announce state changes concisely; do not repeatedly read every progress update.

The prototype's bind code and hash are **illustrative**. Production must use spec07's actual JCS/BLAKE3 verification and authenticated trust channel. A web preview can never grant browser sessions. Prototype queue windows and run limits are also **previews**, not installed automation.

## Implementation path and acceptance

Use the daemon API as the single command/state source. A future Tauri desktop shell should have a typed IPC client, immutable state reducer, revision-aware approvals and an explicit capability map. Keep remote strings as text. Never derive trusted permissions from a display label or from web content. Virtualize long queues; coalesce progress to at most 10 UI frames per second; replay durable events and fetch snapshots on lag.

The current Rust TUI supplies a live paginated queue, source attribution, selected-job details, approvals pane, bounded redraws and legal state controls. The extension supplies a separate browser trust surface. The daemon workflow runtime is being validated independently. A production desktop app, queue scheduler, Link inbox backend and render-page capability remain implementation work; this prototype does not imply those controls are connected.

Acceptance scenarios: interrupted transfer resumes without losing saved bytes; stale approval cannot execute; unknown-size run stops at byte budget; global stop revokes grants and pauses work; release restores neither; canceled job leaves completed files intact; signed URL never appears unredacted; keyboard-only review cannot be triggered by type-ahead; a failed verification never receives a success badge; invalid URL lists make no network request; language and 200% zoom preserve all trust facts.
