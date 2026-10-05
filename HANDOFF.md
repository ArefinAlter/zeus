# Zeus: session handoff (written 2026-10-05, mid-run)

This file is for the next Claude Code session (or the user) resuming the Zeus build. It is ordered by priority. Read section 0 and P0 first; everything else can be consulted on demand. It contains local absolute paths and a transcript location, so delete or trim it before any repo is made public.

---

## 0. Snapshot: where things stand

| Item | State |
|---|---|
| User request | Research the idea in `research.md`, write markdown research docs on **all download standards, mathematics, protocols, cutting-edge research, agent/AI protocols, papers (arXiv)**; initialise the **software repo, extension repo and engine-core repo**; create remote repos (public/private as needed; extra repos on `F:\`); **commit and push**. "Do as much as possible in one pass." |
| Decisions the user made | GitHub auth: install `gh` + web login (done). **Naming: "Zeus everywhere"**. **Visibility: all private for now**. (Details in P4.) |
| Remote repos | **Created, private, empty, `origin` set locally:** `ArefinAlter/zeus` (local `F:\zeus`), `ArefinAlter/zeus-core` (`F:\zeus-core`), `ArefinAlter/zeus-extension` (`F:\zeus-extension`). |
| **Pushed to GitHub** | **NOTHING.** `F:\zeus` has one local commit (`0750c93 Initial commit`, README stub + .gitattributes); `research.md` is **untracked**. `zeus-core` and `zeus-extension` have **zero commits**. |
| Background workflows | Two were launched in the previous session and were still running when this was written: (A) research corpus `wf_2d6c2744-784`, (B) scaffold `wf_115d8706-116`. They may have stopped when the session ended. See P0.1 to check and P1 to relaunch only what is missing. |
| Rust/Node/gh | Rust 1.88 (MSVC), Node 22.14, npm 11 (no pnpm), git 2.46, `gh` 2.102 installed user-scope, logged in as `ArefinAlter` (scopes `repo`, `read:org`, `gist`). Machine: Windows 11, **8 CPUs, 7.8 GB RAM** (builds must be throttled; see Appendix A). |

### What exists on disk right now

`F:\zeus\docs\research\` (research workflow A; agents write directly to disk):

| File | State at snapshot |
|---|---|
| 01-http-download-semantics.md | research written (~14.7k words); **verify pass not confirmed** |
| 02-transport-http2-http3-quic-congestion.md | research written (~13.8k words); verify not confirmed |
| 03-other-transfer-protocols-and-sources.md | research written (~13.5k words); verify not confirmed |
| 05-math-scheduling-reliability-and-estimation.md | research written (~19.5k words); verify not confirmed |
| 06-integrity-chunking-dedup-delta-and-erasure-coding.md | research written (~15.4k words); verify not confirmed |
| 04-math-throughput-concurrency-and-congestion.md | research agent started, file not yet on disk |
| 07 browser-session-handoff, 08 browser-download-internals, 09 auth-bot-defenses, 10 mcp-protocol, 11 a2a | research agents started, files not on disk yet |
| 12 to 28 (see list in P1.A) | not started (queued behind the concurrency cap of 6) |
| 00a/00b claims audits of `research.md` | not seen in the journal; unknown |
| README.md (index), references.md | not written (final index stage) |

Front matter of every doc says `status: draft` until its verifier flips it to `verified` / `verified-with-caveats`. **Treat any doc still saying `draft` as unverified** (citations and maths not yet machine-checked).

`F:\zeus\docs\spec\` (scaffold workflow B, kernel stage finished): `README.md`, `00-conventions.md`, `01-domain-model.md`, `02-native-messaging-v1.md`, `03-journal-and-commit-protocol.md`, `04-audit-log-and-provenance.md`, and `schema/` (`common`, `domain`, `native-messaging.v1`, `journal.v1`, `audit-record.v1`, `provenance-manifest.v0`, `export-mappings`, `job-state-machine.v1.json`, `redaction-vectors.v1.json`) plus `tools/` (validation scripts: `validate-spec.mjs`, `gen-state-table.mjs`, `redact-url.mjs`; its `node_modules/` is gitignored via `tools/.gitignore`). Specs 05 to 12 were being written (05 daemon-api, 06 mcp-tools, 07 approvals, 08 workflow-plan, 09 llm-provider, 10 a2a started; 11 cli and 12 security-invariants queued). **Check which exist.**

`F:\zeus-core` and `F:\zeus-extension`: only `.git` (initialised, remote set). Core and extension skeletons had **not** been written yet. `F:\zeus` has no `crates/` yet.

`F:\zeus\tools\` (added by me at handoff time so nothing ephemeral is lost): `research-checks\refcheck.mjs` (arXiv and RFC metadata checker), `research-checks\mdcheck.mjs` (markdown structure lint), `workflows\research-corpus.workflow.js` and `workflows\scaffold.workflow.js` (full copies of the two workflow scripts, including every agent prompt). These copies are being patched to accept `args` for resume (see P1.0); verify with `Select-String -Path F:\zeus\tools\workflows\*.js -Pattern "args.skip"`.

---

## P0: do these first (loss prevention and unblocking)

### P0.1 Find out whether the workflows are still alive
1. In the Claude Code UI run `/workflows`. If both are listed as running, **leave them alone** and wait for their completion notifications.
2. If not available, check journals (a stale mtime means dead):
   - Research: `C:\Users\arefi\.claude\projects\f--zeus\8bd2c60e-d194-4fa5-8fbc-71ab5d65f741\subagents\workflows\wf_2d6c2744-784\journal.jsonl`
   - Scaffold: `...\subagents\workflows\wf_115d8706-116\journal.jsonl`
   - Live script copies: `C:\Users\arefi\.claude\projects\F--zeus\8bd2c60e-d194-4fa5-8fbc-71ab5d65f741\workflows\scripts\zeus-research-corpus-wf_2d6c2744-784.js` and `...\zeus-scaffold-wf_115d8706-116.js`
3. `Workflow({resumeFromRunId})` is **same-session only**. In a new session do not rely on it. Use the gap-based relaunch in P1.0, which skips what is already on disk.

### P0.2 Commit and push soon, in logical commits (nothing is on GitHub yet)
The remotes are private, so pushing work-in-progress is safe and protects against loss. Use these facts:
- `gh` path: `C:\Users\arefi\AppData\Local\Microsoft\WinGet\Packages\GitHub.cli_Microsoft.Winget.Source_8wekyb3d8bbwe\bin\gh.exe` (not on PATH of old shells; refresh with `$env:Path = [Environment]::GetEnvironmentVariable('Path','User') + ';' + [Environment]::GetEnvironmentVariable('Path','Machine')`). Auth check: `gh auth status` (expect `ArefinAlter`). If it expired, re-run `gh auth login --hostname github.com --git-protocol https --web --skip-ssh-key` and ask the user to enter the one-time code at https://github.com/login/device.
- Commit trailer (append to every commit message): `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. For any PR body end with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- Do not change `git config`. The identity in use is `Arefin_Mizan <adrita.stu2018@juniv.edu>` (a university email: warn the user before any repo goes public).
- **Before `git add`**: confirm no `node_modules/`, `target/`, `dist/`, `*.pem`, `.env`, build caches or tokens are staged: `git status --short`, then `git grep -nIE "gho_|ghp_|BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY"` and `git diff --cached --stat`. `docs/spec/tools/node_modules` must not be committed (it has its own `.gitignore`; the app skeleton adds a root `.gitignore` as well, but confirm).
- Suggested commit grouping for `F:\zeus` (`git -C F:\zeus ...`, branch `main`, then `git push -u origin main`):
  1. `docs: add original market and product research` (`research.md`)
  2. `docs(research): research corpus` (`docs/research/`, `tools/research-checks/`)
  3. `docs(spec): normative contracts and schemas` (`docs/spec/` except node_modules)
  4. `chore: workflow scripts used to generate corpus and scaffold` (`tools/workflows/`, `HANDOFF.md`)
  5. `feat: app workspace (daemon, CLI/TUI, MCP, native host, workflow, llm, a2a)` (everything else, only once `cargo check` is green)
  For `F:\zeus-core` and `F:\zeus-extension`: one initial commit each once their gates pass (P2), then `git push -u origin main`. If a repo is red but valuable, push it on a branch named `wip/initial-scaffold` and say so in the commit message rather than leaving it unpushed.

### P0.3 Keep everything private
The user chose all-private. Do **not** run `gh repo edit --visibility public` unless the user asks. A go-public checklist is in P5.

---

## P1: finish the in-flight work

### P1.0 Relaunching only what is missing (when a workflow died)
Both scripts in `F:\zeus\tools\workflows\` accept `args` (verify the patch is present, otherwise apply the snippets below). Launch with `Workflow({scriptPath: "F:\\zeus\\tools\\workflows\\<file>", args: {...}})`. Their `SP` constant (helper-script folder) defaults to `F:\zeus\tools\research-checks` (research) and `F:\zeus\docs\spec\tools` (scaffold, which contains ajv after `npm ci` there).

**Research script (`research-corpus.workflow.js`) args**
- `skip: [slug,...]`: do not touch docs whose slugs are listed (use for docs already `verified`).
- `verifyOnly: [slug,...]`: skip research, only run the adversarial verifier on a doc that exists but says `status: draft`.
- `skipAudits: true`: skip the two audits of `research.md` if `00a-claims-audit-technical.md` and `00b-claims-audit-market-and-legal.md` already exist.
- `noCritic: true`: skip critic and gap-fill (still runs the final index).
Decide `skip` and `verifyOnly` from disk: for each file in `docs/research/NN-*.md`, `Select-String -Pattern "^status:"` shows `draft` or `verified*`.
Slugs: `http-download-semantics, transport-http2-http3-quic-congestion, other-transfer-protocols-and-sources, math-throughput-concurrency-congestion, math-scheduling-reliability-estimation, integrity-chunking-dedup-delta-erasure, browser-session-handoff-and-extension-platform, browser-download-internals, auth-bot-defenses-and-ethical-fidelity, mcp-protocol-and-tool-design, a2a-and-agent-interoperability-protocols, agent-security-prompt-injection-and-mcp-threats, agentic-browsers-and-web-agent-research, scraping-crawling-and-extraction-research, llm-provider-auth-routing-and-local-models, filesystems-and-file-management-semantics, conversion-parsing-and-sandboxed-pipelines, provenance-dataset-and-content-governance-standards, legal-ethical-and-policy-landscape, rust-ecosystem-and-architecture-survey, competitor-and-reference-implementation-teardowns, desktop-os-integration-packaging-distribution, testing-verification-and-benchmarking-methodology, business-model-licensing-and-open-core-strategy, terminal-orchestration-and-cli-tui-design, workflow-engine-dsl-and-durable-execution, llm-data-mining-pipelines-and-dataset-formats, trust-surface-approvals-and-gui-architecture` (file names are `01-` ... `28-` in that order, e.g. `04-math-throughput-concurrency-and-congestion.md`).

**Scaffold script (`scaffold.workflow.js`) args**
- `skipTasks: [label,...]`: any task whose label is listed returns immediately as a no-op, so completed work is not redone. Labels: `spec:kernel`, `spec:05-daemon-api`, `spec:06-mcp-tools`, `spec:07-approvals`, `spec:08-workflow-plan`, `spec:09-llm-provider`, `spec:10-a2a`, `spec:11-cli`, `spec:12-security`, `skeleton:core`, `skeleton:extension`, `skeleton:app`, `core:zeus-control`, `core:zeus-fs`, `core:zeus-verify`, `core:zeus-journal`, `core:zeus-session`, `core:zeus-http`, `core:zeus-engine`, `ext:downloads`, `ext:native-and-session`, `ext:ui`, `app:zeus-api`, `app:zeus-mcp`, `app:zeus-cli`, `app:zeus-native-host`, `app:zeus-workflow`, `app:zeus-llm`, `app:zeus-a2a`, `app:zeus-daemon`, `integrate:core`, `integrate:app`, `integrate:ext`, `conformance`, `review:core-fs-journal`, `review:core-http-engine-control`, `review:app-daemon-api-native`, `review:app-mcp-workflow-llm-a2a`, `review:extension`, `gates:core`, `gates:app`, `gates:ext`.
- Decide `skipTasks` from disk: a spec is done if its `docs/spec/NN-*.md` and schema exist and `node docs/spec/tools/validate-spec.mjs` passes; a skeleton is done if `F:\zeus-core\Cargo.toml` / `F:\zeus-extension\package.json` / `F:\zeus\Cargo.toml` exist and `cargo check --workspace` / `npm run typecheck` pass; a crate task is done if its crate has real code and `cargo test -p <crate>` passes (not `NotImplemented` stubs).

**If the `args` patch is missing, apply it:**
```js
// research script, near the top (after const SP / COMMON), and before 'const researchPrompt':
const SKIP = new Set((args && args.skip) || [])
const VERIFY_ONLY = new Set((args && args.verifyOnly) || [])
const TODO = DOCS.filter(d => !SKIP.has(d.slug))
// in researchThenVerify stage 1, first line:
//   if (VERIFY_ONLY.has(d.slug)) return { file: ROOT + '\\' + d.file, words: 0, keyFindings: [], gaps: [], unverified: [] }
// call site: researchThenVerify(TODO, 'Research', 'Verify'); missing = TODO.filter(...)
// audits: (args && args.skipAudits) ? Promise.resolve([]) : parallel(AUDITS.map(...))
// critic: const gaps = (args && args.noCritic) ? { newDocs: [] } : await agent(...)
// scaffold script, in run(): if (SKIP_TASKS.has(label)) return { filesChanged: [], files: [], keyDecisions: [], testsPassed: true, knownGaps: ['skipped'] }
```

### P1.A Research corpus workflow (A): what it does and what remains
Pipeline per doc: **research agent (web, writes the file) -> adversarial verifier (runs `refcheck.mjs` against arXiv/RFC registries, `mdcheck.mjs`, recomputes all maths with node, spot-checks 15 to 25 facts, edits in place, flips status) -> repair agent if verdict is needs-rewrite.** Then: two claims audits of `research.md` (00a technical, 00b market/legal) -> critic proposes up to 8 more docs (numbered from 29) -> gap-fill -> index agent writes `docs/research/README.md`, `docs/research/references.md` (generated bibliography tables), and copies checker scripts into `tools/research-checks/`.
Planned docs (28): 01 http-download-semantics, 02 transport (h2/h3/QUIC/CC), 03 other protocols and sources, 04 math: throughput/concurrency/congestion, 05 math: scheduling/reliability/estimation, 06 integrity/chunking/dedup/delta/erasure, 07 browser session handoff + extension platform, 08 browser download internals, 09 auth/bot defenses/ethical fidelity, 10 MCP + tool design, 11 A2A + agent protocols, 12 agent security/prompt injection/MCP threats, 13 agentic browsers + web-agent research, 14 scraping/crawling/extraction research, 15 LLM provider auth/routing/local models, 16 filesystem semantics, 17 conversion/parsing/sandboxed pipelines, 18 provenance/dataset/content-governance standards, 19 legal/ethical/policy, 20 Rust ecosystem + architecture, 21 competitor teardowns, 22 desktop OS integration/packaging/distribution, 23 testing/verification/benchmarking, 24 business model/licensing/open-core, 25 terminal orchestration/CLI/TUI, 26 workflow engine/DSL/durable execution, 27 LLM data-mining pipelines/dataset formats, 28 trust surface/approvals/GUI architecture.

**After the workflow completes (or after a manual relaunch), do these post-steps:**
1. **Rename Haul -> Zeus in `docs/research/*.md`** (agents were told the working name was "Haul"; the user chose Zeus). Case-aware replace, word-boundary: `\bHaul\b` -> `Zeus`, `\bhaul\b` -> `zeus`, `haul-` -> `zeus-`, tool prefix `haul.` -> `zeus_` where it denotes MCP tools, "Implications for Haul" -> "Implications for Zeus". Review the diff for false positives (for example "long-haul"). **Leave `research.md` as the historical snapshot** (it says Haul); add a one-line note at its top pointing to `docs/research/` and the claims audits.
2. Run `node F:\zeus\tools\research-checks\mdcheck.mjs F:\zeus\docs\research\*.md` and `node F:\zeus\tools\research-checks\refcheck.mjs F:\zeus\docs\research\*.md` (the latter takes a few minutes; arXiv API rate limit). Every `MISS` line is a fabricated or mistyped citation: fix or delete it.
3. Fold audit findings (`00a`, `00b`) into `research.md` only if the user wants it. Suspected error I computed while planning: **research.md section 8.14** states a collision probability of about 4x10^-51 for SHA-256 with 10^12 chunks; the stated formula n^2 / 2^(b+1) gives about 4.3x10^-54. Treat that as "suspected, confirm with 00a".
4. Claims in `research.md` that the corpus is explicitly verifying (do not repeat them as fact until audited): MCP "stable 2026-07-28 spec", Chrome 146 DBSC GA in April 2026, ChatGPT Atlas shutdown on 2026-08-09, OpenAI "Sign in with ChatGPT" date and client id (the conversation said DevDay Sept 29; DevDay 2025 may have been Oct 6: check doc 15), Firecrawl and Browserbase funding numbers, `rmcp` 3.x (verified: crates.io shows 3.5.0 on 2026-10-05).
5. Reconcile specs with research: spawn one agent per spec that reads the matching research docs' "Implications" sections and lists/patches contradictions (for example native-messaging message-size limits and MV3 service-worker lifetime in spec 02 vs doc 07; error-class mapping in spec 01 vs doc 08; MCP details in spec 06 vs doc 10). The specs were written while the research was still in progress.

### P1.B Scaffold workflow (B): stages and ownership
Stage order (each stage waits for the previous): **Contracts** (kernel specs 00-04, done; then specs 05-12 + core skeleton + extension skeleton in parallel) -> **app skeleton** -> **Implement** (parallel batch: core `zeus-control, zeus-fs, zeus-verify, zeus-journal, zeus-session`; extension `downloads, native-and-session, ui`; app `zeus-api, zeus-mcp, zeus-cli, zeus-native-host, zeus-workflow, zeus-llm, zeus-a2a`; then sequentially `zeus-http`, `zeus-engine`, `zeus-daemon`) -> **Integrate** (workspace gates, e2e smoke, cross-repo native-messaging conformance test with the real `zeus-native-host` and the extension's own codec) -> **Review** (5 security/correctness reviewers with fix authority) -> final gates.
Design that every agent follows (read the specs; they are authoritative): JSON-RPC 2.0 over local named pipe/Unix socket as the single daemon contract; MCP is the primary agent interface (tools named `zeus_<verb>`, about 9 MVP tools); sessions only via the MV3 extension + native messaging with per-origin consent (**never read browser cookie databases**); in-browser fetch proxy as fallback; redb-backed journal with an explicit crash-point commit protocol; hash-chained audit log and provenance manifest; **human-only approvals** bound to a request hash and carried only by terminal prompt on `CONIN$`/`/dev/tty`, the extension popup or the GUI (never MCP/stdin); LLM provider ladder with a signed policy manifest; A2A as phase 2 (endpoint off by default).
Repository layout: app repo depends on core by **sibling path** (`../zeus-core/crates/<crate>`, with `version = "0.1.0"`); CI checks out both repos (private checkout needs a `ZEUS_CORE_TOKEN` repository secret: **create it before CI can pass**). The extension keeps copies of the normative schemas in `schema/` with a drift check.

### P1.C Cleanups the workflows will not do
- Delete `F:\zeus-build-cache\` when done (cargo target dirs; not a repo).
- Make sure `F:\zeus\docs\spec\tools\node_modules` stays ignored; consider moving the validators into `F:\zeus\tools\` later.
- `CLAUDE.md` (written by the app skeleton agent) should mention `HANDOFF.md` while it exists, then remove the mention when this file is deleted.

---

## P2: verification gates (run yourself before pushing code; do not trust agent summaries)

```powershell
# one cargo at a time; machine is small
$env:CARGO_TARGET_DIR='F:\zeus-build-cache\zeus-core'; $env:CARGO_BUILD_JOBS='3'
cd F:\zeus-core; cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace
$env:CARGO_TARGET_DIR='F:\zeus-build-cache\zeus'
cd F:\zeus; cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace; .\scripts\e2e-smoke.ps1
cd F:\zeus-extension; npm ci; npm run typecheck; npm run lint; npm test; npm run build; npm run check; npm run test:conformance
node F:\zeus\docs\spec\tools\validate-spec.mjs     # (cd docs\spec\tools; npm ci first if node_modules is missing)
```
Pass criteria: all green; the e2e smoke performs download -> `kill -9` mid-way -> restart -> resume -> matching SHA-256; the conformance test proves the extension codec and `zeus-native-host` agree on every message type in both directions plus malformed/oversized/wrong-origin cases. Any unimplemented stub (`NotImplemented`, `todo!`) in an MVP path is a defect; grep: `git grep -nE "todo!\(|unimplemented!\(|NotImplemented"`.

---

## P3: known risks and things that go wrong

1. **Workflow death and non-resume across sessions** (P0.1/P1.0). Cached results exist only for the same session.
2. **Memory pressure**: 7.8 GB RAM; two workflows plus cargo can thrash. If builds time out or the machine swaps, pause the research workflow's concurrency or run the scaffold alone first. All agent prompts set `CARGO_BUILD_JOBS=3` and shared per-repo target dirs under `F:\zeus-build-cache\`.
3. **Cargo.lock races** between parallel agents in one workspace; if `Cargo.lock` looks corrupt, delete it and re-resolve (`cargo update`), then re-run gates.
4. **Cross-crate API drift**: core crates were implemented in parallel against skeleton signatures; agents were told to record API changes in their summaries. Integration agents reconcile; if an app crate fails to compile against core, fix the call site, not the contract, unless the spec is wrong.
5. **Citation hallucination**: the corpus is AI-written. Verified docs have had arXiv IDs and RFC numbers machine-checked; **non-RFC/arXiv claims were only spot-checked (15 to 25 per doc)**. Never present the corpus as authoritative legal advice (doc 19 says so). Any doc still `draft` is unverified.
6. **Docs are very large** (13k to 19k words each); 28 docs may exceed 400k words total. If GitHub rendering or review is a problem, split or trim after the index exists.
7. **Chrome Web Store / ID**: the extension skeleton generates a dev-only `key` and discards the private key; the published extension ID will differ, so native-host `allowed_origins` and the CWS build must be updated at publish time (documented in `docs/EXTENSION_ID.md` in the extension repo).
8. **Antivirus**: a cookie-handling extension plus native host can trip infostealer heuristics; signing and submission plan is in research doc 22.
9. **Trademark**: "Zeus" is a crowded name (software and otherwise). Doc 19 contains a quick conflict search; get a real clearance before a public launch.
10. **`research.md` untracked** in `F:\zeus` and never committed: commit it (P0.2 step 1).

---

## P4: decisions already made (do not re-ask)

- Name: **Zeus everywhere** (repos `zeus`, `zeus-core`, `zeus-extension`; crates `zeus-*`; binaries `zeus`, `zeusd`, `zeus-native-host`; MCP tools `zeus_<verb>`). `research.md` keeps "Haul" as a historical working name.
- Visibility: **all three private** until the user says otherwise (reversible private -> public; the reverse is not).
- Auth: `gh` via winget, user did the device-flow login. Never read the Git Credential Manager token directly (the user declined that option).
- License: Apache-2.0 (open core), "Copyright 2026 Zeus contributors", DCO sign-off for contributions. Paid layers (sync, team policy, enterprise) would be separate closed components later; **never** in the credential path.
- Scope: not marketed as an IDM replacement; GUI and A2A GUI are secondary to the session-aware downloader/file layer; approvals are a core feature from day one (trust surface = terminal on tty + extension popup + OS notifications, full GUI later).
- LLM access ladder: host agent over MCP -> MCP sampling -> Sign in with ChatGPT -> delegate to the user's own CLI (Codex first) -> OpenRouter one-click OAuth -> raw API key / local model. **Never** extract or replay Claude or Gemini consumer subscription tokens; Grok subscription OAuth is experimental opt-in only. Default with no config: no model access. Metadata-only by default.
- Sessions: only through the extension + native messaging, per-origin consent (`optional_host_permissions`), cookies never persisted or logged, zeroized; in-browser fetch fallback instead of TLS impersonation or captcha solving.
- Stack: Rust edition 2024, MSRV 1.88; tokio, reqwest 0.13 (rustls), redb 4.x journal, rmcp 3.5 (MCP), clap 4.6, ratatui 0.30, Tauri 2 for the later GUI.

---

## P5: what comes after this handoff (priority order)

1. **Review the research docs and specs yourself** (or have the user do so) before anything is made public; start with 00a/00b audits, 07, 08, 12, 15, 19.
2. **Go-public checklist** (only on the user's go): remove or trim `HANDOFF.md`; scrub absolute local paths from `tools/workflows/*.js`; confirm no secrets (`git grep`); review commit author email exposure (suggest the GitHub noreply address via `git config user.email` for the repo, and optionally rewrite history before first public push); decide whether the strategy/pricing parts of `research.md` should stay public; add repository secrets; enable GitHub private vulnerability reporting; then `gh repo edit ArefinAlter/<repo> --visibility public --accept-visibility-change-consequences`.
3. **Run the five cheap validation experiments** from `research.md` section 7 (spec'd in detail in doc 24): handoff demo on 10 real failures, MCP wrapper with 10 users who reported Playwright MCP download issues, Show HN/r/DataHoarder post, pricing page, upstream chats with Gopeed and Playwright MCP maintainers.
4. **Publish path for the extension**: Chrome Web Store developer account (one-time fee), privacy disclosures from `docs/PRIVACY.md` and `docs/PERMISSIONS.md`, Firefox AMO unlisted signing for dev, then listed.
5. **Release engineering** (doc 22): code signing options (SignPath Foundation for OSS, Azure Trusted/Artifact Signing eligibility: verify), `cargo-dist`, winget/Scoop/Homebrew, SBOM and attestations.
6. **Roadmap** (from `research.md` 5.8 and the sequencing in the original conversation): 0 to 3 months engine + handoff + MCP tools + trust surface; 3 to 6 months workflows, conversion, provider ladder, MCP App views, daemon API frozen as v1; 6 to 9 months A2A endpoint, rules/dedupe, Tauri GUI beta; 1.0 hardening, signed builds, agent panel, A2A views, organizer previews.
7. **Open questions for the user**: GitHub org vs personal account for the long term; trademark clearance appetite; whether to publish research docs publicly; extension store accounts; who owns signing certificates; desired funding route (sponsors vs grants such as NLnet / Sovereign Tech Agency).

---

## Appendix A: environment cheat sheet

- Working dir: `F:\zeus` (VS Code extension session, Windows 11, PowerShell primary, Bash available).
- Cargo env for every build: `$env:CARGO_TARGET_DIR='F:\zeus-build-cache\<repo>'; $env:CARGO_BUILD_JOBS='3'`; one cargo process at a time; dev profile uses `debug = "line-tables-only"`.
- Crate versions seen on crates.io 2026-10-05: tokio 1.53.2, reqwest 0.13.5, rmcp 3.5.0, redb 4.3.0, clap 4.6.7, ratatui 0.30.2, blake3 1.8.7.
- Session id of the originating session: `8bd2c60e-d194-4fa5-8fbc-71ab5d65f741` (transcripts under `C:\Users\arefi\.claude\projects\f--zeus\8bd2c60e-d194-4fa5-8fbc-71ab5d65f741\`).
- Memory notes for future sessions live in `C:\Users\arefi\.claude\projects\f--zeus\memory\` (`project_zeus.md`, `reference_dev_environment.md`).
- Checker tools: `node F:\zeus\tools\research-checks\refcheck.mjs <files>` (arXiv via export.arxiv.org API, RFC via rfc-editor JSON, 3.5 s sleeps), `node ...\mdcheck.mjs <files>` (front matter, required sections `Implications for`, `Open questions`, `References`, table column counts, fences, emoji).

## Appendix B: required structure of every research doc (what `mdcheck` enforces)
YAML front matter (`title, slug, status, last_researched, confidence, related`), `## TL;DR`, body, `## Implications for Zeus`, optional `## Corrections to research.md`, `## Open questions`, `## References` with stable keys (`[RFC 9110]`, `[arXiv:2307.13854]`, ...). Inferences labelled `*Inference*`; confidence tags High/Medium/Low; no emoji.
