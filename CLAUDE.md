# CLAUDE.md: orientation for coding agents

Zeus is a free, open-core (Apache-2.0), Rust, local-first session-aware download and file layer for AI agents. Product name and codename are both "Zeus". Pre-alpha.

> While `HANDOFF.md` exists at the repository root it describes the in-flight build and decisions already made; read it first and delete it (and this paragraph) before the repository goes public.

## Repositories, side by side

| Path | Role |
|---|---|
| `zeus/` (this repo) | App: `zeusd`, `zeus` CLI/TUI, MCP server, native host, workflows, LLM ladder, A2A types, `docs/spec`. |
| `../zeus-core/` | Engine library crates (`zeus-types`, `-control`, `-fs`, `-verify`, `-journal`, `-session`, `-http`, `-engine`). Path dependencies from this workspace, and the engine Ogun embeds: its public API is a contract with two consumers. Never edit it from here without being asked. |
| `../zeus-extension/` | MV3 TypeScript extension. |
| `../ogun/` | Ogun: separate developer platform (data mining, data engineering, agent benchmarks) built on `zeus-core`; reaches browser sessions only as a `service` client of `zeusd`. Boundary: `docs/adr/0011-ogun-built-on-zeus-core.md`. Never edit it from here without being asked. |

## Repo map

| Path | What |
|---|---|
| `crates/zeus-api` | JSON-RPC types, `ClientKind`, `Permission`, method policy table, NDJSON codec, endpoint and discovery, auth handshake, `Client` and `Router`/`serve_connection`, redacted `JobView`. The contract between all front ends and the daemon. |
| `crates/zeus-daemon` | `zeusd` binary; `Backend` trait + routes, human-decision rules (`approvals.rs`), provisioning, config. The engine adapter is not written yet. |
| `crates/zeus-mcp` | MCP tools (`zeus_<verb>`): catalogue, dispatch over any `Client`, resources, prompt. The rmcp transport binding is not written yet. |
| `crates/zeus-cli` | `zeus` binary, clap tree, exit codes, trust prompt, TUI (`src/tui`). |
| `crates/zeus-native-host` | `zeus-native-host` binary (framing, argv attestation, allow-list), host manifests install. The relay loop is not written yet. |
| `crates/zeus-workflow` | Plan IR v0, templates, validator, estimators. |
| `crates/zeus-llm` | Provider ladder, policy manifest, PKCE, spend ledger. |
| `crates/zeus-a2a` | Agent Card, job to task mapping. |
| `docs/spec/` | Normative specs 00 to 04 (written), 05 to 12 (planned) and JSON Schemas. Prose wins over schema. |
| `docs/ARCHITECTURE.md`, `docs/adr/` | Design and decisions. Section 5 to 12 of ARCHITECTURE are the input for the planned specs. |
| `docs/research/` | Research corpus. Owned by another workflow; do not edit. Unverified unless the front matter says `verified`. |
| `research.md` | Original market research (historical name "Haul"). |

## Commands

```powershell
# one cargo process at a time; the build cache sits next to the repos on any machine (jobs: 3 on 8 GB RAM, 8 on 32 GB)
$env:CARGO_TARGET_DIR = '..\zeus-build-cache\zeus'; $env:CARGO_BUILD_JOBS = '3'
# Windows without MSVC (see CONTRIBUTING.md): $env:RUSTUP_TOOLCHAIN = '1.99.0-x86_64-pc-windows-gnu'
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p zeus-cli -- --help
cd docs/spec/tools; npm ci; npm run validate     # spec examples are tests
```

## Constraints (do not violate)

1. **Approvals are human-only.** No agent-facing path (MCP, A2A, bridges) may decide an approval, create or extend a grant, or return session material. `zeus_api::policy::METHODS` is the single table of who may call what; keep it consistent when adding a method.
2. **Never read browser cookie stores** or any browser profile file (ADR 0004). Sessions come only through the extension and native messaging.
3. **Secrets** (cookies, tokens, keys, URL query strings) never reach logs, events, audit records, errors, test fixtures or LLM prompts. Use `redact_url`, zeroizing types, no `Serialize` on secret types. Leak tests use the marker `ZEUS-CANARY-`.
4. The daemon has **one listener**, the local IPC endpoint. HTTP surfaces are bridge processes (ADR 0010).
5. Rust: edition 2024, MSRV 1.90, `unsafe` only in isolated, commented platform modules (`#![forbid(unsafe_code)]` elsewhere), thiserror in libraries, anyhow only in binaries, tracing for logs, no `todo!()` or `unimplemented!()` (return a typed `NotImplemented`). Public items need doc comments.
6. Dependencies are declared once in the root `[workspace.dependencies]` (use `workspace = true`). Edit the root manifest with small targeted edits.
7. Tests must be hermetic (loopback only, `127.0.0.1:0`). No telemetry. No network at runtime except what a user action causes.
8. JSON on the wire: `snake_case` keys, enums as `snake_case` strings, instants RFC 3339 UTC with three fraction digits, durations in `_ms`, ids are prefixed ULIDs (00 conventions).
9. Do not run mutating git commands unless the user asks; the maintainer commits.

## Where to look first

- Adding an API method: `crates/zeus-api/src/methods/`, the `policy` table, the daemon handler, then docs (ARCHITECTURE section 5).
- Adding an MCP tool: `crates/zeus-mcp/src/{args,output,catalogue,server}.rs` and ADR 0003 (keep the tool set small; mutating tools take an `idempotency_key`; remote-derived strings go in `untrusted_*` fields).
- Changing the API types: regenerate the draft schema with `UPDATE_SCHEMAS=1 cargo test -p zeus-api schema` (a test fails on drift) and update ARCHITECTURE section 5.
- Changing a job state or error code: `docs/spec/01-domain-model.md` and `schema/job-state-machine.v1.json` first; the state machine is table-driven.
