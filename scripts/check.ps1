# Run the Rust quality gates in order. One cargo process at a time; use a shared target dir on small machines.
# Windows without MSVC: set $env:RUSTUP_TOOLCHAIN to a windows-gnu toolchain first (see CONTRIBUTING.md).
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
if (-not $env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS = '3' }

cargo fmt --all --check
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo clippy --workspace --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo test --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
    cargo deny check
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} else {
    Write-Host 'cargo-deny not installed; skipping (cargo install cargo-deny)'
}
