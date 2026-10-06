# ADR 0009: Tauri for the later GUI

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |

## Context

The MVP front ends are the CLI, a ratatui TUI, the MCP server and the browser extension popup (a trust surface). A desktop GUI helps non-terminal users and gives approvals a richer trust surface, but it is not on the critical path and must not distract from the engine.

## Decision

1. The GUI comes **after** the MVP and the workflow phase (roadmap: months 6 to 9, beta). It is a **client of the daemon API** like every other front end (ADR 0002): no engine code in the GUI process.
2. Technology: **Tauri 2** (system WebView, Rust backend), because binaries stay small and the backend language matches the rest of the project. The webview frontend is plain TypeScript with a minimal dependency set.
3. The GUI is a **trust surface**: it may carry approval decisions with the `gui` channel evidence of ADR 0007. It never receives session material.
4. The GUI lives in its own repository or directory, added to this workspace only when work starts, so its dependency tree does not burden the daemon builds.

## Consequences

- Nothing in the current crates depends on a GUI toolkit.
- The approvals and event APIs are designed to be sufficient for a GUI today.

## Alternatives considered

- Electron: larger footprint and a second runtime.
- Native toolkits per platform (WinUI, SwiftUI, GTK): best fidelity, three codebases.
- egui or iced: viable, but weaker accessibility and no webview reuse for MCP App views.
