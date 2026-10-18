#!/usr/bin/env bash
# Ensure the wasm artifact for @cdlab/jsc exists and is not stale.
# A missing artifact, or one older than its Rust inputs, is rebuilt via
# build-wasm.sh. A fresh artifact is left alone.
set -euo pipefail

cd "$(dirname "$0")/.."

WASM_FILE="packages/jsc/wasm/jsc_wasm_bg.wasm"

if [ ! -f "$WASM_FILE" ]; then
  echo "ensure-wasm: rebuilding (missing: $WASM_FILE)"
  bash scripts/build-wasm.sh
  exit 0
fi

# Staleness probe: compare the artifact against the Rust inputs.
newer=$(find crates \( -name '*.rs' -o -name 'Cargo.toml' \) -newer "$WASM_FILE" -print -quit)
if [ -z "$newer" ] && [ Cargo.lock -nt "$WASM_FILE" ]; then
  newer="Cargo.lock"
fi

if [ -n "$newer" ]; then
  echo "ensure-wasm: rebuilding (stale: $newer)"
  bash scripts/build-wasm.sh
fi
