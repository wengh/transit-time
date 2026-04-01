.PHONY: dev wasm clean data-all

# Source files for change detection
ROUTER_SRC := $(shell find transit-router/src -name '*.rs')
PREP_SRC := $(shell find transit-prep/src -name '*.rs')
WASM_OUT := transit-viz/pkg/transit_router_bg.wasm

CITY_FILES := $(wildcard cities/*.json)
CITY_IDS := $(patsubst cities/%.json,%,$(CITY_FILES))
BIN_FILES := $(addprefix transit-viz/public/data/, $(addsuffix .bin, $(CITY_IDS)))

# Build WASM (only when router source changes)
wasm: $(WASM_OUT)
$(WASM_OUT): $(ROUTER_SRC) transit-router/Cargo.toml
	RUSTUP_TOOLCHAIN=nightly wasm-pack build transit-router --target web --out-dir ../transit-viz/pkg -- -Z build-std=panic_abort,std

# Build all data
data-all: $(BIN_FILES)

transit-viz/public/data/%.bin: $(PREP_SRC) transit-prep/Cargo.toml cities/%.json
	@echo "Building data for $*..."
	@BBOX=$$(node -e "console.log(require('./cities/$*.json').bbox)"); \
	PREP_CITY=$$(node -e "console.log(require('./cities/$*.json').prep_city)"); \
	FEED_IDS=$$(node -e "console.log((require('./cities/$*.json').feed_ids || []).join(','))"); \
	if [ -n "$$FEED_IDS" ]; then \
		cargo run --release -p transit-prep -- \
			--city "$$PREP_CITY" \
			--feed-ids "$$FEED_IDS" \
			--bbox="$$BBOX" \
			--output $@ \
			--cache-dir cache; \
	else \
		cargo run --release -p transit-prep -- \
			--city "$$PREP_CITY" \
			--bbox="$$BBOX" \
			--output $@ \
			--cache-dir cache; \
	fi

# Full dev setup: build everything then start dev server
dev: $(WASM_OUT) data-all
	cd transit-viz && npm install --silent && npm run dev -- --port 5173

clean:
	cargo clean
	rm -rf transit-viz/pkg
	rm -f transit-viz/public/data/*.bin
