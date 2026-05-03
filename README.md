# Transit Isochrone Tool

A browser-based tool that shows how far you can travel from any point on a map using public transit and walking, at any time of day. The entire routing computation runs inside your browser — no server required.

Live demo: https://transit-time.pages.dev/

Note: this project is mostly vibe coded.
- `transit-router/src/profile.rs` is mostly manually written. All AI changes in this file are thoroughly reviewed.
- Other parts of the codebase are mostly written by AI. Only the high level design and architecture are driven by human.

### Screenshots
#### Paths from Millennium Park to University of Chicago
<img width="1255" height="1250" alt="paths from Millennium Park to University of Chicago" src="https://github.com/user-attachments/assets/63f3b320-c413-475f-86ae-4f8f309cc6dc" />

## Using the tool

### Picking a city

The landing page lists available cities. Click one to load it. The city's transit and street data (up to ~20 MB compressed depending on city size) downloads and loads in the browser; a progress bar shows the download, then a brief indexing phase.

### Setting an origin

**Desktop:** Double-click anywhere on the map to set your starting point. A pin appears snapped to the nearest walkable street node.

**Mobile:** Use the **Origin / Dest** toggle in the top bar. Tap "Origin", then tap anywhere on the map to set your starting point. The tool automatically switches to "Dest" mode after the origin is set.

Once an origin is set, the map fills with a color-coded isochrone overlay. Green/yellow areas are reachable quickly; the color shifts through orange and red as travel time increases, fading out where nothing is reachable within the time limit.

The overlay also encodes how consistently a location is reachable across the departure window. Locations reachable from every departure within the window use the warm (yellow→red) scale. Locations reachable from only some departures shift toward cool colors — cyan for nearby spots that are only sometimes served, through blue and purple for farther or less reliably served locations. Locations never reachable within the window are not shown.

### Exploring destinations

**Desktop:** Move your cursor over the map. The route from your origin to the point under the cursor is drawn on the map — walk segments as gray dashed lines, transit segments colored by route. A panel appears showing the travel time and the step-by-step itinerary. Single-click to pin a destination so the route stays visible while you adjust controls; click again to unpin.

**Mobile:** Ensure the toggle is set to **Dest**, then tap anywhere on the map to pin a destination. A bottom sheet appears showing the travel time summary. Drag the handle or tap the sheet to expand it and see the full itinerary and sawtooth chart. Tap "Clear" in the sheet or tap another location to pin a new destination.

### Controls

All controls re-run the routing query immediately when changed.

**Mobile:** Access controls by tapping the gear icon in the top bar, which opens a settings sheet.

**Date** — select any calendar date. The tool activates only the transit schedules valid on that date (weekday, weekend, or holiday service), and shows how many service patterns are active.

**Departure window** — dual-ended slider from midnight to midnight in 5-minute steps. The router computes travel times across the selected window, smoothing out the "lucky timing" effect of any single departure. Long windows are split internally and evaluated in parallel; the UI shows how many worker threads were used when the query finishes.

**Max travel time** — caps the search at 10–180 minutes. Locations unreachable within this limit are not shown.

**Transfer slack** — minimum connection time required when switching between transit vehicles (0–300 seconds, default 60 s). A higher value avoids tight transfers that might be missed in practice.

### Sawtooth chart

When a destination is pinned, a chart appears below the itinerary. The X-axis is departure time within the selected window; the Y-axis is travel time to the destination. Each diagonal line represents one transit trip: as your departure time gets later and closer to when the vehicle leaves the stop, your wait shrinks and total travel time decreases — that is the downward slope. When you depart late enough to miss that vehicle, travel time jumps up because you must wait for the next one, forming the sawtooth pattern. The dashed horizontal line (if present) is the walk-only time. Shaded grey columns mark departure times where no transit option falls within the travel-time limit.

Hover over a transit segment to highlight that departure and see its route on the map; click to lock it.

### Copying trip info

When a destination is pinned, a "Copy info" button appears. It copies a plain-text summary of the origin, destination, settings, and itinerary to the clipboard.

---

## Data flow

The pipeline has two stages: offline preprocessing and in-browser routing.

### Offline: building city data files

A Rust preprocessing tool (`transit-prep`) takes a city configuration (a `.jsonc` file in `cities/`) and produces a single self-contained `.bin` file for that city.

The city config specifies:
- One or more GTFS feeds, either as Transitland onestop IDs (e.g. `f-dp3-cta`) or direct URLs
- An OpenStreetMap source for pedestrian street data (BBBike extract name or direct PBF URL)
- Display metadata (name, map center, zoom)

The preprocessor downloads and caches both the GTFS feeds and the OSM extract. For Transitland feeds, it tracks the latest feed version SHA1 and only re-downloads when a new version is published. It then performs the following steps:

1. **Parse GTFS** — reads stops, routes, trips, stop times, service calendars, and shapes from the zip archives. Filters stops to the bounding box. Drops trips with fewer than two in-bbox stops and removes their shapes. Trims remaining shapes to the bounding box. Warns if feed data has expired. For cities with multiple feeds (e.g. Chicago's CTA/Pace/Metra), feeds are merged in a single pass: each feed's stop, route, trip, and service IDs are prefixed with the current total stop count (e.g. `42:stop_id`) to prevent ID collisions across feeds.

2. **Parse OSM and build street graph** — extracts the pedestrian-walkable street network (footways, paths, sidewalks, crossings, and roads that allow foot traffic) within the bounding box. The raw node/way data is then reduced to a proper graph: only intersection nodes (used by two or more ways) become graph vertices; intermediate nodes are discarded and their traversed distance accumulated into edge weights. Finally, small disconnected components with fewer than 50 nodes are removed — typically isolated fragments on the wrong side of a fence or elevated structure that cannot realistically be reached on foot.

3. **Snap stops to street nodes** — each transit stop is matched to the nearest point on the street network by inserting a virtual node on the nearest edge and connecting it. This lets the router walk from any street point directly to any stop.

4. **Compact the walk graph** — three passes shrink the routing graph without changing routing distances:
   - *Prune unreachable nodes:* a breadth-first search from every snapped stop node identifies all street nodes reachable on foot from transit. Nodes and edges outside that reachable set are removed and all indices remapped, discarding dead-end pedestrian areas disconnected from the transit network.
   - *Prune leaf nodes:* iteratively remove non-stop nodes with only one neighbor — driveways, dead-end footway stubs, etc. No shortest path can pass through them. Stops are protected as anchors.
   - *Collapse degree-2 chains:* maximal chains of non-stop nodes with exactly two neighbors are contracted into single edges whose weight is the sum of the chain. This is distance-perfect for routing — every node on such a chain has no choice but to walk the whole chain in order. The pass iterates until stable, since dedup of parallel chains between the same anchor pair (e.g. a pedestrian island with two separately-mapped sides) can leave a previously deg-3 anchor with degree 2 for the next pass to absorb. Typical reduction: 25–35% fewer graph nodes, 20–30% fewer edges, 20–25% faster Dijkstra at query time, with travel-time results bit-for-bit identical.

5. **Build service patterns and extract leg shapes** — trips that share the same stop sequence and service calendar are grouped into a pattern. For each pattern, stop times are stored as a sorted array of time offsets per stop, enabling binary-search-based lookup during routing. Frequency-based routes (trips defined by headway rather than fixed times) are stored separately. For trips that include GTFS shape data, per-leg polylines are extracted: for each (route, from-stop, to-stop) pair, a dynamic-programming subsequence match aligns the shape point sequence to the stop pair, handling reversed routes and partial alignments. The best-aligned shape for each leg is kept. After pattern construction, routes and stops not referenced in any event are removed and indices remapped, keeping the binary compact.

6. **Serialize** — all data is written to a custom binary format, with several layers of sorting applied to improve both compression ratios and runtime locality.

   *Node ordering:* nodes are reordered along a Morton (Z-order) space-filling curve before writing. Because the SFC maps 2D geographic proximity to 1D index proximity, consecutive nodes in the array tend to be geographic neighbors, and their latitude/longitude values form nearly-monotone sequences. The coordinates are stored as fixed-point 32-bit integers (0.1 m resolution) rather than 64-bit floats, reducing raw size by 4×, and the two columns (latitudes, longitudes) are Pcodec-compressed separately — the small deltas between neighboring values compress extremely well. ([Pcodec](https://github.com/pcodec/pcodec) is a library for lossless compression of numerical sequences, featuring delta encoding, etc.)

   *Edge encoding:* the SFC reordering also benefits edges. Each undirected edge is stored canonically with the higher-numbered endpoint first and encoded as a `(u, delta)` pair where `delta = u - v`. Because nearby nodes in SFC order tend to be connected, `delta` values are typically small. Edges are sorted by `(u, delta)` and the three columns (endpoints, deltas, distances) are Pcodec-compressed. Edge distances are stored directly in walk times (at 1.4 m/s) as 16-bit integers.

   *Event ordering:* the events in each service pattern are sorted by `(stop_index, time_offset)` — all events at a given stop contiguous, chronological within the group. This lets the router binary-search by time within each stop's bucket at query time, and the sorted columns compress efficiently with Pcodec. All numeric event columns (time offsets, stop indices, travel times, chain pointers, bucket offsets, route labels) and shape coordinates are Pcodec-compressed.

   The assembled binary is then gzip-compressed for transfer; the browser decompresses it on the fly during download.

### In-browser: routing and rendering

The city `.bin` file is fetched and streamed through the browser's native gzip decompression. The decompressed bytes are handed to a WebAssembly module compiled from the Rust routing engine.

**Loading and indexing:** The WASM module decodes all Pcodec-compressed sections and builds two additional in-memory structures: a flat (jagged array) adjacency list for the street graph, and a spatial grid index over all nodes for snapping a clicked lat/lon to the nearest node. Because nodes are stored in SFC order, walking a neighborhood during the search accesses nodes that are close together in both geography and memory, improving cache locality.

**Routing — profile search over a departure window:** When an origin is set, the router computes a Pareto frontier of (departure, arrival) pairs for every reachable node across the selected departure window. The public routing interface uses a split-window implementation: it divides long windows into a small number of subqueries, generally one per available worker thread, while enforcing a 15-minute minimum chunk size and the internal maximum chunk size required by the profile engine's compact time deltas. A shared query index is built once for the active service patterns and walk-only distances from the source, then each subquery uses the single-window profile router and the wrapper merges the per-node totals and path frontiers transparently. Walking edges have a fixed cost based on distance at 1.4 m/s. At each node that is a transit stop, the router scans the event arrays for all active service patterns and considers every boarding that sits on the frontier; a transfer between different vehicles requires at least the configured transfer slack. From the frontier the router derives, per node, the mean travel time and the fraction of departures within the window from which the node is reachable — the two quantities the overlay and hover summaries display. A compact set of representative (Pareto-optimal) departures is retained for path reconstruction on hover, which drives the sawtooth chart and the itinerary view.

**Rendering:** After routing, each node's travel time is sent to a WebGL shader that maps it to a color. Points are rendered onto an offscreen canvas at a size proportional to the map zoom level, producing a continuous-looking coverage surface. The canvas is then composited onto the Leaflet map as an image overlay. Route polylines are drawn using the GTFS shape data where available, falling back to straight-line segments between stops.

---

## Building

**Prerequisites:**
- Rust (nightly toolchain, for the WASM build)
- [wasm-pack](https://rustwasm.github.io/wasm-pack/)
- Node.js and npm
- A [Transitland API key](https://www.transit.land/) in `.env` as `TRANSITLAND_API_KEY` (needed for building city data that uses Transitland feeds)

**Build the WASM module** (only needed when the routing logic changes):
```
make wasm
```

**Build city data files** (checks for stale feeds, downloads updates, rebuilds affected cities):
```
make data-all
```

This runs the pipeline which: extracts feed IDs from all city configs, checks Transitland for updated feed versions (via SHA1 comparison, skipping feeds checked within the last 2 days), downloads only stale or missing GTFS/OSM data in parallel, and rebuilds only affected city `.bin` files in parallel. Orphaned cache files from removed cities/feeds are cleaned up automatically.

Individual cities can be built with:
```
cargo run --release -p transit-prep -- prep --city-file cities/chicago.jsonc --output transit-viz/public/data/chicago.bin
```

**Start the development server** (builds everything if needed, then serves on port 5173):
```
make dev
```

**Production build:**
```
cd transit-viz && npm run build
```
The output in `transit-viz/dist/` is a fully static site that can be deployed anywhere.

### Formatting

`cargo fmt` formats Rust; `prettier` formats the frontend (TS/TSX/CSS/HTML/JSON/MD inside `transit-viz/`).

A [`pre-commit`](https://pre-commit.com/) hook auto-formats staged files. Install once:

```
pipx install pre-commit   # or: brew install pre-commit
pre-commit install
```

The hooks run `cargo fmt` and `prettier` from your local toolchain (`language: system`), so they don't build any isolated environments. CI also enforces these via `.github/workflows/format.yml` — to check manually:

```
cargo fmt --all -- --check
cd transit-viz && npm run format:check
```

### Adding a city

The easiest way is to auto-generate a config from a BBBike city name or OSM PBF URL:

```
cargo run --release -p transit-prep -- generate \
  --id my_city --bbbike-name MyCity --output cities/my_city.jsonc
```

This downloads the OSM extract, reads its bounding box, queries Transitland for all transit feeds in that area, and writes a `.jsonc` config with Transitland feed IDs and operator name comments. Edit the generated file to fill in `name`, `detail`, `tags`, and remove any unwanted feeds.

You can also create a `.jsonc` file manually:

```jsonc
{
  "id": "my_city",              // used in the URL path
  "name": "My City, ST",        // display name
  "file": "my_city.bin",        // output data file name
  "feed_ids": [
    "f-dp3-cta",                     // Transitland onestop ID
    "https://example.com/gtfs.zip"   // or a direct GTFS feed URL
  ],
  "bbox": "-80.0,43.0,-79.0,44.0",  // min_lon,min_lat,max_lon,max_lat
  "bbbike_name": "MyCity",      // BBBike extract name (for OSM data), OR
  // "osm_url": "https://...",  // direct URL to an OSM PBF file
  "center": [43.65, -79.38],    // map center [lat, lon]
  "zoom": 12,                   // initial zoom level
  "detail": "Agency A, Agency B", // shown in city list
  "allow_stale": null            // stale-policy override: null = auto (default),
                                 //   false = honor dates strictly,
                                 //   true  = force-wipe all service date ranges
}
```

Feed IDs can be Transitland onestop IDs (e.g. `f-dp3-cta`) or direct GTFS zip URLs. Transitland feeds are checked for updates automatically via SHA1 comparison. OSM pedestrian data is fetched from BBBike by name, or from a direct URL if `osm_url` is given. Then run `make data-all` to build the `.bin` file.

### CI/CD pipeline

The GitHub Actions workflow (`.github/workflows/deploy.yml`) runs on every push to `main`, on a weekly Sunday-at-03:00-UTC schedule to pick up fresh GTFS feeds, and can be triggered manually. Only one deployment runs at a time; a new push cancels any in-flight run.

The deploy job has four phases:

1. **WASM** — builds the routing engine with `make wasm` (nightly Rust + wasm-pack). The output is cached by source hash of `transit-router/` and rebuilt only on changes.
2. **Data** — runs `transit-prep pipeline --check-only` to query Transitland for updated SHA1 hashes (without downloading anything). If any feed is stale or a `.bin` file is missing, the job restores the raw GTFS/OSM download cache and runs `make data-all` to rebuild affected cities. If everything is current it skips this step entirely.
3. **Frontend** — installs npm dependencies and runs `npm run build` to produce the static site in `transit-viz/dist/`.
4. **Deploy** — publishes `transit-viz/dist/` to Cloudflare Pages via `wrangler pages deploy`.

Cloudflare deployment requires these GitHub repository secrets:

- `CLOUDFLARE_API_TOKEN`
- `CLOUDFLARE_ACCOUNT_ID`
- `CLOUDFLARE_PAGES_PROJECT_NAME`

The frontend includes `transit-viz/public/_headers` (COOP/COEP for WASM threads) and `transit-viz/public/_redirects` (SPA routing fallback); both are copied into `dist/` during build and applied by Cloudflare Pages. Note that we don't use GitHub Pages because it doesn't support custom headers, which are required for WebAssembly threads.

---

## Performance

The numbers below are from a release build on a Chicago dataset. To reproduce:

```
cargo run --release --bin benchmark_smoke -- transit-viz/public/data/chicago.bin 41.8781 -87.6298 20260413 900 60 45 60 10
```

```
=== Binary Section Sizes (decompressed) ===
Section                          Bytes % of total
header                            32 B     0.0%
nodes                          1.16 MB    13.1%
edges                          1.26 MB    14.2%
stops                         665.0 KB     7.4%
route_names                     1.5 KB     0.0%
route_colors                     844 B     0.0%
patterns                       5.41 MB    61.3%
leg_shapes                    360.4 KB     4.0%
TOTAL decompressed             8.83 MB

=== In-Memory Sizes ===
Structure                        Bytes % of total
nodes                          7.84 MB     7.4%
edges                         10.13 MB     9.6%
stops                         998.5 KB     0.9%
route_names                     5.6 KB     0.0%
route_colors                     844 B     0.0%
patterns/events               54.97 MB    51.9%
patterns/freq                  3.13 MB     3.0%
patterns/other                   124 B     0.0%
adj list                      15.47 MB    14.6%
leg_shapes                     1.31 MB     1.2%
node_grid                      3.24 MB     3.1%
input buf                      8.83 MB     8.3%
TOTAL in-memory              105.90 MB

=== Load Timings ===
Phase                           Time % of total
parse nodes                   8.5 ms     5.4%
parse edges                  13.8 ms     8.7%
parse stops                   0.9 ms     0.5%
parse route_names             0.0 ms     0.0%
parse route_colors            0.0 ms     0.0%
parse+index patterns        102.0 ms    64.5%
parse leg_shapes              1.3 ms     0.8%
build adj list               11.7 ms     7.4%
build node_grid              19.9 ms    12.6%
TOTAL                       158.0 ms

=== Counts ===
nodes                         514123
edges                         885101
stops                          17076
patterns                          48
route_names                      211
leg_shapes                     21570
total events (raw)           3397896
sentinel events                    0
total freq entries                 0
grid cells                      5935

Source node: 440203
Window: 09:00–10:00 (60 min), max_time=45 min, slack=60s
[profile] setup=1.8ms phase1(initial)=3.2ms phase2(transfer)=64.9ms phase3(stats)=3.1ms total=73.1ms initial_transit_entries=46706
[profile] setup=1.5ms phase1(initial)=3.5ms phase2(transfer)=68.0ms phase3(stats)=2.6ms total=75.5ms initial_transit_entries=46670
[profile] setup=1.7ms phase1(initial)=3.5ms phase2(transfer)=86.5ms phase3(stats)=3.2ms total=94.8ms initial_transit_entries=46698
[profile] setup=1.6ms phase1(initial)=3.2ms phase2(transfer)=86.0ms phase3(stats)=4.5ms total=95.3ms initial_transit_entries=46464
  run 1/10: 0.111 s
  run 2/10: 0.124 s
  run 3/10: 0.107 s
  run 4/10: 0.104 s
  run 5/10: 0.124 s
  run 6/10: 0.126 s
  run 7/10: 0.130 s
  run 8/10: 0.136 s
  run 9/10: 0.140 s
  run 10/10: 0.140 s

Profile routing (10 runs, 4 threads): avg 0.124 s, min 0.104 s, max 0.140 s
Nodes reached: 283469 / 514123
Min travel time: 0 min, avg: 35 min, max: 45 min
Always reachable (fraction=1): 135398, sometimes: 148071
```

**Binary sizes** (regenerate with `make sizes`):

<!-- BEGIN sizes -->
| City | Compressed |
|---|---|
| Berlin | 11.2M |
| Boston | 4.2M |
| Calgary | 3.0M |
| Chicago | 8.2M |
| Hong Kong | 8.7M |
| Los Angeles | 9.7M |
| Madrid | 8.3M |
| Mexico City | 1.5M |
| Montreal | 19.6M |
| Moscow | 4.9M |
| New York City | 17.9M |
| Ottawa | 8.0M |
| Paris | 17.4M |
| Philadelphia | 4.3M |
| San Francisco Bay Area | 10.3M |
| Seattle | 5.4M |
| Toronto | 15.9M |
| Vancouver | 5.4M |
| Washington | 11.3M |
| Waterloo | 1.4M |
<!-- END sizes -->

**WASM module** (`ls -lh transit-viz/pkg/transit_router_bg.wasm`): ~250 KB
