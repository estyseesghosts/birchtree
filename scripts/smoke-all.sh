#!/usr/bin/env bash
# Full gate for birchtree: cargo check all targets + cargo test.
# Run before claiming phase-level completion.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> cargo check --all-targets"
cargo check --all-targets

echo "==> cargo test"
cargo test

echo "smoke-all: green"
