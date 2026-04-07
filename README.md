# Transit Isochrone Tool

A browser-based tool that shows how far you can travel from any point on a map using public transit and walking, at any time of day. The entire routing computation runs inside your browser — no server required.

Live demo: https://wengh.github.io/transit-time/

## Using the tool

### Picking a city

The landing page lists available cities. Click one to load it. The city's transit and street data (~1–40 MB compressed depending on city size) downloads and loads in the browser; a progress bar shows the download, then a brief indexing phase (~400 ms for a large city like Chicago).

### Setting an origin

**Desktop:** Double-click anywhere on the map to set your starting point. A pin appears snapped to the nearest walkable street node.

**Mobile:** Long-press to set the origin.

Once an origin is set, the map fills with a color-coded isochrone overlay. Green areas are reachable quickly; the color shifts through yellow and red as travel time increases, fading out where nothing is reachable within the time limit.

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

When a destination is pinned in hour-window average mode, a chart appears below the itinerary. The X-axis is departure time within the hour window; the Y-axis is travel time to the destination. Each diagonal line represents one transit trip: as your departure time gets later and closer to when the vehicle leaves the stop, your wait shrinks and total travel time decreases — that is the downward slope. When you depart late enough to miss that vehicle, travel time jumps up because you must wait for the next one, forming the sawtooth pattern. The dashed horizontal line (if present) is the walk-only time. Hover over the chart to highlight a specific departure and see its route on the map; click to lock it.

### Copying trip info

When a destination is pinned, a "Copy info" button appears. It copies a plain-text summary of the origin, destination, settings, and itinerary to the clipboard.

---

## Data flow

The pipeline has two stages: offline preprocessing and in-browser routing.

### Offline: building city data files

A Rust preprocessing tool (`transit-prep`) takes a city configuration (a `.jsonc` file in `cities/`) and produces a single self-contained `.bin` file for that city.

The city config specifies:
- One or more GTFS feed URLs (the standard transit schedule format used by most agencies)
- An OpenStreetMap bounding box for pedestrian street data
- Display metadata (name, map center, zoom)

The preprocessor downloads and caches both the GTFS feeds and the OSM extract, then performs the following steps:

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

**Loading and indexing (~400 ms for Chicago):** The WASM module decodes all PCO-compressed sections and builds two additional in-memory structures: a flat (jagged array) adjacency list for the street graph, and a spatial grid index over all nodes for snapping a clicked lat/lon to the nearest node in ~78 µs. Because nodes are stored in SFC order, walking a neighborhood during Dijkstra accesses nodes that are close together in both geography and memory, improving cache locality. The in-memory footprint for Chicago is about 250 MB, dominated by the pattern event index (~103 MB) and the adjacency list (~44 MB).

**Routing — time-dependent Dijkstra:** When an origin is set, the router runs a shortest-path search over the combined graph. Walking edges have a fixed cost based on distance at 1.4 m/s (~5 km/h). At each node that is a transit stop, the router scans the event arrays for all active service patterns and boards vehicles. The search is time-dependent: the cost of boarding a transit vehicle is the wait time until its next departure plus the scheduled ride time. A transfer between different vehicles requires at least the configured transfer slack. The router tracks, for each reached node, the arrival time, the incoming route, the boarding time, and the latest possible home-departure time that still makes the connection.

For a city the size of Chicago (817K nodes, 1.2M edges, 200 routes, 6.2M raw trip events), a single query takes about 220 ms and reaches roughly 530K nodes.

**Hour-window average mode** runs multiple queries in parallel using a Rayon thread pool initialized via WebAssembly threads (SharedArrayBuffer). If the browser does not support shared memory, it falls back to sequential execution.

**Rendering:** After routing, each node's travel time is sent to a WebGL shader that maps it to a color. Points are rendered onto an offscreen canvas at a size proportional to the map zoom level, producing a continuous-looking coverage surface. The canvas is then composited onto the Leaflet map as an image overlay. Route polylines are drawn using the GTFS shape data where available, falling back to straight-line segments between stops.

---

## Building

**Prerequisites:**
- Rust (nightly toolchain, for the WASM build)
- [wasm-pack](https://rustwasm.github.io/wasm-pack/)
- Node.js and npm

**Build the WASM module** (only needed when the routing logic changes):
```
make wasm
```

**Build city data files** (downloads GTFS and OSM data, takes a few minutes per city):
```
make data-all
```

Individual cities can be built with:
```
cargo run --release -p transit-prep -- --city-file cities/chicago.jsonc --output transit-viz/public/data/chicago.bin
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

Create a `.jsonc` file in `cities/` with the following fields:

```jsonc
{
  "id": "my_city",              // used in the URL path
  "name": "My City, ST",        // display name
  "file": "my_city.bin",        // output data file name
  "feed_ids": [
    "https://example.com/gtfs.zip"   // one or more GTFS feed URLs
  ],
  "bbox": "-80.0,43.0,-79.0,44.0",  // min_lon,min_lat,max_lon,max_lat
  "bbbike_name": "MyCity",      // BBBike extract name (for OSM data), OR
  // "osm_url": "https://...",  // direct URL to an OSM PBF file
  "center": [43.65, -79.38],    // map center [lat, lon]
  "zoom": 12,                   // initial zoom level
  "detail": "Agency A, Agency B" // shown in city list
}
```

OSM pedestrian data is fetched from BBBike by name, or from a direct URL if `osm_url` is given. Then run `make data-all` (or the individual `cargo run` command) to build the `.bin` file.

---

## Performance

The numbers below are from a release build on a Chicago dataset (33 MB compressed / 250 MB in memory). To reproduce:

```
cargo test --release --test profile_router -- --nocapture
```

**Load and index time (~393 ms total):**

| Phase | Time |
|---|---|
| Parse pattern events and build stop index | 211 ms |
| Build street adjacency list | 117 ms |
| Build spatial node grid | 42 ms |
| Parse nodes + edges + stops | 12 ms |

**Single routing query (Chicago, ~820K nodes):**

| Metric | Value |
|---|---|
| Average query time | 218 ms |
| Min / Max | 208 ms / 230 ms |
| Nodes reached (average) | 527K of 817K |

**Binary sizes** (`ls -lh transit-viz/public/data/`):

| City | Compressed |
|---|---|
| Chapel Hill | 1.3 MB |
| Waterloo | 5.9 MB |
| Seattle | 19 MB |
| Toronto | 18 MB |
| Chicago | 33 MB |
| Montreal | 38 MB |
| NYC | 39 MB |
| SF Bay | 38 MB |

**WASM module** (`ls -lh transit-viz/pkg/transit_router_bg.wasm`): 659 KB

The largest in-memory structure is the pattern event index (103 MB for Chicago), which stores all 6.2M trip events indexed by stop for O(log n) binary-search access during routing. The raw input buffer (40 MB for Chicago) is retained in memory alongside the parsed structures.
