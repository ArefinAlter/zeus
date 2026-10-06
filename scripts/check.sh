#!/usr/bin/env bash
# Run the Rust quality gates in order. One cargo process at a time; use a shared target dir on small machines.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${CARGO_BUILD_JOBS:=3}"
export CARGO_BUILD_JOBS

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
if command -v cargo-deny >/dev/null 2>&1; then
  cargo deny check
else
  echo "cargo-deny not installed; skipping (cargo install cargo-deny)"
fi
