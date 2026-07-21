#!/usr/bin/env bash
# wasm build for @cdlab/jsc: a single artifact built from crates/jsc-wasm with
# its default feature set. The output lands in packages/jsc/wasm and is loaded
# by the @cdlab/jsc package. Unlike @cdlab/image (one artifact per subpath),
# jsc ships a single wasm module, so there is no per-feature fan-out here.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi

CRATE_DIR="crates/jsc-wasm"
OUT_DIR="../../packages/jsc/wasm"
WASM_FILE="packages/jsc/wasm/jsc_wasm_bg.wasm"

if [ ! -f "$CRATE_DIR/Cargo.toml" ]; then
  echo "error: $CRATE_DIR does not exist yet (created by a later task); cannot build wasm." >&2
  exit 1
fi

# --out-dir is resolved relative to the crate directory. Use jsc-wasm's default
# feature set (do NOT pass --features): --no-default-features would strip it.
wasm-pack build "$CRATE_DIR" \
  --target web --release --out-dir "$OUT_DIR"

# Optional size report; guarded so a differing artifact name does not fail the build.
if [ -f "$WASM_FILE" ]; then
  raw_bytes=$(wc -c <"$WASM_FILE")
  gzip_bytes=$(gzip -9 -c "$WASM_FILE" | wc -c)
  echo
  printf '%-20s %12s %12s\n' "artifact" "raw (bytes)" "gzip (bytes)"
  printf '%-20s %12s %12s\n' "jsc_wasm_bg.wasm" "$raw_bytes" "$gzip_bytes"
fi
