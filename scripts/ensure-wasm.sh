#!/usr/bin/env bash
# Ensure the wasm artifact for @cdlab/jsc exists and is not stale.
# A missing artifact triggers a rebuild via build-wasm.sh. A stale artifact
# (Rust inputs newer than the artifact) only prints a warning: staleness is
# non-fatal so existing local flows keep working.
set -euo pipefail

cd "$(dirname "$0")/.."

WASM_FILE="packages/jsc/wasm/jsc_wasm_bg.wasm"

if [ ! -f "$WASM_FILE" ]; then
  bash scripts/build-wasm.sh
  exit 0
fi

# Staleness probe: compare the artifact against the Rust inputs.
newer=$(find crates \( -name '*.rs' -o -name 'Cargo.toml' \) -newer "$WASM_FILE" -print -quit)
if [ -z "$newer" ] && [ Cargo.lock -nt "$WASM_FILE" ]; then
  newer="Cargo.lock"
fi

if [ -n "$newer" ]; then
  {
    echo
    echo "=================== WARNING: STALE WASM ARTIFACT ==================="
    echo "Rust sources changed after the wasm artifact was built"
    echo "($newer is newer than $WASM_FILE)."
    echo "Checks will run against STALE wasm. Rebuild with:"
    echo "    bash scripts/build-wasm.sh"
    echo "===================================================================="
    echo
  } >&2
fi
