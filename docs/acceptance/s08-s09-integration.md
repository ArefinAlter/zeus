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

RESULTS: to be filled in below as the runs complete.
