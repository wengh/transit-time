//! WASM bindings for the pure-Rust `transit-router` engine. Browser-only.
//!
//! Everything in this crate is glue: type translation between `transit_router`
//! types and JS/WebAssembly, plus JSON serialization for the path-display
//! boundary. The routing algorithm lives in `transit_router::profile`.

use std::collections::HashMap;

use serde::Serialize;
use transit_data::PreparedData;
use transit_router::path_display;
use transit_router::profile::{self, ProfileRouter as _};
use transit_router::router;
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

/// Thin WASM adapter over [`profile::SplitProfileRouting`]. All logic lives
/// inside the inner pure-Rust struct; this exists only to serialize outputs
/// for JS.
#[wasm_bindgen]
pub struct WasmProfileRouting {
    inner: profile::SplitProfileRouting,
}

#[wasm_bindgen]
impl WasmProfileRouting {
    /// Per-node mean travel time (seconds) over reachable departures in the
    /// window. Undefined when `reachable_fractions()[i] == 0` — consumers must
    /// check that first. Length = `num_nodes`.
    pub fn mean_travel_times(&self) -> Vec<u16> {
        self.inner.isochrone().mean_travel_time.clone()
    }

    /// Per-node fraction of the departure window during which the node is
    /// reachable within `max_time`, quantized over `u16::MAX`
    /// (i.e. fraction = `value / 65535`). Length = `num_nodes`.
    pub fn reachable_fractions(&self) -> Vec<u16> {
        self.inner.isochrone().reachable_fraction.clone()
    }

    pub fn num_threads(&self) -> u32 {
        self.inner.isochrone().num_threads
    }

    pub fn window_start(&self) -> u32 {
        self.inner.isochrone().query.window_start
    }

    pub fn window_end(&self) -> u32 {
        self.inner.isochrone().query.window_end
    }

    /// All Pareto-optimal paths to `destination`, JSON-serialized. The TS side
    /// calls `JSON.parse` once per hover. Requires a `TransitRouter` for access
    /// to the underlying `PreparedData` (names, colours).
    ///
    /// Emits a JSON object containing `{ paths: Vec<PathView>, representativeIndex: Option<usize> }`.
    pub fn optimal_paths(&self, router: &TransitRouter, destination: u32) -> String {
        let paths = self.inner.optimal_paths(&router.data, destination);
        let views: Vec<PathView> = paths
            .iter()
            .map(|p| PathView::new(&router.data, p))
            .collect();

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
    pub path: &'a profile::Path,
    pub display: path_display::PathDisplay,
    pub dominant_route_color_hex: Option<String>,
}

impl<'a> PathView<'a> {
    pub fn new(data: &PreparedData, path: &'a profile::Path) -> Self {
        Self {
            display: path_display::display(path),
            dominant_route_color_hex: path_display::dominant_route_color(data, path),
            path,
        }
    }
}

/// Computes the mode of the Pareto frontier paths,
/// using the transit routes as signature and weighted by responsible departure time length.
/// Returns the path with median total time among the family of paths.
fn compute_representative_index(paths: &[profile::Path], window_start: u32) -> Option<usize> {
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
    data: PreparedData,
}

#[wasm_bindgen]
impl TransitRouter {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8]) -> Result<TransitRouter, JsValue> {
        let data = transit_data::load(bytes).map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(TransitRouter { data })
    }

    pub fn num_nodes(&self) -> u32 {
        self.data.num_nodes as u32
    }

    pub fn num_edges(&self) -> u32 {
        self.data.num_edges as u32
    }

    pub fn num_stops(&self) -> u32 {
        self.data.num_stops as u32
    }

    pub fn num_routes(&self) -> u32 {
        self.data.route_names.len() as u32
    }

    pub fn node_lat(&self, idx: u32) -> f64 {
        self.data.nodes[idx as usize].lat
    }

    pub fn node_lon(&self, idx: u32) -> f64 {
        self.data.nodes[idx as usize].lon
    }

    /// Return all node positions as flat [lat0, lon0, lat1, lon1, ...] array.
    /// Called once after data load, cached on JS side.
    pub fn all_node_coords(&self) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.data.num_nodes * 2);
        for n in &self.data.nodes {
            out.push(n.lat);
            out.push(n.lon);
        }
        out
    }

    pub fn stop_name(&self, idx: u32) -> String {
        self.data.stops[idx as usize].name.clone()
    }

    pub fn stop_node(&self, idx: u32) -> u32 {
        self.data.stop_to_node(idx)
    }

    pub fn route_name(&self, idx: u32) -> String {
        if (idx as usize) < self.data.route_names.len() {
            self.data.route_names[idx as usize].clone()
        } else {
            String::new()
        }
    }

    /// Hex route colour for `idx`, brightness-adjusted via
    /// [`path_display::adjust_color_for_visibility`] so callers don't re-run
    /// the luminance check in JS. Empty string when the route has no colour.
    pub fn route_color(&self, idx: u32) -> String {
        if (idx as usize) < self.data.route_colors.len() {
            if let Some(color) = self.data.route_colors[idx as usize] {
                return path_display::adjust_color_for_visibility(&color.to_hex())
                    .unwrap_or_default();
            }
        }
        String::new()
    }

    pub fn node_stop_name(&self, node_idx: u32) -> String {
        if let Some(stop_idx) = self.data.node_to_stop(node_idx) {
            return self.data.stops[stop_idx as usize].name.clone();
        }
        String::new()
    }

    pub fn num_patterns(&self) -> u32 {
        self.data.patterns.len() as u32
    }

    pub fn pattern_day_mask(&self, idx: u32) -> u8 {
        self.data.patterns[idx as usize].day_mask
    }

    pub fn num_patterns_for_date(&self, date: u32) -> u32 {
        router::patterns_for_date(&self.data, date).len() as u32
    }

    pub fn snap_to_node(&self, lat: f64, lon: f64) -> Option<u32> {
        router::snap_to_node(&self.data, lat, lon)
    }

    /// Run profile routing over `[window_start, window_end]`. Returns an opaque
    /// handle containing the isochrone (for map rendering) and internal Pareto
    /// frontier state (for subsequent `optimal_paths` queries).
    ///
    /// `progress_cb` (optional): called with `(done, total)` during the
    /// transit phase so the caller can report progress to the UI. The JS
    /// callback returns truthy to request cancellation; if it does (or any
    /// other cancellation path fires) this method returns `None`.
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
        let query = profile::ProfileQuery {
            source_node,
            window_start,
            window_end,
            date,
            transfer_slack,
            max_time,
            is_warmup: is_warmup,
        };
        let cb = progress_cb;
        let result = profile::SplitProfileRouting::compute(&self.data, &query, |done, total| {
            if let Some(ref f) = cb {
                let cancel = f
                    .call2(&JsValue::NULL, &JsValue::from(done), &JsValue::from(total))
                    .map(|v| v.is_truthy())
                    .unwrap_or(false);
                if cancel {
                    return std::ops::ControlFlow::Break(());
                }
            }
            std::ops::ControlFlow::Continue(())
        });
        result
            .continue_value()
            .map(|r| WasmProfileRouting { inner: r })
    }

    /// Chain per-leg GTFS shapes for a transit segment, or build a straight-line
    /// polyline for a walk segment, from a node sequence. Flat `[lat, lon, ...]` f32s.
    ///
    /// `route_index`: `None`/`u32::MAX` for walk segments (straight line between
    /// the two nodes); `Some(r)` for transit (chain per-leg shapes with straight-line
    /// fallback when shape data is missing).
    pub fn segment_shape(&self, route_index: Option<u32>, nodes: Vec<u32>) -> Vec<f32> {
        let ri = match route_index {
            None => None,
            Some(r) if r == u32::MAX => None,
            Some(r) if r <= u16::MAX as u32 - 1 => Some(r as u16),
            Some(_) => None,
        };
        path_display::segment_shape(&self.data, ri, &nodes)
    }

    /// Get the shape polyline for a single leg between two consecutive stops (by node index).
    /// Returns flat array [lat, lon, lat, lon, ...] of the pre-sliced sub-polyline, or empty.
    pub fn route_shape_between(&self, route_idx: u32, from_node: u32, to_node: u32) -> Vec<f64> {
        let Some(from_stop) = self.data.node_to_stop(from_node) else {
            return Vec::new();
        };
        let Some(to_stop) = self.data.node_to_stop(to_node) else {
            return Vec::new();
        };

        let key = (route_idx, from_stop, to_stop);
        let idx = match self.data.leg_shape_keys.binary_search(&key) {
            Ok(i) => i,
            Err(_) => return Vec::new(),
        };

        let start = self.data.leg_shape_offsets[idx] as usize;
        let end = self.data.leg_shape_offsets[idx + 1] as usize;
        let lats = &self.data.leg_shapes_lat[start..end];
        let lons = &self.data.leg_shapes_lon[start..end];

        let min_lat = self.data.coord_min_lat;
        let min_lon = self.data.coord_min_lon;
        let lat_scale = self.data.coord_lat_scale;
        let lon_scale = self.data.coord_lon_scale;
        let mut result = Vec::with_capacity(lats.len() * 2);
        for i in 0..lats.len() {
            result.push(min_lat + lats[i] as f64 / lat_scale);
            result.push(min_lon + lons[i] as f64 / lon_scale);
        }
        result
    }
}
