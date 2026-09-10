#!/bin/sh
# Bundle the modular desktop UI into one ESM file for the Tauri shell and the
# static preview (ESM keeps the top-level await in main.js legal; both hosts
# load it as a module). No install needed (`npx` fetches a pinned esbuild
# into the npm cache); commit the output so fresh clones work without node.
# Usage: sh scripts/build_ui.sh
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
npx --yes esbuild@0.21.5 "$ROOT/crates/herdr-desktop/ui/main.js" \
  --bundle --format=esm \
  --outfile="$ROOT/crates/herdr-desktop/ui/dist/bundle.js"
node --check "$ROOT/crates/herdr-desktop/ui/dist/bundle.js"
echo "bundled $(wc -c < "$ROOT/crates/herdr-desktop/ui/dist/bundle.js") bytes"
