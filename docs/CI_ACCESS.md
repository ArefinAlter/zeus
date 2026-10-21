# CI access to private zeus-core

Configured on 2026-10-21 with the user's approval. The app workflow uses a repository-scoped, read-only Ed25519 deploy key instead of a personal access token.

| Setting | Value |
|---|---|
| Repository granting read access | `ArefinAlter/zeus-core` |
| Deploy key | `167128912` — `zeus Actions read-only core checkout 2026-10-21` |
| Public fingerprint | `SHA256:JorAV4ZR1Ip4fUJtLIH9Fg+u2095kEVxpzNzJoWyUgU` |
| Repository storing the private half | `ArefinAlter/zeus` |
| Actions secret | `ZEUS_CORE_SSH_KEY` |
| Workflow | [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) |

GitHub reports `read_only: true`. An isolated SSH probe using only this key, with host-key verification, read core's `main` at `a94655cdef51e452f6f582fb4be6193ec4a1db1c` on port 22. The private key was encrypted into the repository's Actions secret through GitHub CLI stdin, then its temporary local files were deleted. No private key is committed or retained as a local backup.

The ref-resolution job checks out the compatible pinned core revision with `actions/checkout@v5`, then uses its SSH origin to look for the matching branch. It executes only the inline lookup, retains credentials for that job, and relies on checkout's post-job cleanup. A missing branch selects the compatible pin; authentication or transport errors fail the job. Every build/check job uses `persist-credentials: false`, so the checkout credential is removed before repository code runs. All seven private-core checkout steps use the same secret with strict host-key checking. Fork pull requests do not receive the secret.

These settings follow the official [checkout SSH inputs and credential cleanup](https://github.com/actions/checkout/blob/v5/README.md#usage). GitHub documents the [single-repository deploy-key scope and read-only setting](https://docs.github.com/en/authentication/connecting-to-github-with-ssh/managing-deploy-keys), and publishes the [SSH host fingerprints](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints) used for verification.

Rotation replaces this pair: generate a new key, add its public half as a read-only core deploy key, replace the app Actions secret, verify read access, then remove the old deploy key. Update the ID/fingerprint above. To revoke access, remove the core deploy key and delete the app secret. Deploy keys do not expire automatically; deleting the creating user's access is not a substitute for removing this key.

Validation includes live private-repository SSH access, the server's read-only key metadata, all seven checkout configurations, Bash syntax and five real-Git resolver scenarios: main, a matching branch, a missing branch, an explicit ref and an inaccessible remote.

The first [hosted run](https://github.com/ArefinAlter/zeus/actions/runs/39529976785), at app `81eaa0b`, authenticated successfully on Linux, macOS and Windows x64. It passed app tests on Windows x64 (496) and Linux x64/ARM64 and Intel/Apple Silicon macOS (497 each), MSRV 1.90, contracts, and both Windows/macOS desktop jobs (five Rust tests, four frontend tests, bundled executable build and strict Clippy each). Build/test evidence does not establish installed GUI, browser, keyring or physical-device acceptance. Core/extension Actions still face the reported account billing/spending-limit restriction.

That run exposed two failures addressed by the followup:

- Windows ARM's native OpenSSH rejected checkout's key because its parent directory granted access to Authenticated Users. Before that private checkout, [protect-runner-temp.ps1](../.github/scripts/protect-runner-temp.ps1) replaces only the temp directory's own ACL, limits inheritance to the current runner SID and SYSTEM, and verifies the result. Existing children are untouched. The [disposable Windows test](../.github/scripts/test-runner-temp-acl.ps1) reproduces the unsafe parent, Node-created key and checkout's exact ACL commands; native OpenSSH then reads an unregistered throwaway key successfully. No production credential is used in that test.
- The dependency gate found [RUSTSEC-2026-0382](https://github.com/RustSec/advisory-db/blob/main/crates/rustls-webpki/RUSTSEC-2026-0382.md). App, core and desktop lockfiles now select `rustls-webpki 0.103.16`, the [upstream fix](https://github.com/rustls/webpki/pull/546). No advisory exception was added.

An additional desktop advisory audit using the app policy found six existing unmaintained-dependency notices. A targeted update to `tauri-utils 2.10.2`, accepted by the existing Tauri version requirements, replaces the five `unic-*` crates through `urlpattern 0.6`. The [published utility manifest](https://github.com/tauri-apps/tauri/blob/tauri-utils-v2.10.2/crates/tauri-utils/Cargo.toml) records that dependency. `proc-macro-error` remains through Linux GTK 0.18; [its advisory](https://rustsec.org/advisories/RUSTSEC-2024-0370.html) has no patched version. No suppression or target exclusion was introduced. The hosted dependency job audits the app workspace, not the independent desktop workspace; track the desktop audit separately from passing build/test jobs.

After the dependency updates, local Windows app tests pass **496/496** and core tests pass **287/287**, with one intentionally ignored core documentation example. The app and core pass their full dependency/license/advisory checks. The bundled desktop executable builds and passes five Rust bridge tests, four frontend tests, strict all-target Clippy and a Rust 1.90 source check. Its separate advisory rerun now reports only `RUSTSEC-2024-0370` (`proc-macro-error`), so that full desktop advisory check remains failing. The core security update is [`6c622a1`](https://github.com/ArefinAlter/zeus-core/commit/6c622a18d6822222575902dc3e16b24be6d95ebc); it is also the workflow's compatible fallback pin. Core's [new hosted attempt](https://github.com/ArefinAlter/zeus-core/actions/runs/39531813017) still cannot start because GitHub reports failed account payments or an insufficient spending limit.
