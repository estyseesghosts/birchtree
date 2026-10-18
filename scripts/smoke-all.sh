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

echo "==> scripts/smoke-phase3.sh"
bash scripts/smoke-phase3.sh

echo "==> scripts/smoke-phase4.sh"
bash scripts/smoke-phase4.sh

echo "==> scripts/smoke-phase5.sh"
bash scripts/smoke-phase5.sh

echo "==> scripts/smoke-phase6.sh"
bash scripts/smoke-phase6.sh

echo "==> scripts/smoke-phase7.sh"
bash scripts/smoke-phase7.sh

echo "==> scripts/smoke-phase7-1.sh"
bash scripts/smoke-phase7-1.sh

echo "==> scripts/smoke-phase7-2.sh"
bash scripts/smoke-phase7-2.sh

echo "==> scripts/smoke-phase7-3.sh"
bash scripts/smoke-phase7-3.sh

echo "smoke-all: green"
