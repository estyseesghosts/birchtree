#!/usr/bin/env bash
# Full gate for birchtree: cargo check all targets + cargo test + phase smokes.
# Run before claiming phase-level completion.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> cargo check --all-targets"
cargo check --all-targets

echo "==> cargo test"
cargo test

echo "==> scripts/smoke-phase1.sh"
bash scripts/smoke-phase1.sh

echo "==> scripts/smoke-phase2.sh"
bash scripts/smoke-phase2.sh

echo "smoke-all: green"
