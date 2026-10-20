# Contributing to Zeus

Thank you for helping. Zeus is pre-alpha; the most valuable contributions right now are review of the specifications, tests, and focused implementations of crates that are still stubs.

## Ground rules

- **Developer Certificate of Origin.** Every commit needs a sign-off (`git commit -s`), which adds `Signed-off-by: Your Name <you@example.com>` and certifies the statement at <https://developercertificate.org/>. There is no CLA.
- **Contracts first.** Behaviour that crosses a process boundary is specified in `docs/spec`. If code and spec disagree, the prose of the spec wins; fix the spec in the same pull request when the spec is wrong, and say so in the description. A change that is hard to reverse (a contract, a trust boundary, a licence, a persistent format, the dependency graph) needs an ADR in `docs/adr`.
- **Security-sensitive changes** (anything touching sessions, approvals, transport, native messaging, paths or redaction) need a test that fails without the change and a note in the pull request on which invariant it protects.
- **No telemetry, no cookie-database access, no evasion features.** These are project invariants (ADR 0004, ADR 0007).
- Be kind. See the [Code of Conduct](CODE_OF_CONDUCT.md).

## Development setup

Clone the repositories **side by side**:

The `zeus-core` and `zeus-extension` repositories currently require access. For coordinated pull requests, use the exact companion revisions named in the PR description so that engine interfaces, schemas and generated browser types agree.

```
zeus-workspace/
  zeus/              this repository
  zeus-core/         the engine library (path dependency of this workspace)
  zeus-extension/    the browser extension (only needed for extension work)
```

```sh
git clone https://github.com/ArefinAlter/zeus.git
git clone https://github.com/ArefinAlter/zeus-core.git
git clone https://github.com/ArefinAlter/zeus-extension.git   # optional
cd zeus
```

Requirements: Rust stable (MSRV 1.90), Node 22 and npm for the spec validators, and optionally `cargo-deny` (`cargo install cargo-deny`).

### Checks to run before opening a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check                       # licences, advisories, banned crates
( cd docs/spec/tools && npm ci && npm run validate )
```

The scripts `scripts/check.sh` and `scripts/check.ps1` run the Rust checks in order.

### Small machines

Builds are heavy. Use `CARGO_BUILD_JOBS=3`, a shared target directory outside the repository (`CARGO_TARGET_DIR`), and run one cargo command at a time. The dev and test profiles use `debug = "line-tables-only"` to keep build output small.

### Windows without MSVC

The default Windows Rust toolchain (`x86_64-pc-windows-msvc`) needs the Visual Studio Build Tools and a Windows SDK to link. If you cannot install them, use the GNU toolchain instead; it links with its bundled MinGW and does not need the SDK:

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
$env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-gnu'   # per shell; does not change your rustup default
cargo test --workspace
```

Use `RUSTUP_TOOLCHAIN` per shell rather than `rustup default`, so other projects are unaffected. CI builds on `windows-latest` with MSVC, so keep platform code behind `cfg` and test on that target when you change it. If a C-heavy dependency fails to build on `windows-gnu`, see "Build notes" in `docs/ARCHITECTURE.md`.

## Pull requests

1. Keep them small and focused; describe the user-visible or contract-visible change first.
2. Add or update tests, specs and docs together with the code.
3. Public items need doc comments; comments otherwise explain *why*, not *what*.
4. No `todo!()` or `unimplemented!()`. Something not yet built returns a typed `NotImplemented` error and is listed in the pull request.
5. Never commit secrets, real cookies, tokens or personal data, including in test fixtures. Use the marker `ZEUS-CANARY-` for leak tests.

## Reporting vulnerabilities

Do not open a public issue. See [SECURITY.md](SECURITY.md).
