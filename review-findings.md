# Code review findings (2026-09-06)

Whole-repo review covering bugs, performance, and refactors. Each finding
was verified against the source before being listed. Severity legend:
**bug-high** (user-visible or data-corrupting), **bug-low** (edge case or
transient), **perf**, **refactor**, **nit**.

Status column: `fixed` = implemented in this review series, `skipped` =
deliberately not done (reason given), `open` = worth doing later.

Baseline before changes: `cargo build --release --workspace` clean,
`cargo test -p transit-router` 6/6 pass, `tsc --noEmit` clean, prettier
clean, ~120 clippy warnings.

After the change series (51 commits following this file): workspace
builds with **0 clippy warnings**; router property tests pass on chicago,
hong_kong and paris; `transit-prep`/`city-builder`/`transit-data` unit
tests pass; `tsc`, prettier and `vite build` pass. The binary format is
unchanged (v12) and every shipped `.bin` still loads.

Measured effects (NYC 09:00–10:00, 45 min, 4 threads, 3 runs):
routing 0.153 s → 0.129 s avg, per-query index build 14.5 ms → 5.6 ms,
identical nodes reached and Pareto entries. Preprocessing from cached
inputs: Toronto 43.6 s → 19.7 s, Washington DC 50.7 s → 16.6 s, Chicago
51.4 s → 22.7 s; Washington DC zero-length transit events 47,928 → 0;
Chicago patterns 183 (104 empty) → 79; Toronto weekday-active patterns
37 → 6 (the duplicated GO Transit day-services).

---

## 1. Frontend — state, worker bridge, app shell (`transit-viz/src`)

| # | Sev | Where | Finding | Status |
|---|-----|-------|---------|--------|
| F1 | bug-high | `App.tsx` query effect, `reducer.ts` `SET_SOURCE`, `MapView.tsx` `setSource` | Re-selecting the same origin node (double-click near the pin, mobile origin tap, re-picking a search result) dispatches `SET_SOURCE` with an unchanged `sourceNode`. The reducer wipes `travelTimes`, but the only query trigger is an effect keyed on `sourceNode`, so nothing re-runs: the isochrone disappears, status still says "Done", and a kept pin sits with null hover data forever. | fixed — query is now derived from state with a `querySeq` bumped on every `SET_SOURCE`; the `onRunQuery(overrides)` prop chain is gone |
| F2 | bug-low | `router.ts` `runQuery`, `App.tsx` | A superseded query whose compute phase already finished still resolves and lands `QUERY_DONE` after the newer query's `COMPUTING`, briefly showing old results against new parameters and arming playback on a profile the worker is about to replace. | fixed — `runQuery` rejects results whose id is not the latest |
| F3 | bug-low | `MapView.tsx` `showDestination` | The "re-verify state after async work" guard reads state twice *before* the only `await`, so it can never fire. A hover/pin resolving after a source change paints old-origin routes and repopulates `hoverDest` with stale data. | fixed — guard moved after the await |
| F4 | bug-low | `ControlsBody.tsx` `handleChangeCity`, `router.worker.ts` `handleLoadRouter` | "Change city" during a compute queues `freeProfile`/`loadRouter` behind the running compute (progress sits at 0%), and the previous city's `TransitRouter` is never freed, leaking the whole decoded dataset in WASM memory on every switch. | fixed — `freeProfile` cancels the in-flight query; the old router is freed before the new one is built |
| F5 | bug-low | `reducer.ts` `CHANGE_CITY` | `interactionMode` sticks at `'dest'` across city changes, so on mobile the next city's taps are routed to the destination branch and silently dropped while the hint says "Tap map to set origin". | fixed — reset to `'origin'` on `CHANGE_CITY`/`LOAD_ERROR` |
| F6 | bug-low | `cityLoader.ts`, `CitySelect.tsx`, `ControlsBody.tsx` | Service-pattern count is computed for *today (UTC)* regardless of the restored date, and not at all when a city is picked from the list ("0 service patterns active"). | fixed — one effect on `[date, loadingState]` owns the count; the flag and duplicate fetch are removed |
| F7 | bug-low | `reducer.ts`, `cityLoader.ts`, `format.ts` | Default date uses `toISOString()` (UTC), which is yesterday for users east of UTC in the morning (Hong Kong, Tokyo, Sydney are shipped cities). | fixed — local-date helper |
| F8 | bug-low | `urlHash.ts`, `ControlsBody.tsx`, `router.worker.ts` | An empty/malformed `date` (cleared date input, bad hash) becomes `NaN` → `0` at the WASM boundary, where `decode_yyyymmdd` panics and traps the worker. `dur=0` in the hash also divides by zero in the chart. | fixed — date validated, window duration clamped |
| F9 | bug-low | `animationStore.ts` `onPrimaryResolved`, `pause` | A frame response is painted only if the playhead is still on exactly that departure. At the default 18 h window the playhead advances ~3 grid steps per rAF tick, so any worker round-trip over one frame time discards every response and the map freezes while the readout advances. `pause()` also never renders the paused time, and `renderedDeparture` is reported from the throttle timer rather than from what was drawn. | fixed — stale-while-revalidate: paint any current-epoch frame, then chase; `renderedDeparture` set only on paint |
| F10 | bug-low | `router.ts` | No `worker.onerror`; a worker crash leaves every pending promise unresolved (app sits at "Loading…"). | fixed |
| F11 | perf | `router.worker.ts` | `transfer` list is declared but never populated, so `travelTimes`, `sampleCounts` and `nodeCoords` are structured-cloned. | fixed |
| F12 | perf | `router.worker.ts`, `reducer.ts` | One `LOADING_PROGRESS` dispatch (whole-tree render) per fetch chunk, hundreds per load, even when the integer percent is unchanged. | fixed — post only on percent change; reducer no-ops on equal progress |
| F13 | perf | `hoverInfo.ts` | `HoverData.travelTimes` is built (map/filter/sort over all paths) on every hover and never read. | fixed — removed |
| F14 | perf | `HoverInfo.tsx`, `MapView.tsx`, `MobileBottomSheet.tsx` | `computeChartInfo` runs three times per animation frame (panel, chart effect, map route resolver); `deriveDisplayPath` returns a spread copy so identity checks fail and the pinned route GeoJSON is rebuilt and re-uploaded at ~30 Hz during playback even when the optimal path did not change. | fixed — memoised per `HoverData`; paths returned by reference; redraw skipped when unchanged |
| F15 | perf | `HoverInfo.tsx` `drawChart` | Canvas backing store reallocated on every draw (~30 Hz during playback). | fixed — resize only when dimensions change |
| F16 | perf | `isochroneLayer.ts` | Shader objects are never deleted; each style swap leaks four. | fixed |
| F17 | refactor | `HoverInfo.tsx` | The "hide/Details" mobile branches are unreachable: `HoverInfo` only mounts on desktop (`useIsMobile` is the exact complement of Tailwind `sm`). ~40 dead lines plus `max-sm:` classes. | fixed |
| F18 | refactor | `HoverInfo.tsx`, `MobileBottomSheet.tsx`, `MapView.tsx` | Pure chart/path helpers live in a component file and are imported by two other components; the destination summary derivation is duplicated in both panels. | fixed — helpers moved to `utils/hoverInfo.ts` |
| F19 | refactor | `router.ts`, `hoverInfo.ts`, `HoverInfo.tsx` | `HoverPath.totalTime: number \| null` is never null (Rust `u32`); every null branch is dead. | fixed |
| F20 | refactor | `App.tsx`, `ControlsBody.tsx` | `SHOW_COPIED_MESSAGE` + hide timer dispatched twice per copy; `LOAD_ERROR` dispatched twice per load failure; `fmtT` duplicates `formatTime`. | fixed |
| F21 | refactor | `MapView.tsx`, `PathSegmentList.tsx` | Route colour resolution is implemented twice with different palette-index rules, so the itinerary dot can disagree with the map polyline. | fixed — shared helper in `utils/colors.ts`, deterministic by route index |
| F22 | bug-low | `ControlsBody.tsx` `RangeSlider` | Any keyup (Tab, Shift…) and a touch release plus its synthetic mouseup each commit and re-run the full query even when the value is unchanged. | fixed — commit only on value change, single pointerup |
| F23 | bug-low | `ControlsBody.tsx` `DualRangeSlider` | No `onPointerCancel`; an interrupted drag never commits. | fixed |
| F24 | bug-low | `LocationSearch.tsx` | Fetch responses not checked for `ok`/array shape (Nominatim 429 returns an object → `results.length` undefined); an aborted request's `finally` clears the spinner of the request that replaced it; `skipNextReverseRef` stays armed when the select does not produce a `latLng` change, mislabeling the next map placement. | fixed |
| F25 | bug-low | `MapView.tsx` | `lastHoveredNodeRef` is not reset on unpin, so hovering the just-unpinned node shows nothing until the cursor reaches another node. Destination clicks during a compute are dropped instead of queued. | fixed |
| F26 | bug-low | `MobileBottomSheet.tsx` | Interactive `<span role="button">` nested inside a `<button>` (invalid HTML, inconsistent screen-reader behaviour). | fixed |
| F27 | nit | `format.ts`, `colors.ts`, `router.ts`, `cityLoader.ts`, `vite.config.ts`, `App.tsx` | Dead code: `getNextMonday`, `hexToRgb`, `QueryResult.departureTime`, `loadCity` return value, `__wbg_ptr` probes, `eslint-disable` without ESLint, `server.port` overridden by the Makefile. | fixed |
| F28 | refactor | many components | Theming written as `bg-zinc-900 dark:bg-zinc-900 [@media(prefers-color-scheme:light)]:bg-white`, which under Tailwind v4 equals `bg-white dark:bg-zinc-900`. | skipped — purely cosmetic churn across ~10 files with visual-regression risk and no runtime effect |
| F29 | nit | `MobileTopBar.tsx`, `CitySelect.tsx`, `LoadingOverlay.tsx`, `LocationSearch.tsx` | Accessibility: tablist without tabpanels, filter buttons without `aria-pressed`, progress pill without `role=status`, combobox without `aria-controls`. | open |

## 2. Routing engine (`transit-router`, `transit-router-wasm`, `transit-data`)

The core Pareto/profile algorithm in `profile.rs` was traced end to end
(relax invariants, u16 delta bounds, radix-heap monotonicity, split-window
dedupe, all reconstruction cases) and no correctness defect was found in the
routing math. Findings are at the boundaries and in per-query redundant work.

| # | Sev | Where | Finding | Status |
|---|-----|-------|---------|--------|
| R1 | bug-high | `api.rs` `isochrone_inner` | `max_time` is never validated; `max_time == 0` (the documented default!) or `>= 65535` hits an `assert!` inside the engine, which in the browser is a WASM trap that kills the worker. `#maxtime=0` in the URL hash reaches it. `window_end + max_time` is unchecked u32 arithmetic. | fixed — returns `RouterError::InvalidParams` |
| R2 | bug-low | `profile.rs` `split_profile_query` | Warmup path skips `min_required_chunks`; a long window on a 1-thread pool would trip the u16-delta assert. | fixed |
| R3 | bug-low | `profile.rs` `compute_profile_chunks` | Sequential fallback forwards raw per-chunk `(done,total)`, so with >1 chunk on one thread the caller sees progress reset between chunks. | fixed — same scaled accumulation as the parallel path |
| R4 | perf | `profile.rs` `Index::new` | `PatternReverse` (one u32 per event) is rebuilt for every active pattern on every query, but is query-independent and only consumed during hover-time path reconstruction. | fixed — built lazily once per pattern via `OnceLock` on `PatternData` |
| R5 | perf (memory) | `profile.rs` `ProfileRouting` | Per-chunk `destination_totals` (8 B × nodes × chunks, 32–64 MB on big cities) are kept for the isochrone's lifetime after being merged. | fixed — taken out of the chunks after the merge |
| R6 | perf | `profile.rs` Phase 1 | Iterates all `num_nodes` to find stops although stops occupy `[0, num_stops)`; stable sort where unstable suffices. | fixed |
| R7 | perf | `profile.rs` Phase 2 transfer bound | The upper bound takes the `head_next` bound *or* the walk bound instead of the min of both, and the `max_arrival` cap is applied after the early-exit check, so over-budget stops still pay a `partition_point` per pattern. | fixed |
| R8 | perf | `profile.rs` `SplitProfileRouting::optimal_paths` | The walk-only path is reconstructed once per chunk and all but one discarded; a sort exists only to position it. | fixed — walk path from chunk 0 only |
| R9 | bug-low | `profile.rs`, `transit-prep/gtfs.rs` | `headway_secs == 0` from a malformed feed panics with `% 0` at query time. | fixed — rejected in prep, guarded in the router |
| R10 | bug-low | `transit-data/lib.rs` `load` | Truncated/corrupt input panics on unchecked slicing; `Router::from_bytes` promises `RouterError::Data`. | fixed — bounds-checked reader |
| R11 | nit | `api.rs` docs | `TimeWindow` documented half-open but engine is inclusive; `SinceMidnight` claims a `[0,24h)` constraint that does not exist; `mean_travel_time` sentinel documented as `u16::MAX` but is `0`; `stats()` documented as phase timings but returns an entry count; `num_threads_used` is a min, not a measurement. | fixed |
| R12 | refactor | `profile.rs` | `has_any_transit_paths` has no caller; `ProfileRouting::num_nodes` is dead; `entries()` boxes an iterator over a Vec that the caller re-collects. | fixed |
| R13 | refactor | `profile.rs`, `transit-data/lib.rs` | Identical wasm `Instant` shim duplicated. | fixed |
| R14 | refactor | `benchmark_smoke.rs`, `repro_panic.rs`, `tests/common` | Gunzip-and-load implemented four ways (two shell out to `gzip`, the test fixture decodes the binary twice). | fixed — one `flate2` helper |
| R15 | refactor | `wasm/lib.rs`, `path_display.rs` | `segment_shape` narrows `route_index` to `u16`, silently drawing straight lines for routes ≥ 65535. | fixed — `u32` throughout |
| R16 | perf | `path_display.rs` | One `Vec` allocation per leg per hover. | fixed — extend into the output buffer |
| R17 | perf-low | `profile.rs` `Index::new` | `patterns_at_stop` is `Vec<Vec<u32>>` built by an O(patterns × stops) scan. | skipped — measured `index_build` is small relative to routing; the offsets arrays are already O(P×S) |
| R18 | refactor | `transit-data/lib.rs` | `JaggedArray::build` uses `MaybeUninit` + `from_raw_parts` for a single `T = u32` call site; `len()`/`is_empty()` disagree on what they measure. | fixed — safe scatter |
| R19 | open | `profile.rs` Phase 2 | If trips within a pattern never overtake, only the first boardable trip per pattern per round can be non-dominated; skipping the rest is likely the largest remaining Phase-2 win. Needs a prep-side guarantee or a load-time check. | open |
| R20 | nit | bins/tests | `benchmark_smoke` "reuse-cold" is not cold; `debug_assert_eq!` skipped in release; `repro_panic` doc says Chicago but snaps Hong Kong; test helper re-implements `yyyymmdd_to_naive_date_opt`. | fixed |

## 3. Data pipeline orchestration (`city-builder`, `Makefile`, CI)

| # | Sev | Where | Finding | Status |
|---|-----|-------|---------|--------|
| C1 | bug-high | `main.rs` stage 3, `deploy.yml` | `code_changed`/`config_changed` compare mtimes against the restored `.bin`. In CI `actions/checkout` writes every file at checkout time while `actions/cache` restores `.bin` files with their original (older) mtimes, so every city reports "config changed" on every scheduled run; `--check-only` always says rebuild and all cities are rebuilt. | fixed — config hash and a compile-time source fingerprint (`build.rs` over `transit-data`, `transit-prep`, `city-builder`) recorded in `metadata.json` (schema v2, which forces one full rebuild to seed it); mtime checks removed |
| C2 | bug-high | `main.rs` stage 5, `osm_fetch.rs` | The build record stores the identity stage 2/3 *probed*, not what was built from. Stage 3 sees a new OSM ETag → rebuild; stage 5's `fetch_http_osm` returns the cached old extract without a check (30-day shortcut); metadata records the *new* ETag → the city is pinned to old data until the sidecar ages out. Same shape for the download-failure fallback and a transient Transitland error in stage 4. | fixed — `fetch_osm` takes `force_check` (deleting the sidecar would not defeat the shortcut, which falls back to the extract's mtime); stale feeds are force-downloaded; the recorded identity is read from what is on disk after the fetch; a build that fell back to an unverified cached copy keeps its prior record |
| C3 | bug-low | `binary.rs` `write_binary`, `main.rs` stage 5 | `.bin` written non-atomically (a crash leaves a truncated file that later passes as "up to date"); one failing city discards every successful city's metadata. | fixed — temp + rename; metadata saved for successes before propagating the error |
| C4 | bug-low | `main.rs` `cmd_prep` | `make data CITY=x` never writes metadata, so the next `make data-all` rebuilds that city again. | open — needs the probe step; noted in the code |
| C5 | bug-low | `stale.rs`, `binary.rs`, `gtfs.rs` | Malformed calendar dates (`20240230`) panic inside a rayon worker with no feed name. | fixed — validated at parse with the feed named |
| C6 | bug-low | `main.rs` cleanup | Orphan cleanup deletes the download cache of *disabled* cities (hundreds of MB), and is skipped entirely when nothing rebuilt so a removed city's `.bin` keeps deploying. | fixed |
| C7 | perf-high | `deploy.yml` | Bins cache key hashes `transit-router/src` (does not affect `.bin` content) and its `restore-keys` includes the same hash, so any router edit drops the whole bins cache. | fixed — run-id key with prefix restore, since `metadata.json` now carries the content decisions |
| C8 | perf | `main.rs` stage 2, `transitland.rs` | Transitland probes are sequential and build a fresh TLS client per feed (166 feeds). | fixed — shared client, bounded parallelism |
| C9 | perf | `http_cache.rs`, `transitland.rs` | Downloads buffer the whole body then copy it (`.bytes().to_vec()`), ~1.7 GB peak for the largest extract, inside a city-level `par_iter`. | fixed — streamed to the temp file |
| C10 | perf | `Cargo.toml` | `opt-level = "z"` (WASM size tuning) also applies to the native preprocessing crates. | fixed — per-package override for `transit-prep`/`city-builder` |
| C11 | refactor | `main.rs` | `Check` subcommand is unreferenced and gives a different answer than `pipeline --check-only`. | fixed — removed |
| C12 | refactor | `main.rs`, `osm_fetch.rs`, `transitland.rs`, `gtfs_fetch.rs` | `feed_to_cities` values never read; three near-identical `fetch_osm` branches; HTTP client built three ways; `starts_with("f-")` duplicates `is_transitland_id`. | fixed |
| C13 | nit | comments, `Makefile` | Stale comments (`make wasm-pgo`, weekly cron, cache restore ordering); orphan patterns miss `.tmp`/`.lock`; `.json` configs accepted by the builder but invisible to the frontend; `$(PROFDATA)` lacks the `chicago.bin` prerequisite. | fixed |
| C14 | perf/risk | `main.rs` stage 5 | City-level `par_iter` on top of transit-prep's internal rayon parallelism multiplies peak memory. | skipped — no observed OOM; would need measurement on the CI runner |

## 4. Preprocessing (`transit-prep`)

A probe loaded every shipped `.bin` through `transit_data::load` to check
invariants against real output; the numbers below come from that.

| # | Sev | Where | Finding | Status |
|---|-----|-------|---------|--------|
| P1 | bug-high | `gtfs.rs` service grouping | Services defined only in `calendar_dates.txt` get a weekday mask synthesised from their added dates but an *unbounded* date range, so the router treats them as weekly-recurring forever. GO Transit (Toronto) has 89 single-date services: on any Monday all ~13 Monday-dated variants run at once (duplicate trips, holiday variants leaking into every week). 207/221 Toronto and 3310/3310 Amsterdam patterns are in this state. | fixed — calendar-dates-only services keep `mask = 0` and are activated only through their exception dates; the stale policy is the only place that widens them |
| P2 | bug-high | `gtfs.rs` `stop_times` parsing | The per-trip buffer is flushed on every `trip_id` change, so a feed whose `stop_times.txt` is not grouped by trip (Prince George's County "TheBus", in `washington_dc`) has almost every row as its own buffer: untimed rows are dropped and the monotonicity pass never sees consecutive stops. `washington_dc.bin` carries 47,928 non-sentinel events with `travel_time == 0`, violating the documented invariant. | fixed — rows are collected and sorted by `(trip, stop_sequence)` before flushing |
| P3 | perf-high | `prepare.rs` after `build_service_patterns` | Patterns with no events and no frequency rows are still serialised, and each costs two `(num_stops+1)` u32 offset arrays in the browser: Amsterdam 1745/3310 patterns empty, 125 MB of offsets; Berlin 254 MB. `Index::new` also scans `num_stops` per active pattern per query. | fixed — empty patterns dropped before serialisation |
| P4 | perf-high | `prepare.rs` `build_leg_shapes`, `shape_match.rs` | The shape-matching DP runs once per *trip* (GO Transit: 146,514 trips vs 460 shapes), each allocating `stops × segments` 40-byte matches plus a 2-D backtrack table. | fixed — DP runs once per `(shape, stop sequence)`; forward pass stores only `dist_sq` |
| P5 | bug-low | `gtfs.rs` frequencies | Rows with `start_time == end_time` (Hong Kong: 349 single-departure frequencies) are never boardable because the router uses `board < end_time`; `headway_secs == 0` is accepted and panics at query time. | fixed — `end = max(end, start+1)`, zero headways dropped |
| P6 | bug-low | `shape_match.rs` DP | Segment indices are forced strictly increasing, so two stops projecting onto one long straight segment push the second onto the next vertex, detouring the leg polyline. | fixed — equal segments allowed with a `t` tie-break |
| P7 | bug-low | `gtfs.rs` pattern build | Exception dates are appended once per service in a group although all services in a group share them: Waterloo writes 12,321 add-dates of which 11,170 are duplicates, all scanned linearly by `patterns_for_date`. | fixed |
| P8 | bug-low | `graph.rs` `parse_xml` | Ignores `highway` tags and the bbox (every way becomes a walkable edge); attribute parses `unwrap()`. Reachable through any non-`.pbf` `osm_url`. | fixed |
| P9 | bug-low | `gtfs.rs` interpolation | `t_q - t_p` underflows in release when timepoints go backwards, producing garbage interpolated times. | fixed — computed in i64, rows dropped when non-monotone |
| P10 | bug-low | `graph.rs` snapping, `binary.rs` | Two stops can map to one node when a snap lands exactly on a previous virtual node; `write_binary` only `debug_assert`s uniqueness and then panics on `stops[u32::MAX]` in release. | fixed — always create a fresh node; hard assertions |
| P11 | bug-low | `gtfs.rs` CSV parsing | `parse().unwrap()` on numeric fields aborts the whole build inside a rayon worker with no file/row context. | fixed — errors name the file and row |
| P12 | refactor | `prepare.rs` | `valid_trip_indices` is redundant (leg-shape builder already rejects short trips); stop_times re-grouped and re-sorted after already being sorted; best-leg selection written twice with a needless clone. | fixed |
| P13 | perf | `gtfs.rs`, `binary.rs` | Events sorted three times (once in the pattern builder for an order nothing consumes); Morton key recomputed inside the sort comparator for millions of nodes. | fixed |
| P14 | refactor | `binary.rs`, `transit-data/lib.rs` | `write_pco_u32/i32` and `read_pco_u32/i32` are identical modulo type; inline `simple_compress` duplicates the helper; `FlatEvent` copied field by field. | fixed — generic over `pco::data_types::Number` |
| P15 | refactor | cross-crate | Grid cell sizes triplicated, YYYYMMDD decoding ×4, `Color` defined twice, format version literal in writer and reader, bbox `cos_lat` three ways — because `transit-prep` does not depend on `transit-data`. | fixed — `transit-prep` now depends on `transit-data`, which exports `FORMAT_VERSION`, the grid cell constants, `Color::from_hex` and the date decoder; the three `cos_lat` computations serve different bboxes and were left |
| P16 | refactor | `binary.rs`, `transit-data/lib.rs` | `pattern_id` and per-pattern `max_time` are written and read but never consumed; `OsmNode.index` and `Stop.id` are write-only; `total_sentinels` is a hard-coded 0 reported as a count. | fixed — dead stat removed; the unused per-pattern fields were dropped with the v13 format bump |
| P17 | nit | docs | `binary.rs` header still says v11 and "u32 dates"; README claims shapes are trimmed to the bbox (no such code); Moscow's longitude grid cell is narrower than the 400 m snap radius. | fixed (docs); snap radius left as is |
| P18 | perf-high | `binary.rs`, `transit-data/lib.rs`, `profile.rs` | Every pattern carried two offset arrays over all stops (8 bytes × patterns × stops): Berlin 207 MB, Paris 101 MB, Sydney 87 MB of browser memory, and `Index::new` scanned patterns × stops per query. Only 1–3 % of that grid is occupied. | fixed — format v13 stores one global sparse (stop → pattern, event range, frequency range) index, 0.2–5 MB per city; results verified bit-identical on Chicago, Paris and NYC from reproducible inputs |
| P19 | bug-low | `gtfs.rs`, `graph.rs`, `prepare.rs` | Prep output was not reproducible: hash-map iteration order set service and node numbering, snapped-node order and adjacency order, so two builds of the same inputs differed in Pareto entries for the same query. | fixed — all such orders are now sorted; builds are byte-identical |

