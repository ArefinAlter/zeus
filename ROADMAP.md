# Roadmap

Derived from `research.md` section 5.8 and the sequencing agreed during planning. Dates are targets from the start of public development, not promises. Every phase ends with benchmarks and a written threat-model review.

## Phase 0: contracts and skeleton (now)

- Normative specs 00 to 08 and 13 written (daemon API, MCP tools, approvals, workflow plan, artifacts); specs 09 to 12 **planned** (LLM provider, A2A, CLI, security invariants), designed in `docs/ARCHITECTURE.md` and `docs/adr/`.
- Three repositories scaffolded and compiling: `zeus`, `zeus-core`, `zeus-extension`.
- Research corpus drafted in `docs/research` (unverified until marked verified).

## Phase 1: MVP (months 0 to 3)

| Area | Deliverable |
|---|---|
| Engine (`zeus-core`) | Segmented HTTP(S) download, resume with `If-Range`, redb journal and crash-safe commit protocol, verification (SHA-256, BLAKE3), audit log, provenance manifest. |
| Browser | Chrome and Edge MV3 extension: handoff of failed downloads, consent-gated session release, URL refresh, in-browser fetch fallback. Native-messaging host. |
| Agents | MCP server with nine tools: probe, enqueue, status, wait, control, verify, session list, session request, files list. |
| Humans | CLI, minimal TUI, approvals on the terminal and in the extension popup, kill switch. |
| Safety | Human-only approvals, request-hash binding, path containment, quarantine marks, redacted logs, `cargo deny` gates. |
| Release | Public alpha with reproducible benchmarks (idle memory, memory with 10 jobs, throughput at 1, 4 and 16 segments). |

Exit criteria: handoff success of at least 85 percent on a 200-URL failure test set; crash-and-resume test passes (kill the daemon mid-download, restart, matching SHA-256).

## Phase 2: workflows and models (months 3 to 6)

- Workflow plans (spec 08): pagination, extraction, dry run, approval, checkpoint and resume.
- Sandboxed conversion pipeline, rules, de-duplication, LLM-assisted classification.
- Provider ladder and signed policy manifest (spec 09); `zeus auth` wizard.
- Daemon API frozen as v1.
- Firefox build of the extension.

## Phase 3: interoperability and GUI (months 6 to 9)

- A2A endpoint as a bridge process (spec 10), off by default.
- Tauri GUI beta as an additional trust surface (ADR 0009); MCP App views.
- Rules and organizer previews.

## Phase 4: 1.0 (months 9 to 18)

- Hardening, signed builds, packaging (winget, Homebrew, Scoop), SBOM and attestations.
- Optional closed-source layers kept out of the credential path (sync, team policy), if pursued.
- Additional sources (S3, Hugging Face-native, torrent) behind features.

## Not planned

Captcha solving, TLS fingerprint spoofing, reading browser cookie databases, DRM circumvention, telemetry.
