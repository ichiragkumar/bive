#!/bin/sh
# Typecheck (strict) + bundle the modular desktop UI into one ESM file for the
# Tauri shell and the static preview (ESM keeps the top-level await in main.ts
# legal; both hosts load it as a module). No install needed (`npx` fetches
# pinned tools into the npm cache); commit the output so fresh clones work
# without node.
# Usage: sh scripts/build_ui.sh
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
npx --yes --package typescript@5.6.3 tsc --noEmit -p "$ROOT/crates/herdr-desktop/ui/tsconfig.json"
npx --yes --package esbuild@0.21.5 esbuild "$ROOT/crates/herdr-desktop/ui/src/main.ts" \
  --bundle --format=esm \
  --outfile="$ROOT/crates/herdr-desktop/ui/dist/bundle.js"
node --check "$ROOT/crates/herdr-desktop/ui/dist/bundle.js"
echo "bundled $(wc -c < "$ROOT/crates/herdr-desktop/ui/dist/bundle.js") bytes"
