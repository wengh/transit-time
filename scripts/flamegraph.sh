#!/usr/bin/env bash
set -euo pipefail

# Generate a CPU flamegraph of profile routing on a city.
# Requires: cargo install flamegraph
#
# Usage:
#   ./scripts/flamegraph.sh                                 # defaults: chicago, 10 runs, flamegraph.svg
#   OUT=fg.svg ./scripts/flamegraph.sh                      # custom output
#   CITY=transit-viz/public/data/nyc.bin LAT=40.75 LON=-73.99 RUNS=5 ./scripts/flamegraph.sh
#
# WSL note: Ubuntu's /usr/bin/perf is a wrapper that requires a
# linux-tools package matching the running kernel. If it errors with
# "could not spawn perf", point PERF at a working binary, e.g.:
#   PERF=/usr/lib/linux-tools/5.15.0-176-generic/perf ./scripts/flamegraph.sh

OUT="${OUT:-flamegraph.svg}"
CITY="${CITY:-transit-viz/public/data/chicago.bin}"
LAT="${LAT:-41.8781}"
LON="${LON:--87.6298}"
DATE="${DATE:-20260422}"
HHMM="${HHMM:-900}"
WINDOW_MIN="${WINDOW_MIN:-60}"
MAX_MIN="${MAX_MIN:-45}"
SLACK_S="${SLACK_S:-60}"
RUNS="${RUNS:-10}"

CARGO_PROFILE_RELEASE_DEBUG=true \
RUSTFLAGS="-C force-frame-pointers=yes" \
cargo flamegraph \
  -c "record -F 997 --call-graph dwarf,16384 -g" \
  --bin benchmark_smoke \
  -p transit-router \
  --release \
  -o "$OUT" \
  -- "$CITY" "$LAT" "$LON" "$DATE" "$HHMM" "$WINDOW_MIN" "$MAX_MIN" "$SLACK_S" "$RUNS"

echo "Flamegraph written to $OUT"
