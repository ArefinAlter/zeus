# Zeus desktop client

An executable Tauri 2 client for the local daemon: redacted download pages and inspectors, stable selection, current-page search, eligible batch pause/resume/retry/cancel, link preview and enqueue, workflow reports, default-root files, artifact integrity facts and restricted approval listing. Every operation uses the real authenticated daemon API. Empty, disconnected and unavailable states contain no sample records.

The Rust process owns `IpcClient`, the IPC secret and the dedicated `gui.token`. This credential is `agent` kind with `gui` role and seven explicit permissions. The webview receives display projections and typed receipts. It cannot decide approvals, change grants, reveal original job URLs, invoke arbitrary API methods, launch processes or open arbitrary filesystem paths. Approval listing follows the daemon's agent visibility rules; review and decision remain in the interactive CLI.

This is an independent Cargo workspace, following ADR 0009, so Tauri and system webview dependencies do not enter daemon workspace builds. The frontend is dependency-free ES modules with semantic HTML and CSS. It needs no npm install, bundler or remote asset service.

## Run

Install the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/) for the native OS. Windows needs WebView2 and an MSVC build environment; Linux needs the documented WebKitGTK/system development packages; macOS needs Xcode command line tools. Keep `zeus-core` beside this repository as required by the main workspace.

Stop any running daemon before provisioning the updated installation, then run these commands from the repository root:

```sh
# Build the package binaries together so daemon initialization can verify their paths.
cargo build -p zeus-daemon -p zeus-cli -p zeus-native-host
# Update provisioning to create the dedicated gui.token; existing tokens are preserved.
cargo run -p zeus-cli -- daemon install
# Start the installed daemon normally, or run it in another terminal.
cargo run -p zeus-daemon --bin zeusd -- run
# Build and open the bundled local UI.
cargo run --manifest-path apps/desktop/Cargo.toml
```

The usual `ZEUS_PROFILE`, `ZEUS_STATE_DIR`, `ZEUS_CONFIG_DIR` and `ZEUS_RUNTIME_DIR` settings select the daemon installation. The desktop never falls back to the CLI, native-host or agent token. If an older daemon installation lacks `gui.token`, update it and rerun `zeus daemon install` before reconnecting.

Default features include Tauri's `custom-protocol` production mode. `frontendDist` points to the checked-in `ui` directory; the context macro embeds these files in the executable. There is no development URL, localhost web server or Tauri CLI step. Both direct `cargo run` and `cargo build --release` use the bundled assets. `--no-default-features` intentionally omits the system webview for bridge tests.

Paste one URL per line in Link inbox and select **Preview downloads**. The daemon resolves the destinations without creating jobs. The Rust process retains the exact original inputs for up to ten minutes and exposes a review identifier to the UI. **Start reviewed downloads** enqueues that retained batch using a durable retry key. A reconnect invalidates the review. Different signed query strings remain different inputs. The input limit is 50 unique HTTP(S) URLs and 64 KiB.

Controls require a successful health/authority refresh within 20 seconds, the current connection generation, the appropriate permission and negotiated idempotency support. Connection failure disables changes. Successful receipts are displayed; an uncertain attempt keeps its retry key until a fresh view shows a changed job version. Batch controls run one typed request per selected job and report how many completed if a later request fails. A lost daemon connection invalidates link reviews; inspect Downloads before submitting a replacement batch when the preceding receipt was lost.

## Validate

```sh
node --test apps/desktop/tests/model.test.mjs
cargo test --manifest-path apps/desktop/Cargo.toml --no-default-features
cargo check --manifest-path apps/desktop/Cargo.toml
cargo clippy --manifest-path apps/desktop/Cargo.toml --all-targets -- -D warnings
```

The headless Rust build verifies the bridge without a GUI system dependency. It does not establish that the installed system webview starts. Native GUI acceptance still needs Windows, Linux and macOS runs, keyboard/screen-reader testing, 200% zoom, reduced motion, daemon restart, expired review, denied permission and partial-batch failure testing. A CI build or source-level test is not device acceptance.

## Boundaries

The client refreshes the current page every two seconds while visible. It shows at most 100 records per page, preserves selection by stable identity, and filters the loaded page only. It is not a filesystem index or an event-replay reducer. The Files page reads the default configured root and does not scan the device. Optional pages explain missing daemon capabilities. No queue scheduler, storage treemap, file mutation, artifact deletion, conversion, sync or peer-to-peer transport is simulated.

The application targets desktop operating systems. Android/iOS need native background-transfer and scoped-storage adapters before a mobile app can be claimed; a desktop local IPC connection is not a mobile execution model. Installer signing, notarization, application-store submission and real device acceptance remain release work.

Tauri authority is limited to the bundled `main` window and five generated app-command permissions. No shell, filesystem, HTTP, opener or remote-content plugin is installed. See the official [capability model](https://v2.tauri.app/security/capabilities/) and [command interface](https://v2.tauri.app/develop/calling-rust/) for the framework boundary.
