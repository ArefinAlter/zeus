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
| 05 | daemon-api | planned | Local JSON-RPC API of `zeusd` (transport, authentication, methods, subscriptions, IPC attach used by the host). |
| 06 | mcp-tools | planned | The `zeus_<verb>` MCP tool surface, resources, output and untrusted-content labelling. |
| 07 | approvals | planned | The human trust surface: approval flows, UI contract, grant management, policy defaults. |
| 08 | workflow-plan | planned | Declarative crawl and download plans, pagination, politeness, dry run. |
| 09 | llm-provider | planned | The BYOK provider ladder, budgets, what may enter model context. |
| 10 | a2a | planned | Agent card and task mapping for the A2A protocol. |
| 11 | cli | planned | Commands, exit codes, output formats, TUI. |
| 12 | security-invariants | planned | The consolidated, testable security invariants and the threat model. |

Documents 05 to 12 must reuse the vocabulary of 00 to 04 and reference sections rather than redefine anything.

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
| ID prefixes `job_ run_ gr_ ap_ sess_ conn_ chain_` and the `ev` event sequence | 00 section 5 |
| Native-message types | 02 section 6.2 |
| Grant scopes `once site workflow never`; source kinds `browser_handoff agent cli workflow` | 01 sections 5 and 3.3 |

## Change process

1. Edit the prose first, then the schema or data file, then the examples; run `npm run validate`.
2. A breaking change raises the protocol or format version named in the document (00 section 4) and is recorded in that document's versioning section.
3. Every document ends with an **Open issues** list. Resolve an issue by changing the text and deleting the entry in the same commit, or by promoting it to a dated decision in the document body.
