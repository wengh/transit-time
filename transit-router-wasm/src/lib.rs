//! WASM bindings for the pure-Rust `transit-router` engine. Browser-only.
//!
//! Everything in this crate is glue: type translation between
//! [`transit_router`] types and JS/WebAssembly, plus JSON serialization for
//! the path-display boundary. The routing algorithm lives in
//! [`transit_router::profile`]; the idiomatic-Rust facade in
//! [`transit_router::api`] is what this crate wraps.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::ControlFlow;

use chrono::{Duration, NaiveDate};
use serde::Serialize;
use transit_data::PreparedData;
use transit_router::path_display;
use transit_router::{Isochrone, IsochroneParams, NodeId, Path, Router, SinceMidnight, TimeWindow};
use wasm_bindgen::prelude::*;

/// Re-export wasm-bindgen-rayon's thread-pool initializer. The JS side calls
/// `initThreadPool(navigator.hardwareConcurrency)` once after `init()`.
pub use wasm_bindgen_rayon::init_thread_pool;

/// Called from the JS-side `initThreadPool` wrapper to mark rayon as ready
/// in the underlying `transit-router` crate. Until this fires, the engine
/// uses single-threaded fallbacks.
#[wasm_bindgen(js_name = "__markRayonReady")]
pub fn mark_rayon_ready() {
    transit_router::mark_rayon_ready();
}

// === WASM wrappers ===

/// Thin WASM adapter over [`Isochrone`]. All logic lives inside the inner
/// pure-Rust struct; this exists only to serialize outputs for JS.
#[wasm_bindgen]
pub struct WasmProfileRouting {
    inner: Isochrone,
    /// Persistent scratch buffer for [`Self::travel_times_at_into`]. JS keeps
    /// a `Uint16Array` view over this memory (via `travel_times_buffer_ptr`)
    /// and reuses it across every animation frame — no per-frame Vec
    /// allocation and no copy across the JS↔WASM boundary. Length is fixed
    /// at `num_nodes` for the lifetime of this isochrone.
    scratch: RefCell<Vec<u16>>,
}

#[wasm_bindgen]
impl WasmProfileRouting {
    /// Per-node mean travel time (seconds) over reachable departures in the
    /// window. Undefined when `reachable_fractions()[i] == 0` — consumers
    /// must check that first. Length = `num_nodes`.
    pub fn mean_travel_times(&self) -> Vec<u16> {
        self.inner.mean_travel_time().to_vec()
    }

    /// Per-node fraction of the departure window during which the node is
    /// reachable within `max_time`, quantized over `u16::MAX`
    /// (i.e. fraction = `value / 65535`). Length = `num_nodes`.
    pub fn reachable_fractions(&self) -> Vec<u16> {
        self.inner.reachable_fraction().to_vec()
    }

    pub fn num_threads(&self) -> u32 {
        self.inner.num_threads_used()
    }

    /// Per-node travel time (seconds) for a single `departure` (seconds since
    /// midnight) — one animation frame. `u16::MAX` marks unreachable nodes.
    ///
    /// Allocates and returns a fresh `Vec<u16>` every call. Prefer
    /// [`Self::travel_times_at_into`] + [`Self::travel_times_buffer_ptr`] on
    /// the hot animation path — this variant remains for callers that don't
    /// hold a stable Uint16Array view.
    pub fn travel_times_at(&self, departure: u32) -> Vec<u16> {
        self.inner
            .travel_times_at(SinceMidnight::from_seconds(departure))
    }

    pub fn num_nodes(&self) -> u32 {
        self.inner.num_nodes() as u32
    }

    /// Raw pointer into WASM linear memory for the internal scratch buffer.
    /// JS constructs `new Uint16Array(wasm.memory.buffer, ptr, num_nodes())`
    /// once per query and reads it after every [`Self::travel_times_at_into`]
    /// call — zero-copy. Re-fetch the view if `wasm.memory.buffer` is ever
    /// replaced (memory growth detaches existing views).
    pub fn travel_times_buffer_ptr(&self) -> *const u16 {
        self.scratch.borrow().as_ptr()
    }

    /// Compute one animation frame into the persistent scratch buffer. JS
    /// reads the result via the `Uint16Array` view it built from
    /// [`Self::travel_times_buffer_ptr`]. No allocation, no copy.
    pub fn travel_times_at_into(&self, departure: u32) {
        let mut buf = self.scratch.borrow_mut();
        self.inner
            .travel_times_at_into(SinceMidnight::from_seconds(departure), &mut buf);
    }

    pub fn window_start(&self) -> u32 {
        self.inner.params().window.start.as_seconds()
    }

    pub fn window_end(&self) -> u32 {
        self.inner.params().window.end.as_seconds()
    }

    /// All Pareto-optimal paths to `destination`, JSON-serialized. The TS side
    /// calls `JSON.parse` once per hover. The `_router` parameter is kept for
    /// JS-side ABI stability (the frontend still passes it); the path-display
    /// data is resolved via the `Isochrone`'s own shared `PreparedData`.
    ///
    /// Emits a JSON object containing `{ paths: Vec<PathView>, representativeIndex: Option<usize> }`.
    pub fn optimal_paths(&self, _router: &TransitRouter, destination: u32) -> String {
        let data = self.inner.data();
        let paths = self.inner.paths(NodeId(destination));
        let views: Vec<PathView> = paths.iter().map(|p| PathView::new(data, p)).collect();

        let representative_index = compute_representative_index(&paths, self.window_start());

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct OptimalPathsResponse<'a> {
            paths: Vec<PathView<'a>>,
            representative_index: Option<usize>,
        }

        let response = OptimalPathsResponse {
            paths: views,
            representative_index,
        };

        serde_json::to_string(&response)
            .unwrap_or_else(|_| "{\"paths\":[],\"representativeIndex\":null}".to_string())
    }
}

/// JSON-boundary wrapper: flattens `Path`'s fields at the top level and adds
/// derived data (display strings, colour). Lives in the wasm wrapper because
/// it only matters at the JS serialization boundary; pure-Rust callers work
/// with `&Path` directly.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathView<'a> {
    #[serde(flatten)]
    pub path: &'a Path,
    pub display: path_display::PathDisplay,
    pub dominant_route_color_hex: Option<String>,
}

impl<'a> PathView<'a> {
    pub fn new(data: &PreparedData, path: &'a Path) -> Self {
        Self {
            display: path_display::display(path),
            dominant_route_color_hex: path_display::dominant_route_color(data, path),
            path,
        }
    }
}

/// Compute the mode of the Pareto frontier paths, using the transit routes
/// as signature and weighted by responsible departure-time length. Returns
/// the path with median total time among that family.
fn compute_representative_index(paths: &[Path], window_start: u32) -> Option<usize> {
    if paths.is_empty() {
        return None;
    }

    struct Family {
        paths: Vec<usize>,
        weight: u32,
    }

    let mut families: HashMap<Vec<u32>, Family> = HashMap::new();

    for (i, path) in paths.iter().enumerate() {
        let signature = path
            .segments
            .iter()
            .filter_map(|s| s.route_index)
            .collect::<Vec<_>>();

        let prev_departure = if i == 0 {
            window_start
        } else {
            paths[i - 1].home_departure
        };

        let weight = path.home_departure.saturating_sub(prev_departure);
        let family = families.entry(signature).or_insert(Family {
            paths: Vec::new(),
            weight: 0,
        });
        family.weight += weight;
        family.paths.push(i);
    }

    let mut mode_indices = families
        .into_iter()
        .max_by_key(|(_, family)| (family.weight, family.paths[0]))
        .unwrap()
        .1
        .paths;

    mode_indices.sort_by_key(|&idx| paths[idx].total_time);
    Some(mode_indices[mode_indices.len() / 2])
}

#[wasm_bindgen]
pub struct TransitRouter {
    inner: Router,
}

#[wasm_bindgen]
impl TransitRouter {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8]) -> Result<TransitRouter, JsValue> {
        let inner = Router::from_bytes(bytes).map_err(|e| JsValue::from_str(&format!("{e}")))?;
        Ok(TransitRouter { inner })
    }

    pub fn num_nodes(&self) -> u32 {
        self.inner.data().num_nodes as u32
    }

    pub fn num_edges(&self) -> u32 {
        self.inner.data().num_edges as u32
    }

    pub fn num_stops(&self) -> u32 {
        self.inner.data().num_stops as u32
    }

    pub fn num_routes(&self) -> u32 {
        self.inner.data().route_names.len() as u32
    }

    pub fn node_lat(&self, idx: u32) -> f64 {
        self.inner.data().nodes[idx as usize].lat
    }

    pub fn node_lon(&self, idx: u32) -> f64 {
        self.inner.data().nodes[idx as usize].lon
    }

    /// Return all node positions as a flat `[lat0, lon0, lat1, lon1, …]`
    /// array. Called once after data load, cached on JS side.
    pub fn all_node_coords(&self) -> Vec<f64> {
        let data = self.inner.data();
        let mut out = Vec::with_capacity(data.num_nodes * 2);
        for n in &data.nodes {
            out.push(n.lat);
            out.push(n.lon);
        }
        out
    }

    pub fn stop_name(&self, idx: u32) -> String {
        self.inner.data().stops[idx as usize].name.clone()
    }

    pub fn stop_node(&self, idx: u32) -> u32 {
        self.inner.data().stop_to_node(idx)
    }

    pub fn route_name(&self, idx: u32) -> String {
        let data = self.inner.data();
        if (idx as usize) < data.route_names.len() {
            data.route_names[idx as usize].clone()
        } else {
            String::new()
        }
    }

    /// Hex route colour for `idx`, brightness-adjusted via
    /// [`path_display::route_color`] so callers don't re-run the luminance
    /// check in JS. Empty string when the route has no colour.
    pub fn route_color(&self, idx: u32) -> String {
        path_display::route_color(self.inner.data(), idx).unwrap_or_default()
    }

    pub fn node_stop_name(&self, node_idx: u32) -> String {
        let data = self.inner.data();
        if let Some(stop_idx) = data.node_to_stop(node_idx) {
            return data.stops[stop_idx as usize].name.clone();
        }
        String::new()
    }

    pub fn num_patterns(&self) -> u32 {
        self.inner.data().patterns.len() as u32
    }

    pub fn pattern_day_mask(&self, idx: u32) -> u8 {
        self.inner.data().patterns[idx as usize].day_mask
    }

    pub fn num_patterns_for_date(&self, date: u32) -> u32 {
        let nd = decode_yyyymmdd(date);
        self.inner.patterns_for_date(nd) as u32
    }

    pub fn snap_to_node(&self, lat: f64, lon: f64) -> Option<u32> {
        self.inner.snap(lat, lon).map(|n| n.get())
    }

    /// Run profile routing over `[window_start, window_end]`. Returns an
    /// opaque handle containing the isochrone (for map rendering) and the
    /// internal Pareto frontier (for subsequent `optimal_paths` queries).
    ///
    /// `progress_cb`: called with `(done, total)` from the transit phase;
    /// returning truthy from JS cancels and produces `None`.
    pub fn compute_profile(
        &self,
        source_node: u32,
        window_start: u32,
        window_end: u32,
        date: u32,
        transfer_slack: u32,
        max_time: u32,
        progress_cb: Option<js_sys::Function>,
        is_warmup: bool,
    ) -> Option<WasmProfileRouting> {
        let params = IsochroneParams {
            source: NodeId(source_node),
            date: decode_yyyymmdd(date),
            window: TimeWindow {
                start: SinceMidnight::from_seconds(window_start),
                end: SinceMidnight::from_seconds(window_end),
            },
            max_time: Duration::seconds(max_time as i64),
            transfer_slack: Duration::seconds(transfer_slack as i64),
            max_parallelism: None,
        };
        let on_progress = |done: usize, total: usize| {
            if let Some(ref f) = progress_cb {
                let cancel = f
                    .call2(&JsValue::NULL, &JsValue::from(done), &JsValue::from(total))
                    .map(|v| v.is_truthy())
                    .unwrap_or(false);
                if cancel {
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        };
        let result = if is_warmup {
            self.inner.isochrone_warmup(params, on_progress)
        } else {
            self.inner.isochrone(params, on_progress)
        };
        result.ok().map(|inner| {
            // Pre-allocate the per-frame scratch buffer once. Calloc'd pages
            // are nearly free on native and fault in lazily on wasm.
            let scratch = RefCell::new(vec![u16::MAX; inner.num_nodes()]);
            WasmProfileRouting { inner, scratch }
        })
    }

    /// Chain per-leg GTFS shapes for a transit segment, or build a straight-
    /// line polyline for a walk segment, from a node sequence. Returns a flat
    /// `[lat, lon, …]` `f32` array.
    pub fn segment_shape(&self, route_index: Option<u32>, nodes: Vec<u32>) -> Vec<f32> {
        let ri = match route_index {
            None => None,
            Some(r) if r == u32::MAX => None,
            Some(r) if r <= u16::MAX as u32 - 1 => Some(r as u16),
            Some(_) => None,
        };
        path_display::segment_shape(self.inner.data(), ri, &nodes)
    }
}

fn decode_yyyymmdd(date: u32) -> NaiveDate {
    transit_data::yyyymmdd_to_naive_date_opt(date)
        .unwrap_or_else(|| panic!("invalid YYYYMMDD from JS boundary: {date}"))
}
