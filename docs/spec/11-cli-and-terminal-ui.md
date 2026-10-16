# 11 — CLI and terminal UI

Draft 1 · 2026-10-16. Defines the `zeus` command surface and human terminal interface. The executable command tree in `crates/zeus-cli/src/cli.rs` and the contracts in 00, 01, 05, 06 and 07 are the source of vocabulary. This document does not make an unimplemented daemon method available.

## 1. Process and output contract

`zeus` is an authenticated daemon client. It MUST complete the mutual handshake in 05 before sending work or decisions. `zeus mcp` uses the agent credential; ordinary CLI commands use the CLI credential. Command names, labels or `--yes` never raise a credential's authority.

Data goes to stdout; diagnostics and progress go to stderr. `--json` produces one JSON document and conflicts with `--ndjson`; the latter emits one object per list entry. Errors include their stable 00/05 code and map to section 4. Output MUST redact URLs and secrets using 00. File names, requester text and server errors MUST pass the display sanitizer before terminal rendering. Success is reported only after a successful daemon response, never because a message was merely written.

Global flags: `--json`, `--ndjson`, `--quiet`/`-q`, `--no-color`, `--dry-run`, `--yes`, `--profile`. `NO_COLOR` disables color. The current parser accepts `--profile`, but profile-specific configuration loading is unfinished and MUST NOT be presented as working isolation.

## 2. Command catalogue

| Command | Purpose and binding |
|---|---|
| `get <URL>...` | `jobs.enqueue`; `-o/--output` is a destination directory, `-H/--header` is a restricted request header, `--cookie-from-extension` requests human-approved website access, `-w/--wait` waits for results, `--sha256` checks one URL, `--priority` and `--idempotency-key` are explicit controls. |
| `probe <URL>` | `jobs.probe`; returns size, representation, resumability and session requirements. |
| `ls [--state <STATE>] [--limit <N>]` | `jobs.list`, with a bounded page of at most 100 jobs. |
| `status [<JOB>]` | `jobs.get` when an ID is given, otherwise daemon status. |
| `wait <JOB>... [--timeout <SECONDS>]` | Repeated bounded `jobs.wait` calls. A timeout or Ctrl-C ends the wait, not the download. |
| `pause / resume / cancel / retry <JOB>` | The corresponding `jobs.control` action. Illegal transitions are errors. Cancel does not delete an already completed file. |
| `verify <JOB> [--sha256 <HEX>]` | Re-hash a completed file through `jobs.verify`. A mismatch is never a success. |
| `approvals list` | Pending requests visible to the client. |
| `approvals approve <ID> [--scope <SCOPE>]` | Fetch the full request, verify its binding, review it at the controlling terminal, then decide. The flag constrains the user's selection; it does not make the decision. |
| `approvals deny <ID> [--never]` | Review and deny; an explicit block is scoped to the exact origin. |
| `sessions list / revoke <ID>` | Inspect permissions without exposing cookies, or revoke through a human surface. |
| `audit tail [--limit <N>] / verify` | Read redacted records or verify their chain. |
| `daemon install` | Create first-run state and credentials via `zeusd init`. OS autostart registration is still a separate release task. |
| `daemon start / stop / status` | Spawn `zeusd run` and wait for authenticated readiness; request shutdown; inspect status. Starting does not silently create new credentials. |
| `daemon kill-switch / release` | Deliberately engage or release the persistent global stop at a controlling terminal. Release leaves jobs paused and grants revoked. |
| `native-host install / uninstall` | Register or remove browser host manifests for explicitly selected browser/extension identities. Development identity requires the development option. |
| `config get / set / path` | Read or change allowed settings, or show configuration/state paths. Sensitive policy changes require daemon-enforced human evidence. |
| `doctor` | Display stable diagnostic IDs and actionable fixes. |
| `mcp` | Run the MCP stdio service as an agent client. Only protocol messages appear on stdout. |
| `tui` | Live terminal queue and approval inbox, section 5. |
| `completions <SHELL>` | Print a shell completion script. |
| `auth` | Controlling-terminal setup for explicit local or remote model configuration, described below. Saving never probes a model. |
| `a2a --token-file <PATH> [--port <PORT>]` | Run the optional loopback A2A bridge using its dedicated agent credential and per-peer bearer tokens. |

`mcp --http` is a reserved opt-in bridge. Until Host/Origin validation and bearer-token authentication exist, the CLI MUST refuse that option; it MUST NOT quietly run stdio instead.

Cookie and Authorization header injection, cookie-file import, automatic approval, and bypassing site access controls are outside this command contract. Batch stdin input, recursive retrieval, queue schedule editing, conversion and organization commands are proposals until added to the command tree and backed by implemented daemon methods. Wget-style behavior is described in the product design document, not implied by accepting a URL.

## 3. Human terminal decisions

Approval decisions MUST follow 07: verify the full `ApprovalRequest`, reject unknown facts, recompute BLAKE3 over canonical facts, validate mirrored identity/revision/expiry/scope fields and the agent-text hash, and send the computed hash. Untrusted text is a separate labelled block. Typed-code requirements use the exact bind-code algorithm in 07; `os_auth` fails closed until implemented.

Input comes from `/dev/tty` or `CONIN$`, never a pipe. There is no `--approve`, environment bypass or `--yes` shortcut. The daemon independently verifies the executable, role and terminal evidence. The UI discards buffered terminal input after the 750 ms input delay in 07 and again before typed-code entry, using the controlling-terminal handle. This requires real-terminal validation on each supported platform. An unavailable or unverified terminal leaves the request pending. A changed hash/revision requires a fresh fetch and review.

A TUI key opens this same terminal review; selecting a row never approves it. The current implementation sends these decisions as `cli_interactive`, because the TUI is hosted by the CLI credential and actually opens the CLI review surface.

## 4. Stable exits

| Exit | Meaning |
|---|---|
| 0 | Successful command. |
| 1 | Internal error. |
| 2 | Usage or invalid request. |
| 3 | Daemon unavailable. |
| 4 | Unauthenticated or human interaction required. |
| 5 | Not found. |
| 6 | Conflict or illegal transition. |
| 7 | Timeout; work can continue in the daemon. |
| 8 | Unsupported capability/version. |
| 10–18 | Failed job: transient network, throttled, auth expired, URL expired, blocked, permanent, disk, integrity, policy denied, respectively. |
| 20 | Completed but quarantined. |
| 130 | Interrupted wait/client process. |

A multi-job wait reports the strongest relevant failure under the implementation's deterministic exit aggregation. A client interruption MUST NOT silently send cancel.

## 5. Terminal workspace

The table shows name, state, requester source, saved bytes/percentage and size. A detail pane shows source, job ID and the next recovery step. The approval inbox is visually neutral. Throughput samples are inferred from durable-byte snapshots; they are not a raw network-speed measurement and should be labelled accordingly.

Keys: `j`/down and `k`/up select; Tab changes pane; `p` pauses; `r` resumes/retries; `x` retries a scheduled retry now; `c` cancels; Enter in approvals opens review; `q`, Escape and Ctrl-C exit. Key-release and auto-repeat events do not trigger repeated actions. Use no more than 10 frames per second; `ZEUS_NO_ANIMATION` enables change-driven rendering. Entering and leaving raw/alternate-screen modes MUST restore the terminal on every exit path.

Snapshots are paginated and refreshed after notifications and periodically. A lagged stream invalidates the snapshot; no approval or state transition may be inferred from a missing event. The current TUI repairs display state by fetching authoritative snapshots, rather than claiming durable history replay. Connection loss exits with an actionable diagnostic.

## 6. Acceptance and known gaps

Required checks: parser and stable exits, JSON/NDJSON behavior, pipe input cannot approve, stale/altered/unknown facts cannot approve, typed-code mismatch cannot approve, canceled wait leaves the job active, terminal restoration on EOF/error/Ctrl-C, TUI action legality, event lag repair, Windows key-event handling and `NO_COLOR`.

Current implementation limitations requiring follow-up: real browser/terminal platform acceptance, daemon autostart registration, profile-specific loading, streaming progress independent of snapshot RPC latency, and exact spec06 MCP resource/pagination parity. These are not implied complete by this draft.

Executable registration is explicit: install `zeusd`, `zeus` and `zeus-native-host` together, then run `zeusd init` (also used by `zeus daemon install`). Startup only reads registrations; missing or changed pins disable human authority. After an upgrade, stop the daemon and run `zeusd trust-clients` at the controlling terminal. It holds the daemon lock, shows old/new canonical paths and BLAKE3 digests, requires fresh typed confirmation after the input delay, rehashes the installed files, atomically replaces private pins with a backup, and records intent/completion in the anchored audit chain. There is no noninteractive override. Deleting pins or automatically replacing a mismatched digest is not an approved recovery procedure. Real-terminal and installer upgrade acceptance remain release gates.

Browser-parent attestation currently recognizes machine-installed Windows browsers registered under HKLM App Paths and root-owned Linux installations. HKCU registrations cannot attest a browser because any same-user program can rewrite them. Per-user/portable Windows installations, unsupported Linux packaging and macOS fail closed; they need platform validation before trusted browser operations are supported.

## Implemented local bridges and model setup

`zeus a2a --token-file PATH [--port 4750]` runs the optional authenticated loopback bridge using its dedicated agent credential. See [the bridge profile](../../crates/zeus-a2a-bridge/README.md) for request shapes, limits and ownership. It does not start alongside the daemon automatically.

`zeus auth` opens a controlling-terminal menu for model configuration, ChatGPT/OpenRouter sign-in, saved ChatGPT account inspection and provider sign-out. Sign-in presents the one-use URL only at that terminal, polls the daemon's owned flow, and cancels on Ctrl-C; it never enables inference or changes spending settings. Sign-out distinguishes local credential removal from confirmed remote revocation. Live provider/account and real-terminal/platform acceptance remain release gates.

The configuration wizard supports local Ollama/OpenAI-compatible servers or explicit remote OpenAI-compatible/Anthropic API keys. Model and endpoint are explicit; remote price ceilings and daily/monthly caps are required. Keys use hidden terminal input and the daemon OS keyring. Local providers select `local_only`; remote providers select `metadata_only`. Saving requires a fresh typed confirmation and never probes or spends on a model. Selecting an OAuth account for inference and its model settings remains an API/configuration task.
