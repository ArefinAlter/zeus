# Common tasks. Install `just` (https://github.com/casey/just) or run the commands directly.
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

default:
    @just --list

fmt:
    cargo fmt --all

check:
    cargo check --workspace --all-targets

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

deny:
    cargo deny check

spec:
    cd docs/spec/tools; npm ci; npm run validate

all: fmt lint test
