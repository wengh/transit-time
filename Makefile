.PHONY: dev wasm data-chicago clean

# Source files for change detection
ROUTER_SRC := $(shell find transit-router/src -name '*.rs')
PREP_SRC := $(shell find transit-prep/src -name '*.rs')
WASM_OUT := transit-viz/pkg/transit_router_bg.wasm
CHICAGO_BIN := transit-viz/public/data/chicago.bin

# Build WASM (only when router source changes)
wasm: $(WASM_OUT)
$(WASM_OUT): $(ROUTER_SRC) transit-router/Cargo.toml
	wasm-pack build transit-router --target web --out-dir ../transit-viz/pkg

# Build Chicago data (only when prep source changes)
data-chicago: $(CHICAGO_BIN)
$(CHICAGO_BIN): $(PREP_SRC) transit-prep/Cargo.toml
	cargo run --release -p transit-prep -- \
		--city Chicago \
		--bbox="-87.94,41.64,-87.52,42.02" \
		--output $(CHICAGO_BIN) \
		--cache-dir cache

# Full dev setup: build everything then start dev server
dev: $(WASM_OUT) $(CHICAGO_BIN)
	cd transit-viz && npm install --silent && npm run dev

clean:
	cargo clean
	rm -rf transit-viz/pkg
