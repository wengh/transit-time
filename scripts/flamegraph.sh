#!/usr/bin/env bash
set -euo pipefail

# Generate a CPU flamegraph of the transit router.
# Requires: cargo install flamegraph
#
# Usage:
#   ./scripts/flamegraph.sh              # default output: flamegraph.svg
#   ./scripts/flamegraph.sh out.svg      # custom output path

OUT="${1:-flamegraph.svg}"

RUSTFLAGS="-C force-frame-pointers=yes" \
cargo flamegraph \
  -c "record -g" \
  --bin profile \
  -p transit-router \
  --release \
  -o "$OUT"

echo "Flamegraph written to $OUT"
