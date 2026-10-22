#!/usr/bin/env bash
# Lab-only launcher for the phase 6 interop run (see README.md). Not for production:
# --federation-allow-http, --federation-allow-private and --federation-extra-ca are
# lab switches, and the allowlist also turns on the crate's debug mode.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LAB="${LAB:-/root/parity/lab}"
BT_DATA="${BT_DATA:-/root/parity/birchtree-lab-data}"
cd "$ROOT"
RUST_LOG="${RUST_LOG:-info,birchtree::federation=debug,birchtree::jobs::worker=debug,activitypub_federation=debug}" \
  exec ./target/debug/birchtree --data-dir="$BT_DATA" --public-url=https://birchtree.lab \
  --listen=0.0.0.0:8080 --federation-allow-private=172.28.0.0/24 \
  --federation-extra-ca="$LAB/certs/lab-ca.crt" --federation-allow-http --trusted-proxy=172.28.0.20 \
  --smtp-url=smtp://127.0.0.1:25 --mail-from=noreply@example.social
