#!/usr/bin/env bash
# Trains a PGO profile by running benchmark_smoke against a representative
# routing query, then merges the .profraw output to the path given as $1.
#
# Used by `make wasm-pgo`. Standalone usage:
#   ./scripts/pgo-train.sh target/pgo-data/merged.profdata
#
# Empirically, one routing query is enough.
# Adding more cities or runs did not improve the
# resulting WASM speedup beyond noise.
#
# Requires: rustup component add llvm-tools-preview

set -euo pipefail

OUT="${1:?usage: $0 <output.profdata>}"
TRAIN_CITY="${TRAIN_CITY:-transit-viz/public/data/chicago.bin}"
TRAIN_LAT="${TRAIN_LAT:-41.8781}"
TRAIN_LON="${TRAIN_LON:--87.6298}"
TRAIN_DATE="${TRAIN_DATE:-$(date +%Y%m%d)}"
TRAIN_HHMM="${TRAIN_HHMM:-0}"
TRAIN_WINDOW_MIN="${TRAIN_WINDOW_MIN:-1620}"
TRAIN_MAX_MIN="${TRAIN_MAX_MIN:-90}"
TRAIN_SLACK_S="${TRAIN_SLACK_S:-60}"

if [[ ! -f "$TRAIN_CITY" ]]; then
    echo "PGO training city not found: $TRAIN_CITY" >&2
    echo "Build it first: make data CITY=chicago" >&2
    exit 1
fi

LLVM_PROFDATA="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's|host: ||p')/bin/llvm-profdata"
if [[ ! -x "$LLVM_PROFDATA" ]]; then
    echo "llvm-profdata not found at $LLVM_PROFDATA" >&2
    echo "Run: rustup component add llvm-tools-preview" >&2
    exit 1
fi

mkdir -p "$(dirname "$OUT")"
OUT_ABS="$(cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")"
RAW_DIR="$(dirname "$OUT_ABS")/raw"
INSTRUMENT_DIR="target/pgo-instrument"

rm -rf "$RAW_DIR"
mkdir -p "$RAW_DIR"

echo "[pgo-train] building instrumented benchmark_smoke"
RUSTFLAGS="-Cprofile-generate=$RAW_DIR" \
    cargo build --release --bin benchmark_smoke -p transit-router \
    --target-dir "$INSTRUMENT_DIR"

# RUSTFLAGS=-Cprofile-generate is applied to every crate in the dep graph,
# including build.rs scripts and proc-macro crates that *execute* during
# `cargo build`. Those executions deposit profraw files into RAW_DIR before
# the training query runs. Wipe them so the merged profile reflects only
# benchmark_smoke's routing workload, not cargo's build-time machinery.
rm -rf "$RAW_DIR"
mkdir -p "$RAW_DIR"

echo "[pgo-train] running training query on $TRAIN_CITY"
"$INSTRUMENT_DIR/release/benchmark_smoke" \
    "$TRAIN_CITY" "$TRAIN_LAT" "$TRAIN_LON" \
    "$TRAIN_DATE" "$TRAIN_HHMM" "$TRAIN_WINDOW_MIN" \
    "$TRAIN_MAX_MIN" "$TRAIN_SLACK_S" 1 >/dev/null

echo "[pgo-train] merging $(ls "$RAW_DIR"/*.profraw | wc -l) .profraw files"
"$LLVM_PROFDATA" merge -o "$OUT_ABS" "$RAW_DIR"
echo "[pgo-train] wrote $OUT_ABS ($(du -h "$OUT_ABS" | cut -f1))"
