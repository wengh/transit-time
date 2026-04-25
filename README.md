# Transit Isochrone Tool

A browser-based tool that shows how far you can travel from any point on a map using public transit and walking, at any time of day. The entire routing computation runs inside your browser — no server required.

Live demo: https://transit-time.pages.dev/

Note: this project is mostly vibe coded.
- `transit-router/src/profile.rs` is mostly manually written. All AI changes in this file are thoroughly reviewed.
- Other parts of the codebase are mostly written by AI. Only the high level design and architecture are driven by human.

## Using the tool

### Picking a city

The landing page lists available cities. Click one to load it. The city's transit and street data (up to ~20 MB compressed depending on city size) downloads and loads in the browser; a progress bar shows the download, then a brief indexing phase.

### Setting an origin

**Desktop:** Double-click anywhere on the map to set your starting point. A pin appears snapped to the nearest walkable street node.

**Mobile:** Long-press to set the origin.

Once an origin is set, the map fills with a color-coded isochrone overlay. Green/yellow areas are reachable quickly; the color shifts through orange and red as travel time increases, fading out where nothing is reachable within the time limit.

The overlay also encodes how consistently a location is reachable across the departure window. Locations reachable from every departure within the window use the warm (yellow→red) scale. Locations reachable from only some departures shift toward cool colors — cyan for nearby spots that are only sometimes served, through blue and purple for farther or less reliably served locations. Locations never reachable within the window are not shown.

### Exploring destinations

**Desktop:** Move your cursor over the map. The route from your origin to the point under the cursor is drawn on the map — walk segments as gray dashed lines, transit segments colored by route. A panel appears showing the travel time and the step-by-step itinerary.

**Mobile:** Tap to pin a destination.

**Pinning:** Single-click (desktop) or tap (mobile) to pin a destination so the route stays visible while you adjust controls. Click/tap again to unpin.

### Controls

All controls re-run the routing query immediately when changed.

**Date** — select any calendar date. The tool activates only the transit schedules valid on that date (weekday, weekend, or holiday service), and shows how many service patterns are active.

**Departure time** — slider from midnight to midnight in 5-minute steps. The router computes travel times across a one-hour window starting at this time, smoothing out the "lucky timing" effect of any single departure.

**Max travel time** — caps the search at 10–180 minutes. Locations unreachable within this limit are not shown.

**Transfer slack** — minimum connection time required when switching between transit vehicles (0–300 seconds, default 60 s). A higher value avoids tight transfers that might be missed in practice.

### Sawtooth chart

When a destination is pinned, a chart appears below the itinerary. The X-axis is departure time within the hour window; the Y-axis is travel time to the destination. Each diagonal line represents one transit trip: as your departure time gets later and closer to when the vehicle leaves the stop, your wait shrinks and total travel time decreases — that is the downward slope. When you depart late enough to miss that vehicle, travel time jumps up because you must wait for the next one, forming the sawtooth pattern. The dashed horizontal line (if present) is the walk-only time. Shaded grey columns mark departure times where no transit option falls within the travel-time limit.

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

2. **Parse OSM and build street graph** — extracts the pedestrian-walkable street network (footways, paths, sidewalks, crossings, and roads that allow foot traffic) within the bounding box. Subway entrances (`railway=subway_entrance`) are flagged as mandatory graph nodes. The raw node/way data is then reduced to a proper graph: only intersection nodes (used by two or more ways) and entrance nodes become graph vertices; intermediate nodes are discarded and their traversed distance accumulated into edge weights. Entrance nodes not already on a way are linked to the nearest street node within 200 m. Finally, small disconnected components with fewer than 50 nodes are removed — typically isolated fragments on the wrong side of a fence or elevated structure that cannot realistically be reached on foot.

3. **Snap stops to street nodes** — each transit stop is matched to the nearest point on the street network by inserting a virtual node on the nearest edge and connecting it. This lets the router walk from any street point directly to any stop.

4. **Prune unreachable nodes** — a breadth-first search from every snapped stop node identifies all street nodes reachable on foot from transit. Nodes and edges outside that reachable set are removed and all indices remapped. This discards dead-end pedestrian areas disconnected from the transit network, shrinking both the routing graph and the output binary.

5. **Build service patterns and extract leg shapes** — trips that share the same stop sequence and service calendar are grouped into a pattern. For each pattern, stop times are stored as a sorted array of time offsets per stop, enabling binary-search-based lookup during routing. Frequency-based routes (trips defined by headway rather than fixed times) are stored separately. For trips that include GTFS shape data, per-leg polylines are extracted: for each (route, from-stop, to-stop) pair, a dynamic-programming subsequence match aligns the shape point sequence to the stop pair, handling reversed routes and partial alignments. The best-aligned shape for each leg is kept. After pattern construction, routes and stops not referenced in any event are removed and indices remapped, keeping the binary compact.

6. **Serialize** — all data is written to a custom binary format, with several layers of sorting applied to improve both compression ratios and runtime locality.

   *Node ordering:* nodes are reordered along a Morton (Z-order) space-filling curve before writing. Because the SFC maps 2D geographic proximity to 1D index proximity, consecutive nodes in the array tend to be geographic neighbors, and their latitude/longitude values form nearly-monotone sequences. The coordinates are stored as fixed-point 32-bit integers (0.1 m resolution) rather than 64-bit floats, reducing raw size by 4×, and the two columns (latitudes, longitudes) are Pcodec-compressed separately — the small deltas between neighboring values compress extremely well. ([Pcodec](https://github.com/pcodec/pcodec) is a library for lossless compression of numerical sequences, featuring delta encoding, etc.)

   *Edge encoding:* the SFC reordering also benefits edges. Each undirected edge is stored canonically with the higher-numbered endpoint first and encoded as a `(u, delta)` pair where `delta = u - v`. Because nearby nodes in SFC order tend to be connected, `delta` values are typically small. Edges are sorted by `(u, delta)` and the three columns (endpoints, deltas, distances) are Pcodec-compressed. Edge distances are stored directly in walk times (at 1.4 m/s) as 16-bit integers.

   *Event ordering:* the events in each service pattern are sorted by `(stop_index, time_offset)` — all events at a given stop contiguous, chronological within the group. This lets the router binary-search by time within each stop's bucket at query time, and the sorted columns compress efficiently with Pcodec. All numeric event columns (time offsets, stop indices, travel times, chain pointers, bucket offsets, route labels) and shape coordinates are Pcodec-compressed.

   The assembled binary is then gzip-compressed for transfer; the browser decompresses it on the fly during download.

### In-browser: routing and rendering

The city `.bin` file is fetched and streamed through the browser's native gzip decompression. The decompressed bytes are handed to a WebAssembly module compiled from the Rust routing engine.

**Loading and indexing:** The WASM module decodes all Pcodec-compressed sections and builds two additional in-memory structures: a flat (jagged array) adjacency list for the street graph, and a spatial grid index over all nodes for snapping a clicked lat/lon to the nearest node. Because nodes are stored in SFC order, walking a neighborhood during the search accesses nodes that are close together in both geography and memory, improving cache locality.

**Routing — profile search over a departure window:** When an origin is set, the router computes a Pareto frontier of (departure, arrival) pairs for every reachable node in a single pass across the full one-hour window. Walking edges have a fixed cost based on distance at 1.4 m/s. At each node that is a transit stop, the router scans the event arrays for all active service patterns and considers every boarding that sits on the frontier; a transfer between different vehicles requires at least the configured transfer slack. From the frontier the router derives, per node, the mean travel time and the fraction of departures within the window from which the node is reachable — the two quantities the overlay and hover summaries display. A compact set of representative (Pareto-optimal) departures is retained for path reconstruction on hover, which drives the sawtooth chart and the itinerary view.

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
nodes                          1.85 MB    15.4%
edges                          1.71 MB    14.3%
stops                         665.7 KB     5.4%
route_names                     1.5 KB     0.0%
route_colors                     844 B     0.0%
patterns                       7.40 MB    61.8%
leg_shapes                    363.8 KB     3.0%
TOTAL decompressed            11.96 MB

=== In-Memory Sizes ===
Structure                        Bytes % of total
nodes                         12.69 MB     8.5%
edges                         13.87 MB     9.3%
stops                         999.5 KB     0.7%
route_names                     5.6 KB     0.0%
route_colors                     844 B     0.0%
patterns/events               76.95 MB    51.6%
patterns/freq                  4.56 MB     3.1%
patterns/other                   148 B     0.0%
adj list                      21.66 MB    14.5%
leg_shapes                     1.32 MB     0.9%
node_grid                      5.07 MB     3.4%
input buf                     11.96 MB     8.0%
TOTAL in-memory              149.07 MB

=== Load Timings ===
Phase                           Time % of total
parse nodes                  12.0 ms     5.5%
parse edges                  17.9 ms     8.3%
parse stops                   1.0 ms     0.4%
parse route_names             0.0 ms     0.0%
parse route_colors            0.0 ms     0.0%
parse+index patterns        137.1 ms    63.6%
parse leg_shapes              1.5 ms     0.7%
build adj list               16.7 ms     7.7%
build node_grid              29.7 ms    13.7%
TOTAL                       215.7 ms

=== Counts ===
nodes                         831341
edges                        1211969
stops                          17094
patterns                          70
route_names                      211
leg_shapes                     21738
total events (raw)           4743765
sentinel events                    0
total freq entries                 0
grid cells                      6312

Source node: 716037
Window: 09:00–10:00 (60 min), max_time=45 min, slack=60s
[profile] setup=8.0ms phase1(walk)=11.0ms phase2(transit)=403.2ms phase3(stats)=10.2ms total=432.5ms initial_transit_entries=182546
  run 1/10: 0.433 s
  run 2/10: 0.442 s
  run 3/10: 0.432 s
  run 4/10: 0.427 s
  run 5/10: 0.435 s
  run 6/10: 0.428 s
  run 7/10: 0.423 s
  run 8/10: 0.417 s
  run 9/10: 0.436 s
  run 10/10: 0.421 s

Profile routing (10 runs): avg 0.429 s, min 0.417 s, max 0.442 s
Nodes reached: 452760 / 831341
Min travel time: 0 min, avg: 35 min, max: 45 min
Always reachable (fraction=1): 216318, sometimes: 236442
```

**Binary sizes** (regenerate with `make sizes`):

<!-- BEGIN sizes -->
| City | Compressed |
|---|---|
| Berlin | 12.9M |
| Boston | 5.0M |
| Calgary | 3.3M |
| Chicago | 11.2M |
| Hong Kong | 9.2M |
| Los Angeles | 11.1M |
| Madrid | 8.9M |
| Mexico City | 1.9M |
| Montreal | 21.0M |
| Moscow | 5.9M |
| New York City | 19.3M |
| Ottawa | 7.4M |
| Paris | 19.0M |
| Philadelphia | 4.8M |
| San Francisco Bay Area | 12.2M |
| Seattle | 6.6M |
| Toronto | 17.4M |
| Vancouver | 9.7M |
| Washington | 12.0M |
| Waterloo | 1.8M |
<!-- END sizes -->

**WASM module** (`ls -lh transit-viz/pkg/transit_router_bg.wasm`): ~250 KB
