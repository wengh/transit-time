# Transit Isochrone Tool

A transit travel-time isochrone visualizer. Given a point on a map and a departure time, it computes how long it takes to reach every other point by walking and public transit, then renders the result as a colored heatmap overlay.

The system has three components:

- **transit-prep** — Rust CLI that downloads GTFS + OSM data, builds a unified walking/transit graph, and serializes it to a compact binary format
- **transit-router** — Rust library compiled to WebAssembly that loads the binary and runs time-dependent Dijkstra shortest-path queries
- **transit-viz** — Web app (Vite + Leaflet) that renders isochrone heatmaps on an interactive map

## How It Works

### Data Preparation (`transit-prep`)

1. **GTFS download** — Fetches the city's transit feed from the [Mobility Database](https://mobilitydatabase.org/) API (stops, routes, trips, stop_times, calendars, frequencies)
2. **OSM download** — For large cities, downloads a PBF extract from [BBBike](https://download.bbbike.org/osm/); for small areas, queries the Overpass API. Extracts the pedestrian-walkable street network (footways, sidewalks, paths, corridors, crossings, residential streets, etc.)
3. **Graph construction** — Builds a walking graph from OSM data. Intersection and endpoint nodes become graph nodes; street segments become edges weighted by haversine distance. Subway entrance nodes (`railway=subway_entrance`) are detected and connected to the street network.
4. **Stop snapping** — Each GTFS transit stop is snapped to its nearest OSM graph node (max 400m). Stops near subway entrances (within 150m) are preferentially snapped to the entrance node, so routing naturally traverses mapped indoor corridors and stairways.
5. **Service patterns** — GTFS services are grouped by day-of-week bitmask (e.g. weekday, Saturday, Sunday). For each pattern, a direct-index event array is built: one slot per second of the day, containing all departures at that second with their destination stop, route, and travel time.
6. **Binary serialization** — Everything is serialized into a gzip-compressed binary format (header `TRNS` v1) containing nodes, edges, stops, stop-to-node mapping, route names, service patterns with event arrays, and route shapes.

### Routing (`transit-router`)

The router runs **time-dependent Dijkstra (TDD)** over the unified graph:

- **Walking edges** are weighted by distance / 1.4 m/s (~5 km/h walking speed)
- **Transit edges** are time-dependent — at each stop node, the router scans the event array for the next departure on each route after the current time
- **Transfer slack** — When switching between different transit routes, a configurable minimum transfer time (default 60s) is enforced to account for walking between platforms
- **Max trip duration** is capped at 2 hours
- The result is a single-source shortest-path tree: for every node in the graph, the arrival time, predecessor, and route taken

The router also supports **sampled mode**: running TDD at multiple departure times across a window (e.g. every 6 minutes over an hour) and averaging the results, smoothing out sensitivity to exact departure timing.

### Visualization (`transit-viz`)

The web app loads the WASM router and binary data, then:

- Renders each graph node as a colored dot on a canvas overlay (green = nearby, yellow/red = farther, up to 120 min)
- Click anywhere on the map to set the origin point
- Adjust departure time, service pattern (weekday/weekend), number of samples, and transfer slack via sliders
- Hover over any point to see its travel time

## Prerequisites

- **Rust** (stable, 1.70+)
- **wasm-pack** — `cargo install wasm-pack`
- **Node.js** (18+) and npm
- A **Mobility Database refresh token** — sign up at [mobilitydatabase.org](https://mobilitydatabase.org/) and save the token to `.mdb_refresh_token` in the repo root

## Quick Start

### 1. Prepare Data

```bash
# Chapel Hill, NC (small city, quick test)
cargo run --release -p transit-prep -- \
  --city-file cities/chapel_hill.jsonc \
  --output transit-viz/public/data/chapel_hill.bin \
  --cache-dir cache

# Chicago, IL (large city — downloads ~94 MB PBF)
cargo run --release -p transit-prep -- \
  --city-file cities/chicago.jsonc \
  --output transit-viz/public/data/chicago.bin \
  --cache-dir cache
```

Options:
| Flag | Description |
|------|-------------|
| `--city-file` | Path to city JSON config (e.g. `cities/chicago.jsonc`) |
| `--output` | Output binary file path (default: `city.bin`) |
| `--cache-dir` | Directory to cache downloaded GTFS/OSM files (default: `cache`) |
| `--token-file` | Path to MDB refresh token file (default: `.mdb_refresh_token`) |

Downloaded GTFS zips and OSM data are cached in `--cache-dir`, so subsequent runs skip the download step.

### 2. Build the WASM Module

```bash
cd transit-router
wasm-pack build --target web --out-dir ../transit-viz/pkg
```

This produces `transit-viz/pkg/transit_router.js` and `transit_router_bg.wasm`.

### 3. Run the Web App

```bash
cd transit-viz
npm install
npm run dev
```

Open `http://localhost:3000`. Click on the map to set an origin and see the isochrone.

## Running Tests

Tests use real cached data (no mocking). Run the data prep step first to populate the cache, then:

```bash
# Unit + integration tests for data prep
cargo test -p transit-prep -- --nocapture

# Chapel Hill routing tests
cargo test -p transit-router --test integration_test -- --nocapture

# Chicago routing tests (requires chicago.bin in cache/)
cargo test -p transit-router --test chicago_test -- --nocapture
```

The Chicago test verifies a specific route from the West Side to the Loop:
- Origin: (41.896, -87.778) — near Austin Blvd
- Destination: (41.884, -87.629) — near LaSalle/Wacker
- Departure: 11:10 AM Thursday
- Expected: Walk → Bus 91 → Green Line → Walk, ~45 min

## Project Structure

```
transit-time/
├── transit-prep/           # Data preparation CLI (Rust)
│   └── src/
│       ├── main.rs         # CLI entry point
│       ├── mdb.rs          # Mobility Database API client
│       ├── osm.rs          # OSM data fetcher (Overpass + BBBike PBF)
│       ├── gtfs.rs         # GTFS parser + service pattern builder
│       ├── graph.rs        # OSM graph builder (XML + PBF) + stop snapping
│       └── binary.rs       # Binary format serialization
├── transit-router/         # Routing engine (Rust → WASM)
│   └── src/
│       ├── lib.rs          # WASM bindings (wasm-bindgen)
│       ├── router.rs       # Time-dependent Dijkstra implementation
│       └── data.rs         # Binary format deserialization
├── transit-viz/            # Web visualization (Vite + Leaflet)
│   ├── index.html          # Map UI with controls
│   ├── src/main.js         # WASM integration + canvas rendering
│   ├── public/data/        # Place city.bin here
│   └── pkg/                # WASM build output (from wasm-pack)
└── cache/                  # Cached downloads (gitignored)
```

## Binary Format

The `.bin` file is gzip-compressed with the following structure:

| Section | Contents |
|---------|----------|
| Header | Magic `TRNS`, version 1, counts for each section |
| Nodes | OSM node ID, lat, lon, is_entrance flag |
| Edges | Source/dest node indices, distance in meters |
| Stops | GTFS stop name, lat, lon, stop index |
| Stop-to-Node | Mapping from stop indices to graph node indices |
| Route Names | Short names for each route |
| Patterns | Day-of-week bitmask, event arrays (one slot per second), frequency routes |
| Shapes | Route shape polylines (lat/lon sequences) |

## License

MIT
