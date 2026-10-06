# ADR 0007: Human-only approvals

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |
| Specs affected | 07 (planned), 05 (planned), 02 section 10, 01 section 5 |

## Context

Zeus lets agents trigger downloads, some of which need the user's session or perform destructive file operations. The main threat is a prompt-injected agent forging or auto-approving consent. A hostile web page can also try to flood the user with requests until they click yes.

## Decision

1. An **approval** is a request for a human decision: kinds `handoff`, `session_grant`, `retry_escalation`, `policy_exception`, `workflow_plan`, `destructive_op` (01 section 5.3). Only an actor of kind `human` on a **trust surface** may decide.
2. The request shown to the human contains **daemon-computed facts only**: origin(s), counts, total bytes, destination, expiry, requesting client identity, plan hash. The only agent-supplied text is a clearly separated `agent says:` field, labelled untrusted. A **`request_hash`** (BLAKE3 over the JCS form of the facts) is computed by the daemon; a decision names the hash and is single-use, so a decision made for one request cannot be replayed on another.
3. Channels that can carry a human decision, in order of strength: terminal prompt read from the **controlling terminal** (`CONIN$` on Windows, `/dev/tty` on Unix, never stdin), the extension popup over native messaging (user-gesture attested, pinned extension), a future GUI, OS notification actions (convenience only, spoofable). There is no MCP, A2A or stdin path.
4. `approvals.decide` requires client kind `human` **and** channel evidence that the daemon can check: for the terminal, a peer process that owns a controlling terminal and is the installed `zeus` binary; for the extension, the pinned extension identity and gesture attestation of spec 02 section 10. If there is no tty and no extension, the approval stays pending until it expires (auto-deny).
5. **Honest limit.** An agent that runs as the same OS user with arbitrary shell access can read the same credential files and drive a pseudo-terminal. Software cannot fully prevent that. Mitigations: kind separation, tty and binary-identity checks, the request hash, rate limits (at most 50 pending, 10 decisions per minute), expiry, a kill switch, the audit log, and a later option to require an OS authentication prompt (Windows Hello, Touch ID, polkit) for `site` and `workflow` scopes. This residual risk is documented in spec 12.
6. Kill switch: one action (`daemon.kill_switch`, reachable from the popup, TUI and CLI) revokes all grants and pauses all jobs.

## Consequences

- The strongest property Zeus can claim is: *no agent-facing interface can approve anything*, and every approval is attributable and audited. The same-user shell-access attacker is out of scope for the claim and documented.
- UI work (terminal prompt, popup, later GUI) is part of the MVP, not polish.

## Alternatives considered

- MCP elicitation for approvals: the host agent controls it, so it is not a human-only channel.
- Interactive approval through stdin: an agent can write to stdin.
- No approvals, trust the agent: defeats the product's safety story.
