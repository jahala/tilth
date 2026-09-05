#!/usr/bin/env bash
# The workspace is green on stable, the same three gates CI runs, across every
# member: formatting, clippy with warnings denied on all targets, the tests.
set -euo pipefail
cd "$(dirname "$0")/../.."
rustup run stable cargo fmt --all --check
rustup run stable cargo clippy --workspace --all-targets -- -D warnings
rustup run stable cargo test --workspace
