.PHONY: dev wasm clean data-all data data-some flamegraph sizes

# Normalize: accept either lowercase (city=, cities=) or uppercase (CITY=, CITIES=)
CITY   ?= $(city)
CITIES ?= $(cities)

# Source files for change detection
ROUTER_SRC := $(shell find transit-router/src -name '*.rs')
WASM_OUT := transit-viz/pkg/transit_router_bg.wasm
PROFDATA := target/pgo-data/merged.profdata

# WASM rustflags for the PGO build. Mirrors target.wasm32-unknown-unknown.rustflags
# in .cargo/config.toml — keep these in sync. Cargo's rustflags arrays from
# different config sources don't merge (first source wins), so the PGO build
# must inline the full set via `--config`.
WASM_RUSTFLAGS_PGO := "-C","target-feature=+atomics,+bulk-memory,+mutable-globals,+simd128","-C","link-arg=--shared-memory","-C","link-arg=--max-memory=4294967296","-C","link-arg=--import-memory","-C","link-arg=--export=__wasm_init_tls","-C","link-arg=--export=__tls_size","-C","link-arg=--export=__tls_align","-C","link-arg=--export=__tls_base","-C","link-arg=--export=__heap_base","-C","link-arg=--export=__data_end","-C","profile-use=$(abspath $(PROFDATA))","-C","llvm-args=-pgo-warn-missing-function=false"

# Build PGO-optimized WASM. Requires transit-viz/public/data/chicago.bin
# (run `make data CITY=chicago` first if missing). Trains a native profile
# by running benchmark_smoke against Chicago, then builds WASM with
# -Cprofile-use. ~17% faster routing than a plain build.
wasm: $(WASM_OUT)
$(WASM_OUT): $(ROUTER_SRC) transit-router/Cargo.toml .cargo/config.toml Makefile $(PROFDATA)
	RUSTUP_TOOLCHAIN=nightly wasm-pack build transit-router --target web --out-dir ../transit-viz/pkg -- -Z build-std=panic_abort,std --config 'target.wasm32-unknown-unknown.rustflags=[$(WASM_RUSTFLAGS_PGO)]'

$(PROFDATA): $(ROUTER_SRC) transit-router/Cargo.toml scripts/pgo-train.sh
	./scripts/pgo-train.sh $@

# Build all data via pipeline (checks feeds, downloads stale, rebuilds affected)
data-all:
	cargo run --release -p transit-prep --bin transit-prep -- pipeline \
		--cities-dir cities/ \
		--output-dir transit-viz/public/data/ \
		--cache-dir cache

# Build data for one city, e.g. `make data city=montreal`
data:
	@test -n "$(CITY)" || (echo "Usage: make data city=montreal" && exit 1)
	cargo run --release -p transit-prep --bin transit-prep -- prep \
		--city-file cities/$(CITY).jsonc \
		--output transit-viz/public/data/$(CITY).bin \
		--cache-dir cache

# Build data for a selected set of cities, e.g. `make data-some cities='montreal boston'`
data-some:
	@test -n "$(CITIES)" || (echo "Usage: make data-some cities='montreal boston'" && exit 1)
	for city in $(CITIES); do \
		cargo run --release -p transit-prep --bin transit-prep -- prep \
			--city-file cities/$$city.jsonc \
			--output transit-viz/public/data/$$city.bin \
			--cache-dir cache || exit 1; \
	done

# Dev setup: build city=..., cities='...', or everything by default
dev: $(WASM_OUT)
	@if [ -n "$(CITY)" ]; then \
		$(MAKE) data CITY="$(CITY)"; \
	elif [ -n "$(CITIES)" ]; then \
		$(MAKE) data-some CITIES="$(CITIES)"; \
	else \
		$(MAKE) data-all; \
	fi
	cd transit-viz && npm install --silent && npm run dev -- --port 5173

# CPU flamegraph of profile routing (override via env: OUT, CITY, LAT, LON, RUNS, NO_PGO, etc.)
# When NO_PGO is unset, depends on $(PROFDATA) so source changes retrain
# before sampling; with NO_PGO=1 the dep is skipped (no wasted training).
flamegraph: $(if $(NO_PGO),,$(PROFDATA))
	./scripts/samply.sh

sizes:
	./scripts/sizes.py

clean:
	cargo clean
	rm -rf transit-viz/pkg
	rm -rf target/pgo-data target/pgo-instrument target/pgo-samply
	rm -f transit-viz/public/data/*.bin
