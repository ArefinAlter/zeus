# Actual-process smoke test

On Windows, build the complete debug package from this repository, then run the dependency-free Node harness:

```powershell
cargo build -p zeus-cli -p zeus-daemon -p zeus-native-host --bins --all-features
node tools/smoke-daemon.mjs
```

Use Windows Node 22 or later. All three executables must be together in `target/debug`; the harness accepts no binary, endpoint or state-directory overrides. Do not rebuild while it runs: executable hashes must stay unchanged.

Each run creates an exclusive directory under `target/smoke-runs`. State, configuration, runtime files and download write roots live there. The harness clears inherited Zeus/proxy environment settings and serves synthetic files only on `127.0.0.1`. Windows still names the IPC pipe per OS user: if an existing daemon occupies it, the test aborts before making CLI connections. The test never stops an existing daemon. Every CLI request is gated on fresh discovery matching the exact live child process and instance started by this run.

Checks cover idempotent initialization, explicit executable pins, authenticated status, rejection of a second listener with independent state, real HTTP probe/download/wait/verification, durable enqueue idempotency, audit integrity, clean shutdown and restart. A slow 8 MiB fixture delivers 64 KiB chunks every 30 ms. After observing a durable but incomplete download, the harness abruptly terminates only its own daemon child. It verifies and archives that child's stale discovery, restarts, checks nonzero resume ranges and the original strong ETag, and compares the complete file bytes and SHA-256. The fixture exceeds the production 4 MiB commit unit so it can expose committed progress before completion.

`report.json` records assertions, executable hashes and crash/resume evidence. Per-command logs and synthetic state remain beside it for review. Exit code 0 means every assertion passed; a failure returns nonzero and cleans up only spawned process handles. Windows extended-length file paths are resolved with native canonicalization before checking that final files remain inside the synthetic write root.

This test does not validate power-loss durability, every injected crash point, physical-terminal approvals, browsers, OS keyrings, providers, packaging or non-Windows behavior. It performs no browser registration, human approval, account login or paid model call.
