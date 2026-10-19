#!/usr/bin/env bash
# Fetch tc39/test262 at a pinned commit into <repo>/.tmp/test262 (idempotent).
set -euo pipefail

TEST262_SHA=2e0a56762801e275a9fdf96dc49d90ba0cddcf63
TEST262_URL=https://github.com/tc39/test262.git

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/.tmp/test262"

if [ -d "$dest/.git" ] && [ "$(git -C "$dest" rev-parse HEAD 2>/dev/null || true)" = "$TEST262_SHA" ]; then
  echo "test262 already at $TEST262_SHA in $dest"
  exit 0
fi

rm -rf "$dest"
mkdir -p "$dest"
git -C "$dest" init -q
git -C "$dest" remote add origin "$TEST262_URL"
git -C "$dest" fetch -q --depth 1 origin "$TEST262_SHA"
git -C "$dest" -c advice.detachedHead=false checkout -q --detach FETCH_HEAD
git -C "$dest" rev-parse HEAD
