# Zeus specifications

Normative contracts for Zeus, the session-aware download and file layer for AI agents. Three codebases implement them independently:

| Repository | Role |
|---|---|
| `zeus` | The app: daemon `zeusd`, CLI `zeus` and TUI, MCP server, native-messaging host `zeus-native-host`, workflow engine, LLM provider ladder, A2A. |
| `zeus-core` | The embeddable engine library: `zeus-types`, `zeus-control`, `zeus-fs`, `zeus-verify`, `zeus-journal`, `zeus-session`, `zeus-http`, `zeus-engine`. |
| `zeus-extension` | The Manifest V3 browser extension (Chrome and Edge first, Firefox build target). |

The documents use MUST, MUST NOT, SHOULD and MAY as defined in RFC 2119 and RFC 8174. If a document and its JSON Schema disagree, the prose wins and the schema is a bug. If a contract is wrong or incomplete, make the smallest consistent fix in the contract file and say so in the change description.

## Documents

| # | Document | Status | What it fixes |
|---|---|---|---|
| 00 | [conventions](00-conventions.md) | Draft 1 | Naming, protocol versioning and capability negotiation, identifier formats, time and units, canonical JSON (RFC 8785), logging and redaction rules, the error envelope, limits, localisation. |
| 01 | [domain model](01-domain-model.md) | Draft 1 | Job, JobSpec, Segment, ProbeResult, Validators, Origin, SessionGrant, Approval, Progress; the exact job state machine; error classes with classification and retry policy; the event catalogue. |
| 02 | [native messaging v1](02-native-messaging-v1.md) | Draft 1 | Extension to host to daemon protocol: framing and limits, handshake, handoff, session oracle, URL refresh, in-browser fetch proxy with credit flow control, approvals, events, keepalive, security. |
| 03 | [journal and commit protocol](03-journal-and-commit-protocol.md) | Draft 1 | The durable transfer journal (redb), write and commit ordering, every crash point, resume validation, finalization, recovery, invariants I1 to I12, model-check sketch. |
| 04 | [audit log and provenance](04-audit-log-and-provenance.md) | Draft 1 | Hash-chained audit log, per-file provenance manifest v0, PROV and Croissant crosswalk, OS provenance metadata and when not to write it. |
| 05 | [daemon API v1](05-daemon-api-v1.md) | Draft 1 | The local JSON-RPC 2.0 API of `zeusd`, the single contract for every front end: endpoints (named pipe and Unix socket, first instance, owner-only access, peer checks), newline framing and the JSON-RPC profile, discovery, start and shutdown, the mutual challenge-response handshake with test vectors, client kinds `agent`, `service`, `human`, `admin`, roles and permissions, the method table with MVP and later methods (including `files.roots.register` and `clients.*` for service clients), event subscriptions with replay and lag, idempotency and `replayed`, pagination, the JSON-RPC error mapping and the `api.*` code registry, limits, rate limits and audit, and the `nm.frame` relay for the native host. |
| 06 | [MCP tools v1](06-mcp-tools-v1.md) | Draft 1 | The MCP surface for agents, targeting MCP 2026-07-28 and serving 2025-11-25 and 2025-06-18 clients: which MCP features Zeus relies on and which it never uses (elicitation, sampling, roots, tasks); stdio and the loopback HTTP bridge (Host, Origin, bearer token); the nine `zeus_<verb>` tools with model-facing descriptions, input and output schemas, annotations, idempotency keys, dry run and error results with recovery hints; untrusted-content labelling (`untrusted_*` fields); the long-running pattern (`zeus_wait` with a cursor, progress notifications, resource subscriptions); `zeus://` resources, the `rescue-failed-download` prompt, the later tools and the token budget. |
| 07 | [approvals, trust surface and consent](07-approvals-trust-surface-and-consent.md) | Draft 1 | The human-only approval protocol: the ApprovalRequest with daemon-computed facts per kind, agent text as a separate untrusted field, the request hash (JCS + BLAKE3) and bind code, revisions and batching, the decision and its validation order, decision records with channel evidence, grant lifetimes, invariants, renewal, revocation and the never list; the channels (terminal on `CONIN$` or `/dev/tty`, extension popup, later GUI, notifications as pointers), client classification and binary verification, confirmation levels, the same-user residual risk; the kill switch; caps, back-off and other anti-fatigue rules; UX rules, example screens and localisation; events, audit and error codes. |
| 08 | [workflow plan v0](08-workflow-plan-v0.md) | Draft 1 | The workflow plan IR v0 (sources, pagination strategies, extraction with CSS, JSONPath and regex, filters, fetch policy, mandatory budgets, post-processing including conversion, sinks, stop rules), its normal form and plan hash, the template language and query-language limits, validation against live page samples, the plan, dry run, approve, run lifecycle and run states, robots.txt and politeness, checkpoints and resume, drift detection, the dry-run and run reports, three example plans, and the portable core Ogun reuses (ADR 0011). |
| 09 | [LLM provider](09-llm-provider.md) | Draft 1 | The BYOK provider ladder, budgets, what may enter model context, the signed policy manifest. Design: [ARCHITECTURE section 9](../ARCHITECTURE.md), [ADR 0008](../adr/0008-byok-provider-ladder.md); draft types in `crates/zeus-llm`. |
| 10 | [A2A bridge](10-a2a.md) | Draft 1 | Agent card and task mapping for the A2A protocol. Design: [ARCHITECTURE section 10](../ARCHITECTURE.md), [ADR 0010](../adr/0010-one-local-listener-http-bridges.md); draft types in `crates/zeus-a2a`. |
| 11 | [CLI and terminal UI](11-cli-and-terminal-ui.md) | Draft 1 | Commands, exit codes, output formats, TUI. Design: [ARCHITECTURE section 11](../ARCHITECTURE.md); draft command tree in `crates/zeus-cli`. |
| 12 | [security invariants](12-security-invariants.md) | Draft 1 | The consolidated, testable security invariants and the threat model. Design: [ARCHITECTURE section 12](../ARCHITECTURE.md). |
| 13 | [artifacts and content store](13-artifacts-and-content-store.md) | Draft 1 | The `zeus-core` artifact store shared by Zeus and Ogun ([ADR 0011](../adr/0011-ogun-built-on-zeus-core.md)): the artifact record (`art_` ids, SHA-256 and BLAKE3, origin, operation, lineage, verification, retention) and its crosswalk to the 04 manifest; the CAS layout `objects/<algo>/<2>/<2>/<hex>`, ingest with crash points, materialization (reflink, hardlink, copy); the metadata index and its single writer; reachability, collection and quotas; import between stores; integrity repair; deduplication scope; invariants A1 to A15. |

Specifications 00–13 are written. The new drafts 09–12 and existing contracts still require review against implementation. Draft status describes a contract; implementation and release evidence are tracked separately. Prose and canonical schemas must agree before an API freeze.

## Machine-readable contracts

All schemas are JSON Schema draft 2020-12 under [`schema/`](schema/), with `$id` of the form `https://zeus.invalid/schema/<name>`.

| File | Describes |
|---|---|
| [`common.schema.json`](schema/common.schema.json) | Identifier patterns, timestamps, byte counts, origins, error class and envelope. |
| [`domain.schema.json`](schema/domain.schema.json) | Job, JobSpec, ProbeResult, Progress, SessionGrant, Approval and every event. |
| [`job-state-machine.v1.json`](schema/job-state-machine.v1.json) | The state machine as data: states, actors, every transition with trigger, guard and events. The transition table in 01 is generated from it. |
| [`native-messaging.v1.schema.json`](schema/native-messaging.v1.schema.json) | Every native-messaging message, per direction, and the host manifests. |
| [`journal.v1.schema.json`](schema/journal.v1.schema.json) | Logical form of the journal records. |
| [`audit-record.v1.schema.json`](schema/audit-record.v1.schema.json) | One audit log line. |
| [`provenance-manifest.v0.schema.json`](schema/provenance-manifest.v0.schema.json) | Manifest header and file records. |
| [`daemon-api.v1.schema.json`](schema/daemon-api.v1.schema.json) | Every daemon API method's `<Name>Params`, `<Name>Result`, `<Name>Request` and `<Name>Response`, the JSON-RPC envelopes, notifications (`events.event`, `events.lagged`, `daemon.stopping`, `nm.frame`), the discovery record and the shared views (`JobView`, `NewJob`, `ClientRecord`, `ServiceRoot`) (05). |
| [`mcp-tools.v1.schema.json`](schema/mcp-tools.v1.schema.json) | Inputs, outputs and annotations of the MCP tools, `ToolError`, the tool definition and call result profile, progress notifications, the `zeus://` resource contents and the prompt arguments (06). |
| [`artifact.v1.schema.json`](schema/artifact.v1.schema.json) | Artifact record, `CasObjectRef`, `ImportRecord` and the store manifest `store.json` (13). |
| [`workflow-plan.v0.schema.json`](schema/workflow-plan.v0.schema.json) | The workflow plan (strict) and its portable parts, `Diagnostic`, `PageSample`, `DryRunReport`, `RunReport`, `Checkpoint`, `ItemRecord` (08). Reference normal form and hash: `tools/plan-normalize.mjs`. |
| [`approvals.v1.schema.json`](schema/approvals.v1.schema.json) | `ApprovalRequest` with its per-kind `RequestFacts`, `ApprovalDecision`, `DecisionRecord` with `ChannelEvidence`, `GrantRecord` invariants, `KillSwitchRequest` and `KillSwitchResult` (07). ApprovalRequest examples carry the fence flag `hash`, which recomputes the request hash. |
| [`export-mappings.schema.json`](schema/export-mappings.schema.json) | Shapes of the PROV-JSON and Croissant exports. |
| [`redaction-vectors.v1.json`](schema/redaction-vectors.v1.json) | Normative test vectors for `redact_url`. |

## Checking the documents

The examples in these documents are tests. [`tools/`](tools/) holds a small Node program (dependencies: `ajv`, `ajv-formats`, `@noble/hashes`, `canonicalize`; development only):

```
cd docs/spec/tools
npm install
npm run validate          # validates every JSON example against the schemas (strict mode), recomputes
                          # the audit hash chain, checks the redaction vectors, the state machine
                          # (reachability, liveness, event names), vocabulary coverage and cross references
npm run gen:state-table   # regenerates the generated tables in 01-domain-model.md from the JSON file
```

Fence conventions in the markdown: a `json` or `jsonl` block carries `validate=<schema-name>#/$defs/<Def>` (and `chain` for an audit chain), or `fragment` for a deliberately partial excerpt. Any other `json` block fails the check. CI should run `npm run validate`.

## Vocabulary quick reference

Defined once, in the document named; other documents must not redefine it.

| Vocabulary | Where |
|---|---|
| Job states `queued probing awaiting_approval awaiting_session downloading paused retry_scheduled verifying finalizing completed failed canceled` (and the `quarantined` flag on `completed`) | 01 section 6 |
| Error classes `transient_network throttled auth_expired url_expired blocked permanent disk integrity policy_denied` (job) plus protocol classes | 00 section 9, 01 section 8 |
| Event names | 01 section 9 |
| ID prefixes `job_ run_ gr_ ap_ sess_ conn_ chain_ art_ plan_ cli_` and the `ev` event sequence | 00 section 5 |
| Client kinds `agent service human admin`; roles `cli tui gui native_host mcp a2a service embedded test`; permissions; `api.*` error codes | 05 sections 7 and 11 |
| Run statuses `awaiting_approval running paused completed stopped budget_exhausted failed canceled`; pagination strategies `rel_next page_param offset_limit cursor next_selector infinite_scroll date_range`; diagnostic codes `plan.*` | 08 sections 7.5, 4.6 and 6.3 |
| Object states `present absent damaged`; retention `keep evictable`; origin kinds `remote derived local`; verification `verified unverified failed` | 13 sections 6.4, 9.1, 4.2 and 4.4 |
| Native-message types | 02 section 6.2 |
| Grant scopes `once site workflow never`; source kinds `browser_handoff agent cli workflow` | 01 sections 5 and 3.3 |

## Change process

1. Edit the prose first, then the schema or data file, then the examples; run `npm run validate`.
2. A breaking change raises the protocol or format version named in the document (00 section 4) and is recorded in that document's versioning section.
3. Every document ends with an **Open issues** list. Resolve an issue by changing the text and deleting the entry in the same commit, or by promoting it to a dated decision in the document body.
