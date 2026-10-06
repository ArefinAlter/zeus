# ADR 0006: Apache-2.0 open core

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |

## Context

Zeus handles session cookies and runs as a long-lived local daemon. Users and extension-store reviewers must be able to audit it. The product is free; sustainability comes later from optional services.

## Decision

1. The engine (`zeus-core`), the app (`zeus`) and the extension (`zeus-extension`) are licensed **Apache-2.0**, "Copyright 2026 Zeus contributors". Apache-2.0 gives an express patent grant, which matters for a download-and-credentials tool embedded by others.
2. Contributions require a **DCO sign-off** (`Signed-off-by`). There is no CLA.
3. "Open core": optional paid layers (cross-device sync, team policy, enterprise controls) would be **separate, closed components** built on the public API. They are never in the credential path: nothing paid ever sees cookies, tokens or consent decisions, and the free product is complete for individual use.
4. No telemetry, ever, in any component of the open repositories.
5. Trademark: "Zeus" is a crowded name. A real clearance is required before any public launch (see research document 19). The licence grants no trademark rights.

## Consequences

- Anyone can fork, embed or audit. A competitor could build a closed product on the engine; the answer is the user's trust in the maintained original.
- Dependencies must be compatible with Apache-2.0; `cargo deny` enforces an allow-list (`deny.toml`).

## Alternatives considered

- AGPL or a source-available licence: stronger against hosted forks but a barrier for the agent-tool ecosystem and for embedding.
- MIT: simpler but lacks the patent grant.
