# Acceptance 0.0.1b: browser integration (scenario 8) and MCP for AI assistants (scenario 9)

Written by the integration workstream. Status of each claim is stated as it was observed; anything not proved says so.

## What was built

| Piece | Where |
|---|---|
| Packaging of the browser extension | `apps/client/scripts/package-extension.mjs` builds `A:\zeus-extension`, writes `zeus-extension-chromium.zip`, `zeus-extension-firefox.zip` and `extension.json` to `apps/client/dist/extension/` and `apps/client/src-tauri/resources/extension/` (both git-ignored) |
| The package inside the app | `crates/zeus-app-integration/build.rs` embeds the zips; `package.rs` unpacks them to `<home>/app/integration/extension/{chromium,firefox}` (idempotent, path-escape and size checks) |
| Host registration from the app | `host.rs`: a launcher script (`.cmd` on Windows, `.sh` elsewhere) that sets this installation's `ZEUS_*` variables and starts the bundled `zeus-native-host`; manifests and registry values through `zeus-native-host`'s own planner (HKCU on Windows, the user's configuration folders on Linux); consent required |
| Status, plan, install, uninstall, export routes | `GET /native/v1/integration`, `POST /native/v1/integration/{plan,install,uninstall,unpack,export}` |
| MCP configuration and test | `GET /native/v1/mcp/config`, `POST /native/v1/mcp/test` (a real handshake with the installed `zeus mcp`) |
| The pages | `Connect` (sidebar, Agents) and Settings, Browser integration (`views/Connect.tsx`, `views/integration/*`) |
| Host changes (daemon workspace) | `InstallRequest.allow_list_dir`, `zeus native-host install --allow-list-dir`, the host reads `allowed-extensions.json` from the state folder when none sits beside it (`allowlist::locate`), `parent::recognized_installs()` |
| Acceptance scripts | `tests/acceptance/s08-browser.mjs`, `s09-mcp.mjs`, `tests/mcp-client.mjs` |

## Results

Runs on Windows 11 (2026-10-27) with the release binaries of `zeus`, `zeusd` and `zeus-native-host` (`ZEUS_BIN_DIR`), the app's engine behind the loopback harness, and a real, machine-installed Microsoft Edge started by Playwright in a throw-away profile with the packaged Chromium extension. The extension id is the development id `nfiaihlklfadnbcfadmhbmddabmafilo` that the build embeds.

### Scenario 8, `s08-browser.mjs`: passed with limits

Proved:

- The packaged extension is in the build; Edge is found and the host can verify it (the machine-wide install under `Program Files (x86)`).
- Installing without consent is refused and writes nothing. The plan lists six steps (registry key, manifest, launcher, allow-list, extension folders). With consent, the registration points at this app's launcher and pins the extension id; the host's allow-list lists the id as a development extension in the engine's state folder; the launcher sets this installation's `ZEUS_*` variables and starts the bundled host.
- Edge loads the packaged extension under its development id, and the extension connects to the engine through the native host.
- The browser's own download is interrupted and cannot be resumed (`SERVER_BAD_CONTENT`, 0 bytes); the extension offers it to Zeus; the engine creates a job from the browser in state `awaiting_approval` and an approval of kind `handoff` is pending, as listed by the command line a person uses (`zeus approvals list`). The status shows one request waiting for a person.
- With no decision nothing downloads: the job waits and no bytes were fetched.
- No route of the app can decide it (`approval.not_found`).
- Cleanup: the Edge registration is removed again.

Not proved:

- Completion after approval. The popup offers "Allow once", "Allow this site for 24 hours", "Don't allow" and "Never for this site", and a click delivered through the browser's input pipeline answers "The decision could not be sent (This version of Zeus cannot take decisions from the browser.)". A development extension has no published id, so `zeusd` does not accept decisions from it (Zeus ADR 0007). The script records this as a limit, the approval keeps waiting, and a person decides it at the terminal (`zeus approvals`). Deciding it that way, and the bytes arriving after it, was not run: the human surface needs a person at a terminal, and the acceptance runs do not stand in for one.
- Chrome, Firefox, and Linux browsers.

Screenshots (kept outside the repository): the popup with the pending request, and after the refused click.

### Scenario 9, `s09-mcp.mjs`: passed

The Connect page's configuration for Claude Code, Cursor, Codex and a generic client each parse and carry the four variables that select this app's engine (`ZEUS_PROFILE`, `ZEUS_STATE_DIR`, `ZEUS_CONFIG_DIR`, `ZEUS_RUNTIME_DIR`); the connection test (a real handshake from Rust with the installed `zeus mcp`) lists thirteen tools in 89 ms. A separate MCP client over stdio (`tests/mcp-client.mjs`), started from the generated configuration, initialises (protocol 2025-06-18, zeus 0.0.1-beta), lists the thirteen tools, probes a URL, enqueues a download, waits for it, verifies it and gets the SHA-256 the fixture server holds. No listed tool can decide an approval; a session request creates a pending approval and nothing more; a call to an approving tool is refused as unknown; the app's route table has no way to decide a daemon approval.

