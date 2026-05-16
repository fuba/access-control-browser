#!/usr/bin/env bash
# Build the Next.js UI bundle that the daemon embeds via rust-embed.
# Run this before `cargo build` whenever ui/ changes.
set -euo pipefail
cd "$(dirname "$0")/../ui"
if [ ! -d node_modules ]; then
  npm ci
fi
npm run build
echo "ui/out/ rebuilt"
