# ADR 0011: zeus-core is shared with Ogun; Zeus keeps its consumer scope

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-06 |
| Decided by | Maintainer, 2026-10-06. Q5 (client kind) was delegated; the answer follows from the other decisions. |
| Specs affected | 00, 04 (written); 05, 07 (planned); new 13 |
| Counterpart | `ogun/docs/adr/0002-separate-product-on-zeus-core.md` |

## Context

The product discussion of 2026-10-06 described an agentic data environment: workflows, partitions and backfills, operators that run code (Python, SQL, dbt, OCR, LLM), assets, lineage, an ADE-style workbench, and a benchmark profile for data agents (Spider 2.0-DBT, ADE-bench). The maintainer decided that this is **not** the Zeus app. The Zeus app stays the consumer agentic download and file manager that the original research describes. The researched download mechanism in `zeus-core` is also the foundation of a second product: **Ogun** (repository `ogun`), a developer tool for AI, LLM and data engineers.

## Decision

1. **Zeus scope is unchanged.** The app keeps its roadmap: engine, journal, sessions and grants, extension and native host, approvals and trust surfaces, MCP, workflow plans (spec 08 as designed in ARCHITECTURE section 8, conversion included), its own LLM provider ladder (ADR 0008, spec 09), its own A2A (spec 10) and its own GUI (ADR 0009).
2. **`zeus-core` is a shared engine library with two first-party consumers.** Ogun embeds the `zeus-core` crates and runs its own `Engine` in `ogund`, wired with a session provider and proxy-fetch port that always answer "no session". `zeus-core` therefore keeps product policy out: no daemon, no UI, no product configuration. Its public API is reviewed as a contract.
3. **Browser sessions stay in `zeusd`.** Only `zeusd` holds session material and talks to the extension (ADR 0004, ARCHITECTURE section 1, rule 1). Ogun reaches browser-session sources only through `zeusd`, as Zeus jobs.
4. **New client kind `service` (Q5).** It serves first-party programs that use `zeusd` unattended. Ogun is the first. Its ceiling is the agent ceiling plus `files.roots.register` (register write roots it owns, inside the user's allowed roots). It is never human: `approvals.decide`, `sessions.revoke`, `config.set` and the kill switch are refused, exactly as for agents. Credentials are provisioned only by a human (`zeus clients add --kind service --name ogun`, admin). Session use by a service is bounded by grants that a human created on a trust surface, and every use is audited with the service's client id. Service clients get their own rate limits. Spec 05 section 5.3, `ClientKind`, `ClientRole` (new role `service`) and `policy::METHODS` change accordingly.
5. **Each product owns its approvals, LLM ladder, A2A and GUI.** Ogun has its own trust surfaces for its own domain and applies ADR 0007's rules. Session release is always a Zeus approval.
6. **New spec 13: artifacts and content-addressed store, as a `zeus-core` library.** It defines `art_` ids (new 00 section 5.1 prefix), the CAS layout (`objects/<algo>/<2>/<2>/<hash>`), and the artifact record (extending the spec 04 manifest record with parents, producing operation and verification state). It also defines import of a file from another store with its provenance. Each product runs its own store instance; the content hash is the cross-product identity.
7. **Shared code moves to `zeus-core` when a second consumer needs it.** Candidates: `zeus-llm` (ladder mechanics, PKCE, policy-manifest verification, spend ledger), `zeus-a2a` (card and task-mapping types), and the pure plan-IR parts of `zeus-workflow` (template language, selectors, pagination, budgets). Each move gets its own ADR. Configuration, credentials and state never move.
8. **Spec 00 prefix table:** reserve `art_`, and register Ogun's prefixes when Ogun spec O-00 is written.

## Consequences

- Specs 05 (service kind, `files.roots.register`, linking) and 07 (approvals raised by service-originated jobs) must reflect decisions 4 and 5 when segment S1 writes them. `docs/ARCHITECTURE.md` section 5.3 needs the `service` row first.
- `zeus-core` changes now affect two products. Breaking API changes need a note in both repositories.
- Research docs 17, 18, 23, 26 and 27 stay in the Zeus plan. Ogun writes deeper developer-focused companions (OR1 to OR4) that cite them.
- Ogun can start implementation once the engine passes crash-and-resume (segment I3), independently of the extension work.

## Alternatives considered

- **Workflows and operators inside `zeusd`:** rejected. Arbitrary code would run in the cookie-holding process, and the consumer app would bloat.
- **Ogun as a pure `zeusd` client with no embedded engine:** rejected by the maintainer. Ogun is a standalone developer tool built on the engine.
- **Reusing the `agent` kind for Ogun:** rejected. Audit would not distinguish Ogun's unattended runs from interactive agents, and there would be no separate limits.
- **Giving Ogun its own native host or extension:** rejected. Two processes would hold session material, and the browser would have two consent surfaces.
