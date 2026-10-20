# ADR 0009: Tauri desktop client

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |
| Updated | 2026-10-21 |

## Context

The first front ends are the CLI, a ratatui TUI, the MCP server and the browser extension popup (a trust surface). The daemon now supports enough download, workflow, file and artifact operations to implement an initial desktop client. Its authority and dependencies must remain separate from the engine and from attested human approval channels.

## Decision

1. The GUI is a **client of the daemon API** like every other front end (ADR 0002): no engine code in the GUI process. Its initial scope follows existing daemon capabilities; unsupported operations are unavailable rather than simulated.
2. Technology: **Tauri 2** (system WebView, Rust backend). The initial frontend uses dependency-free ES modules, semantic HTML and CSS, with no transpiler, development server or remote assets. Default Cargo features embed the checked-in frontend through Tauri's production `custom-protocol` mode; direct Cargo builds do not require the Tauri CLI.
3. The initial GUI is **not an attested human decision surface**. Its dedicated `gui.token` has kind `agent`, role `gui` and an explicit permission subset for reading work and previewing, enqueueing and controlling downloads. The closed Rust command registry never exposes approval decisions, grants, session material, original job URLs or generic RPC. Human review remains in the interactive CLI. A future GUI approval surface requires the `gui` channel evidence of ADR 0007 and a separate reviewed authority upgrade.
4. The GUI lives in [apps/desktop](../../apps/desktop/README.md), an independent Cargo workspace excluded from the daemon workspace. Its Tauri dependency tree and lockfile do not burden daemon builds. Rust owns the authenticated connection and private link reviews; the bundled main webview receives explicit metadata projections and operation receipts.

## Consequences

- None of the daemon workspace crates depends on a GUI toolkit.
- Current desktop views poll bounded pages and retain selection by stable identity. Event replay, full file indexing, native mobile execution and GUI approval attestation remain separate work.
- Windows desktop checks and informational macOS checks validate source compilation. Installed system-webview behavior, accessibility, signing and device acceptance still require native runs.

## Alternatives considered

- Electron: larger footprint and a second runtime.
- Native toolkits per platform (WinUI, SwiftUI, GTK): best fidelity, three codebases.
- egui or iced: viable, but weaker accessibility and no webview reuse for MCP App views.
