#!/usr/bin/env bash
set -euo pipefail

# Sampling profile of routing on a city using samply.
# Produces a Firefox Profiler JSON with per-line source attribution (inlined
# frames preserved, unlike flamegraph.sh).
#
# Requires: cargo install samply
#
# Usage:
#   ./scripts/samply.sh                     # defaults: chicago, 10 runs, opens UI
#   OUT=prof.json.gz ./scripts/samply.sh
#   NO_OPEN=1 ./scripts/samply.sh           # record only, don't launch browser
#   RATE=4000 ./scripts/samply.sh           # higher sampling rate
#   CITY=transit-viz/public/data/nyc.bin LAT=40.75 LON=-73.99 RUNS=5 ./scripts/samply.sh
#   CITY=transit-viz/public/data/paris.bin LAT=48.862305 LON=2.344500 ./scripts/samply.sh

OUT="${OUT:-profile.json.gz}"
CITY="${CITY:-transit-viz/public/data/chicago.bin}"
LAT="${LAT:-41.8781}"
LON="${LON:--87.6298}"
DATE="${DATE:-20260422}"
HHMM="${HHMM:-900}"
WINDOW_MIN="${WINDOW_MIN:-60}"
MAX_MIN="${MAX_MIN:-45}"
SLACK_S="${SLACK_S:-60}"
RUNS="${RUNS:-10}"
RATE="${RATE:-4000}"

OPEN_FLAG=""
if [[ -n "${NO_OPEN:-}" ]]; then
  OPEN_FLAG="--no-open --save-only"
fi

CARGO_PROFILE_RELEASE_DEBUG=true \
RUSTFLAGS="-C force-frame-pointers=yes" \
cargo build \
  --bin benchmark_smoke \
  -p transit-router \
  --release

BIN="target/release/benchmark_smoke"

# shellcheck disable=SC2086
samply record \
  --rate "$RATE" \
  -o "$OUT" \
  $OPEN_FLAG \
  -- "$BIN" "$CITY" "$LAT" "$LON" "$DATE" "$HHMM" "$WINDOW_MIN" "$MAX_MIN" "$SLACK_S" "$RUNS"

echo "Profile written to $OUT"
echo "To view later: samply load $OUT"
