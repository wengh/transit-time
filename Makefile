.PHONY: dev wasm clean data-all data data-some flamegraph sizes

# Normalize: accept either lowercase (city=, cities=) or uppercase (CITY=, CITIES=)
CITY   ?= $(city)
CITIES ?= $(cities)

# Source files for change detection
ROUTER_SRC := $(shell find transit-router/src -name '*.rs')
WASM_OUT := transit-viz/pkg/transit_router_bg.wasm

# Build WASM (only when router source changes)
wasm: $(WASM_OUT)
$(WASM_OUT): $(ROUTER_SRC) transit-router/Cargo.toml .cargo/config.toml
	RUSTUP_TOOLCHAIN=nightly wasm-pack build transit-router --target web --out-dir ../transit-viz/pkg -- -Z build-std=panic_abort,std

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

# CPU flamegraph of profile routing (override via env: OUT, CITY, LAT, LON, RUNS, etc.)
flamegraph:
	./scripts/samply.sh

sizes:
	./scripts/sizes.py

clean:
	cargo clean
	rm -rf transit-viz/pkg
	rm -f transit-viz/public/data/*.bin
