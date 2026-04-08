# Transit Isochrone Tool

A browser-based tool that shows how far you can travel from any point on a map using public transit and walking, at any time of day. The entire routing computation runs inside your browser — no server required.

Live demo: https://wengh.github.io/transit-time/

## Using the tool

### Picking a city

The landing page lists available cities. Click one to load it. The city's transit and street data (~1–30 MB compressed depending on city size) downloads and loads in the browser; a progress bar shows the download, then a brief indexing phase (~400 ms for a large city like Chicago).

### Setting an origin

**Desktop:** Double-click anywhere on the map to set your starting point. A pin appears snapped to the nearest walkable street node.

**Mobile:** Long-press to set the origin.

Once an origin is set, the map fills with a color-coded isochrone overlay. Green/yellow areas are reachable quickly; the color shifts through orange and red as travel time increases, fading out where nothing is reachable within the time limit.

In **hour-window average** mode the overlay also encodes how consistently a location is reachable across the sampled departure times. Locations reachable from every sampled departure use the warm (yellow→red) scale. Locations reachable from only some departures shift toward cool colors — cyan for nearby spots that are only sometimes served, through blue and purple for farther or less reliably served locations. Locations never reachable in any sample are not shown.

### Exploring destinations

**Desktop:** Move your cursor over the map. The route from your origin to the point under the cursor is drawn on the map — walk segments as gray dashed lines, transit segments colored by route. A panel appears showing the travel time and the step-by-step itinerary.

**Mobile:** Tap to pin a destination.

**Pinning:** Single-click (desktop) or tap (mobile) to pin a destination so the route stays visible while you adjust controls. Click/tap again to unpin.

### Controls

All controls re-run the routing query immediately when changed.

**Mode**
- *Single Departure Time* — computes travel times for one exact departure.
- *Hour-Window Average* — runs the router at multiple evenly-spaced departure times across a one-hour window and averages the results. This smooths out the "lucky timing" effect and shows more typical travel times. The number of sample departures is adjustable.

**Date** — select any calendar date. The tool activates only the transit schedules valid on that date (weekday, weekend, or holiday service), and shows how many service patterns are active.

**Departure time** — slider from midnight to midnight in 5-minute steps.

**Samples** — (hour-window average only) number of departure times spread across the hour window. More samples give a smoother average at the cost of longer computation. Default is 15.

**Max travel time** — caps the search at 10–180 minutes. Locations unreachable within this limit are not shown.

**Transfer slack** — minimum connection time required when switching between transit vehicles (0–300 seconds, default 60 s). A higher value avoids tight transfers that might be missed in practice.

### Sawtooth chart (hour-window average)

When a destination is pinned in hour-window average mode, a chart appears below the itinerary. The X-axis is departure time within the hour window; the Y-axis is travel time to the destination. Each diagonal line represents one transit trip: as your departure time gets later and closer to when the vehicle leaves the stop, your wait shrinks and total travel time decreases — that is the downward slope. When you depart late enough to miss that vehicle, travel time jumps up because you must wait for the next one, forming the sawtooth pattern. The dashed horizontal line (if present) is the walk-only time. Shaded grey columns mark departure times where no transit option falls within the travel-time limit.

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

1. **Parse GTFS** — reads stops, routes, trips, stop times, service calendars, and shapes from the zip archives. Filters stops to the bounding box. Warns if feed data has expired.

2. **Parse OSM** — extracts the pedestrian-walkable street network (footways, paths, sidewalks, crossings, and regular roads that allow foot traffic) within the bounding box.

3. **Snap stops to street nodes** — each transit stop is matched to the nearest point on the street network by inserting a virtual node on the nearest edge and connecting it. This lets the router walk from any street point directly to any stop.

4. **Build service patterns** — trips that share the same stop sequence and service calendar are grouped into a pattern. For each pattern, stop times are stored as a sorted array of time offsets per stop, enabling binary-search-based lookup during routing. Frequency-based routes (trips defined by headway rather than fixed times) are stored separately. The resulting structure enables the router to scan only the relevant events at each stop rather than searching all trips.

5. **Serialize** — all data is written to a custom binary format, with several layers of sorting applied to improve both compression ratios and runtime locality.

   *Node ordering:* nodes are reordered along a Morton (Z-order) space-filling curve before writing. Because the SFC maps 2D geographic proximity to 1D index proximity, consecutive nodes in the array tend to be geographic neighbors, and their latitude/longitude values form nearly-monotone sequences. The coordinates are stored as fixed-point 32-bit integers (0.1 m resolution) rather than 64-bit floats, reducing raw size by 4×, and the two columns (latitudes, longitudes) are PCO-compressed separately — the small deltas between neighboring values compress extremely well.

   *Edge encoding:* the SFC reordering also benefits edges. Each undirected edge is stored canonically with the higher-numbered endpoint first and encoded as a `(u, delta)` pair where `delta = u - v`. Because nearby nodes in SFC order tend to be connected, `delta` values are typically small. Edges are sorted by `(u, delta)` and the three columns (endpoints, deltas, distances) are PCO-compressed. Edge distances are stored not as absolute values but as the excess above the straight-line haversine distance between the two endpoints; straight edges and many short segments encode as exactly zero, compressing very efficiently.

   *Event ordering:* the events in each service pattern are sorted by `(stop_index, time_offset)` — all events at a given stop contiguous, chronological within the group. This lets the router binary-search by time within each stop's bucket at query time, and the sorted columns compress efficiently with PCO. All numeric event columns (time offsets, stop indices, travel times, chain pointers, bucket offsets, route labels) and shape coordinates are PCO-compressed.

   The assembled binary is then gzip-compressed for transfer; the browser decompresses it on the fly during download.

File sizes for the included cities range from 1.3 MB (Chapel Hill) to 39 MB (NYC and SF Bay), reflecting network and schedule size.

### In-browser: routing and rendering

The city `.bin` file is fetched and streamed through the browser's native gzip decompression. The decompressed bytes are handed to a WebAssembly module (~700 KB) compiled from the Rust routing engine.

**Loading and indexing (~330 ms for Chicago):** The WASM module decodes all PCO-compressed sections and builds two additional in-memory structures: a flat (jagged array) adjacency list for the street graph, and a spatial grid index over all nodes for snapping a clicked lat/lon to the nearest node in ~8 µs. Because nodes are stored in SFC order, walking a neighborhood during Dijkstra accesses nodes that are close together in both geography and memory, improving cache locality. The in-memory footprint for Chicago is about 186 MB, dominated by the pattern event index (~103 MB), the adjacency list (~21 MB), and the raw input buffer (~17 MB).

**Routing — time-dependent Dijkstra:** When an origin is set, the router runs a shortest-path search over the combined graph. Walking edges have a fixed cost based on distance at 1.4 m/s (~5 km/h). At each node that is a transit stop, the router scans the event arrays for all active service patterns and boards vehicles. The search is time-dependent: the cost of boarding a transit vehicle is the wait time until its next departure plus the scheduled ride time. A transfer between different vehicles requires at least the configured transfer slack. During the search, the router also tracks the latest possible home-departure time that still makes each connection — this is used to prefer paths that allow leaving later — but it is transient and not retained after the query completes. The result stored per reached node is 12 bytes: two u16 offsets from the query departure time (arrival and boarding times, supporting up to ~18 hours of travel), the incoming route index, and the predecessor node for path reconstruction.

For a city the size of Chicago (817K nodes, 1.2M edges, 200 routes, 6.2M raw trip events), a single query takes about 122 ms and reaches roughly 527K nodes. In hour-window average mode, one result set is kept per sample departure and all are live simultaneously for path reconstruction on hover; at 15 samples (the default) this adds about 150 MB on top of the ~186 MB base footprint.

**Hour-window average mode** runs multiple queries in parallel using a Rayon thread pool initialized via WebAssembly threads (SharedArrayBuffer). If the browser does not support shared memory, it falls back to sequential execution.

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

This runs the pipeline which: extracts feed IDs from all city configs, checks Transitland for updated feed versions (via SHA1 comparison), downloads only stale or missing GTFS/OSM data, and rebuilds only affected city `.bin` files.

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

This downloads the OSM extract, reads its bounding box, queries Transitland for all transit feeds in that area, and writes a `.jsonc` config with Transitland feed IDs and operator name comments. Edit the generated file to fill in `name`, `detail`, and remove any unwanted feeds.

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
  "detail": "Agency A, Agency B" // shown in city list
}
```

Feed IDs can be Transitland onestop IDs (e.g. `f-dp3-cta`) or direct GTFS zip URLs. Transitland feeds are checked for updates automatically via SHA1 comparison. OSM pedestrian data is fetched from BBBike by name, or from a direct URL if `osm_url` is given. Then run `make data-all` to build the `.bin` file.

---

## Performance

The numbers below are from a release build on a Chicago dataset (16 MB compressed / 186 MB in memory). To reproduce:

```
cargo test --release --test profile_router -- --nocapture
```

```
=== Binary Section Sizes (decompressed) ===
Section                          Bytes % of total
header                            36 B     0.0%
nodes                          1.80 MB    10.6%
edges                          1.44 MB     8.5%
stops                         671.5 KB     3.9%
stop_to_node                  135.0 KB     0.8%
route_names                     1.5 KB     0.0%
route_colors                     844 B     0.0%
patterns                       9.91 MB    58.5%
shapes                         2.99 MB    17.7%
route_shapes                    8.7 KB     0.0%
TOTAL decompressed            16.94 MB

=== In-Memory Sizes ===
Structure                        Bytes % of total
nodes                         12.47 MB     6.6%
edges                         13.70 MB     7.3%
stops                        1008.9 KB     0.5%
stop_node_map                  67.5 KB     0.0%
node_is_stop                  798.3 KB     0.4%
node_stop_indices             627.5 KB     0.3%
route_names                     5.6 KB     0.0%
route_colors                     844 B     0.0%
patterns/events              104.52 MB    55.4%
patterns/freq                  8.90 MB     4.7%
patterns/other                   468 B     0.0%
adj list                      21.38 MB    11.3%
shapes                         3.39 MB     1.8%
route_shapes                       0 B     0.0%
node_grid                      5.00 MB     2.7%
input buf                     16.94 MB     9.0%
TOTAL in-memory              188.76 MB

=== Load Timings ===
Phase                           Time % of total
parse nodes                  11.7 ms     3.7%
parse edges                  67.4 ms    21.3%
parse stops                   1.3 ms     0.4%
parse stop_to_node            1.3 ms     0.4%
parse route_names             0.0 ms     0.0%
parse route_colors            0.0 ms     0.0%
parse+index patterns        188.1 ms    59.5%
parse shapes                  0.3 ms     0.1%
parse route_shapes            0.0 ms     0.0%
build adj list               16.2 ms     5.1%
build node_grid              29.8 ms     9.4%
TOTAL                       316.2 ms

=== Counts ===
nodes                         817507
edges                        1197032
stops                          17274
stop_to_node                   17274
patterns                         135
route_names                      211
shapes                          2350
total events (raw)           6266597
sentinel events                    0
total freq entries                 0
grid cells                      6352
snap_to_node: 17µs -> node 722404 (41.88439954326267, -87.62934665014365)
Monday patterns: 12 total

Depart     Time(ms)    Reached    Transit
------------------------------------------
09:00      132.3ms     541122       3149
09:06      125.4ms     537332       2996
09:12      129.7ms     562554       2865
09:18      131.3ms     568234       3088
09:24      129.6ms     553720       3077
09:30      117.4ms     506895       3104
09:36      119.9ms     521497       3071
09:42      125.3ms     534934       3085
09:48      122.6ms     520163       2879
09:54      125.3ms     510709       3108

=== Summary (10 runs) ===
Avg: 125.9ms  Min: 117.4ms  Max: 132.3ms
Avg reachable nodes: 535716
```

**Binary sizes** (`ls -lh transit-viz/public/data/`):

| City | Compressed |
|---|---|
| Chapel Hill | 454 KB |
| Waterloo | 2.3 MB |
| Seattle | 8.0 MB |
| Toronto | 18 MB |
| Chicago | 16 MB |
| SF Bay | 18 MB |
| NYC | 20 MB |
| Montreal | 22 MB |

**WASM module** (`ls -lh transit-viz/pkg/transit_router_bg.wasm`): 659 KB
