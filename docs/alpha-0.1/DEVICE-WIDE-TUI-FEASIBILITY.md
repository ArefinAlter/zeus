# Zeus TUI and device-wide transfers: feasibility

28 October 2026. This is a feasibility assessment, not a claim that a network takeover driver or Go application has shipped.

## Result

A separate Go terminal client built with Bubble Tea and Lip Gloss is feasible. It should control Zeus's existing Rust daemon rather than implement a second transfer engine. Users could ask the existing agent to download, upload, organize files or run a reviewed Git operation into an authorized folder, and monitor those tasks from either the desktop or terminal.

Silently replacing *every* download and upload on a device with file-aware parallel transfers is not feasible as a general feature. A cooperative application can hand Zeus a URL or an explicit source/destination plan. Arbitrary network traffic does not carry a reusable file-download contract: it may be encrypted, authenticated, multiplexed, negotiated, signed, compressed or subject to a server-specific multipart protocol. A packet filter can observe or shape traffic without gaining the application semantics needed to reproduce it safely.

## Integration matrix

| Source | Feasible integration | What Zeus can improve | Boundary |
|---|---|---|---|
| Browsers | Existing extension/native-host handoff, explicit capture and granted origin sessions | Resume, dynamic verified ranges, queues, media manifests, destination review | Browser consent and session scope remain authoritative; no cookie-store extraction. |
| Ordinary HTTP/HTTPS applications | Opt-in URL handoff, documented application hook, configured explicit proxy for cooperating clients | Range-capable immutable objects can use multiple connections, retries and integrity checks | CONNECT normally carries opaque TLS. A local root certificate is neither a universal solution nor part of this proposal. Certificate pinning, signed requests and HTTP/3 require application-specific handling. |
| GitHub Desktop / Git clone | A Zeus agent command invokes the installed Git with an explicit repository URL, destination and bounded arguments; optional Git-aware cache/transport integration | Scheduling, progress, cancellation, retry of supported phases; reusable object caches and independent Git LFS objects | Git negotiates a pack of repository objects. It is not one HTTP file per source file. Splitting a live pack arbitrarily cannot manufacture independent valid range requests. Git performs object/hash validation. Existing Desktop clones cannot be transparently adopted mid-transfer through its packets alone. |
| GitHub source archives / releases / LFS | Explicit archive/release/LFS-object handoff using their documented access mechanisms | Ordinary object ranges, hashing, caching and batch concurrency where supported | An archive is a snapshot, not a clone with history. LFS upload/download authorization and expiry are separate from Git authentication. |
| Windows Update / Store | Display documented BITS/Delivery Optimization status and use documented owner/admin policy controls | Scheduling/visibility/cost-aware policy, local cache or supported peer-delivery configuration | Windows already selects trusted update content and delivery. Zeus must not impersonate the update service or replace signed payload handling. User-owned BITS jobs can be managed through BITS APIs; other owners' jobs have authority constraints. |
| Other software updates | Vendor-specific supported adapters or handed-off payload URLs | Queueing, resume and verification while preserving vendor signatures | Proprietary updater authentication, delta formats, rollback and process installation remain vendor-specific. Downloading a payload is distinct from executing an installer. |
| Cloud/filehoster uploads | Native provider adapter or an explicitly trusted rclone backend | Provider-supported resumable sessions, multipart parts, retries, progress and checksums | A single arbitrary application POST cannot generally be split or resumed. The provider dictates part sizes, upload IDs, checksums, rate limits and commit semantics. No universal connector can promise all hosts. |
| SMB/SFTP/local transfers | Explicit file operation/source adapter | Bounded concurrent files, integrity, cancellation and durable operation records | Network protocol, filesystem semantics and server limits determine safe parallelism; HTTP ranges do not apply automatically. |
| Any device traffic | Optional observation and bandwidth scheduling through documented OS facilities | Per-app visibility and prioritization | Visibility or bandwidth control is not acceleration. Encrypted content, authorization and end-to-end semantics remain with applications. |

## Platform boundary

A cross-platform Go terminal client does not make system networking adapters portable. The control service would need a separate implementation and acceptance process for each operating system:

| Platform | Relevant boundary |
|---|---|
| Windows | WFP has user-mode management APIs as well as kernel filtering layers. Basic filter management does not inherently require a new driver; custom packet/stream processing can require a callout driver. Neither layer supplies a universal application-level file contract. [Microsoft WFP architecture](https://learn.microsoft.com/en-us/windows/win32/fwp/windows-filtering-platform-architecture-overview). |
| Linux | cgroup/BPF hooks can observe or change selected socket operations within their documented context. They are Linux-specific mechanisms, not a replacement for Git, HTTP or provider upload protocols. A production service needs its own privilege, lifecycle and kernel-support design. [Linux cgroup socket hooks](https://docs.kernel.org/bpf/prog_cgroup_sockopt.html). |
| macOS | Network Extension filters/proxies use Apple's capability and entitlement model. Developer ID distribution has specific system-extension entitlement values; a standalone Go executable does not remove the surrounding app/signing requirements. [Apple Network Extension entitlements](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.networking.networkextension). |

These are architectural options, not adapters implemented or installed by this alpha. The proposed first TUI can remain an ordinary daemon client while each network adapter is evaluated separately.

## Terminal architecture

Use a separate `zeus-tui` Go repository with a small authenticated client for the existing local protocol. Bubble Tea provides the event/update/view loop; Lip Gloss supplies theme/layout. Views should cover downloads, upload plans, files/search, inbox/packages, agent runs and connection status. Preserve stable row identities, virtualized visible windows, keyboard navigation, reduced animation and color/terminal fallbacks.

A connection goroutine translates daemon snapshots and durable event sequence numbers into model messages. Reconnect reads a fresh bounded snapshot, replays from the last accepted sequence and explicitly handles retention gaps. Progress updates are coalesced; durable state changes are not silently dropped. The client holds an owner-only credential, never prints original signed URLs/tokens and does not acquire arbitrary RPC permissions.

The terminal UI's chat uses the existing agent integration and its configured account/subscription or BYOK provider. The agent proposes a typed operation: repository/source, destination root, collision policy, resource limits and intended action. Daemon validation and existing human review apply. A model's text never becomes a shell command, filesystem grant or approval decision. Existing trusted review remains separate until a new TUI trust channel is deliberately implemented and accepted; agents cannot approve their own proposals.

For cloning, use the installed Git as a trusted helper with an argument vector (no shell), a controlled environment, a private/approved working directory, progress parsing, process-group/Job Object cancellation and bounded logs. Do not log credentials embedded in URLs, copy Git credential stores or claim Zeus's HTTP scheduler accelerated Git's negotiated pack. A first useful version can expose a normal, auditable clone task without making that claim.

## Suggested delivery sequence and acceptance

1. Build the terminal client against the existing daemon: authenticated connection, queue/actions, files/search, uploads, agent runs, reconnection/event gap handling. Validate with a fake daemon and real local daemon; no new transfer engine.
2. Add explicit `download`, `upload` and `clone` agent operations with typed plans and existing scoped filesystem authority. Test cancellation, collisions, revoked roots, credentials redaction and interrupted helper processes.
3. Add selected cooperative adapters (Git LFS/release assets, package managers with supported handoff, user-owned BITS jobs). Publish a capability matrix and measured results for each adapter.
4. Only if there is a concrete need, evaluate a separate opt-in OS observation/bandwidth service. A WFP driver adds kernel/signing/installation/upgrade obligations and does not solve application-level takeover. It is unnecessary for the first three steps.

Benchmark against each application's own native behavior using the same network/server, file or repository, cache state, CPU/disk, number of requests and retry policy. Measure elapsed time, bytes retransmitted, server errors, hash validity and resource use. More connections can reduce throughput or increase server throttling; no universal speed multiplier is defensible.

## Primary references

- [Bubble Tea](https://github.com/charmbracelet/bubbletea) and [Lip Gloss](https://github.com/charmbracelet/lipgloss): terminal event loop and styling.
- [Git pack protocol](https://git-scm.com/docs/gitprotocol-pack): negotiated object transfer and pack protocol.
- [Windows Filtering Platform](https://learn.microsoft.com/en-us/windows/win32/fwp/windows-filtering-platform-start-page): filtering facilities and layer boundaries.
- [BITS](https://learn.microsoft.com/en-us/windows/win32/bits/background-intelligent-transfer-service-portal): background uploads/downloads and owner-controlled jobs.
- [Delivery Optimization](https://learn.microsoft.com/en-us/windows/deployment/do/waas-delivery-optimization): supported Windows content delivery/cache/peer behavior.

No system proxy, root certificate, packet filter, driver, Windows update policy or third-party application configuration was installed or changed for this assessment.
