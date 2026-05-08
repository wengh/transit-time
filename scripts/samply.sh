#!/usr/bin/env bash
set -euo pipefail

# Sampling profile of routing on a city using samply.
# Produces a Firefox Profiler JSON with per-line source attribution (inlined
# frames preserved, unlike flamegraph.sh).
#
# Defaults to a PGO-optimized build (matches what `make wasm` deploys), so
# the flamegraph reflects production-shape inlining and branch layout. Pass
# NO_PGO=1 to profile the unoptimized release build for comparison.
#
# Requires: cargo install samply
#
# Usage:
#   ./scripts/samply.sh                     # defaults: chicago, 1 run, opens UI
#   OUT=prof.json.gz ./scripts/samply.sh
#   NO_OPEN=1 ./scripts/samply.sh           # record only, don't launch browser
#   NO_PGO=1 ./scripts/samply.sh            # plain release build, no PGO
#   RATE=4000 ./scripts/samply.sh           # higher sampling rate
#   CITY=transit-viz/public/data/nyc.bin LAT=40.75 LON=-73.99 RUNS=5 ./scripts/samply.sh
#   CITY=transit-viz/public/data/paris.bin LAT=48.862305 LON=2.344500 ./scripts/samply.sh

OUT="${OUT:-profile.json.gz}"
CITY="${CITY:-transit-viz/public/data/chicago.bin}"
LAT="${LAT:-41.8781}"
LON="${LON:--87.6298}"
DATE="${DATE:-$(date +%Y%m%d)}"
HHMM="${HHMM:-0}"
WINDOW_MIN="${WINDOW_MIN:-1620}"
MAX_MIN="${MAX_MIN:-90}"
SLACK_S="${SLACK_S:-60}"
RUNS="${RUNS:-1}"
RATE="${RATE:-4000}"
PROFDATA="${PROFDATA:-target/pgo-data/merged.profdata}"

OPEN_FLAG=""
if [[ -n "${NO_OPEN:-}" ]]; then
  OPEN_FLAG="--no-open --save-only"
fi

PGO_FLAGS=""
TARGET_DIR="target/pgo-samply"
if [[ -z "${NO_PGO:-}" ]]; then
    if [[ ! -f "$PROFDATA" ]]; then
        echo "[samply] PGO profile missing — training now"
        ./scripts/pgo-train.sh "$PROFDATA"
    fi
    PGO_FLAGS="-C profile-use=$(realpath "$PROFDATA") -C llvm-args=-pgo-warn-missing-function=false"
fi

CARGO_PROFILE_RELEASE_DEBUG=true \
RUSTFLAGS="-C force-frame-pointers=yes $PGO_FLAGS" \
cargo build \
  --bin benchmark_smoke \
  -p transit-router \
  --release \
  --target-dir "$TARGET_DIR"

BIN="$TARGET_DIR/release/benchmark_smoke"

# shellcheck disable=SC2086
samply record \
  --rate "$RATE" \
  -o "$OUT" \
  $OPEN_FLAG \
  -- "$BIN" "$CITY" "$LAT" "$LON" "$DATE" "$HHMM" "$WINDOW_MIN" "$MAX_MIN" "$SLACK_S" "$RUNS"

echo "Profile written to $OUT"
echo "To view later: samply load $OUT"
