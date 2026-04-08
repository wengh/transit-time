.PHONY: dev wasm clean data-all

# Source files for change detection
ROUTER_SRC := $(shell find transit-router/src -name '*.rs')
PREP_SRC := $(shell find transit-prep/src -name '*.rs')
WASM_OUT := transit-viz/pkg/transit_router_bg.wasm

CITY_FILES := $(wildcard cities/*.jsonc)
CITY_IDS := $(patsubst cities/%.jsonc,%,$(CITY_FILES))
BIN_FILES := $(addprefix transit-viz/public/data/, $(addsuffix .bin, $(CITY_IDS)))

# Build WASM (only when router source changes)
wasm: $(WASM_OUT)
$(WASM_OUT): $(ROUTER_SRC) transit-router/Cargo.toml .cargo/config.toml
	RUSTUP_TOOLCHAIN=nightly wasm-pack build transit-router --target web --out-dir ../transit-viz/pkg -- -Z build-std=panic_abort,std

# Build all data (skips up-to-date based on file timestamps)
data-all: $(BIN_FILES)

transit-viz/public/data/%.bin: $(PREP_SRC) transit-prep/Cargo.toml cities/%.jsonc
	@echo "Building data for $*..."
	cargo run --release -p transit-prep --bin transit-prep -- prep \
		--city-file cities/$*.jsonc \
		--output $@ \
		--cache-dir cache

# Full dev setup: build everything then start dev server
dev: $(WASM_OUT) data-all
	cd transit-viz && npm install --silent && npm run dev -- --port 5173

clean:
	cargo clean
	rm -rf transit-viz/pkg
	rm -f transit-viz/public/data/*.bin
