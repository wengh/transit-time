pub mod data;
pub mod path_display;
pub mod profile;
pub mod router;
pub mod sssp_path;

use data::PreparedData;
use profile::ProfileRouter as _;
use wasm_bindgen::prelude::*;

use rayon::prelude::*;
pub use wasm_bindgen_rayon::init_thread_pool;

use std::collections::HashMap;

use pco;
use router::{BoardingEvent, NodeResult};

/// Whether the rayon thread pool has been initialized (via `initThreadPool` from JS).
/// When false, we fall back to sequential iteration.
static RAYON_INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// High bit of `BoardingEvent::event_index` set for frequency-based boardings.
/// Lower 31 bits encode the `freq_index` (index into `PatternData::frequency_routes`).
pub const FREQ_BOARDING_FLAG: u32 = 0x8000_0000;

/// Called from the JS-side `initThreadPool` wrapper to mark rayon as ready.
#[wasm_bindgen(js_name = "__markRayonReady")]
pub fn mark_rayon_ready() {
    RAYON_INITIALIZED.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn rayon_available() -> bool {
    RAYON_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Map + collect, using par_iter when rayon is available, plain iter otherwise.
fn par_map_collect<R: Send>(
    range: std::ops::Range<u32>,
    f: impl Fn(u32) -> R + Sync + Send,
) -> Vec<R> {
    if rayon_available() {
        range.into_par_iter().map(f).collect()
    } else {
        range.into_iter().map(f).collect()
    }
}

/// SSSP result from a TDD query. Usable from both WASM and native code.
pub struct SsspResult {
    pub results: Vec<NodeResult>,
    pub boarding_events: HashMap<u32, BoardingEvent>,
    pub departure_time: u32,
}

// === WASM wrappers ===

#[wasm_bindgen]
pub struct WasmSsspResult {
    inner: SsspResult,
}

/// Thin WASM adapter over [`profile::ProfileRouting`]. All logic lives inside
/// the inner pure-Rust struct; this exists only to serialize outputs for JS.
#[wasm_bindgen]
pub struct WasmProfileRouting {
    inner: profile::ProfileRouting,
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

    pub fn window_start(&self) -> u32 {
        self.inner.isochrone().window_start
    }

    pub fn window_end(&self) -> u32 {
        self.inner.isochrone().window_end
    }

    /// All Pareto-optimal paths to `destination`, JSON-serialized. The TS side
    /// calls `JSON.parse` once per hover. Requires a `TransitRouter` for access
    /// to the underlying `PreparedData` (names, colours).
    ///
    /// Emits `Vec<PathView>` — each element flattens a `Path` and adds the
    /// display strings and dominant route colour computed from that path.
    pub fn optimal_paths(&self, router: &TransitRouter, destination: u32) -> String {
        let paths = self.inner.optimal_paths(&router.data, destination);
        let views: Vec<path_display::PathView> = paths
            .iter()
            .map(|p| path_display::PathView::new(&router.data, p))
            .collect();
        serde_json::to_string(&views).unwrap_or_else(|_| "[]".to_string())
    }
}

#[wasm_bindgen]
pub struct TransitRouter {
    data: PreparedData,
}

#[wasm_bindgen]
impl TransitRouter {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8]) -> Result<TransitRouter, JsValue> {
        let data = data::load(bytes).map_err(|e| JsValue::from_str(&format!("{}", e)))?;
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
        self.data.stop_to_node[idx as usize]
    }

    pub fn route_name(&self, idx: u32) -> String {
        if (idx as usize) < self.data.route_names.len() {
            self.data.route_names[idx as usize].clone()
        } else {
            String::new()
        }
    }

    pub fn route_color(&self, idx: u32) -> String {
        if (idx as usize) < self.data.route_colors.len() {
            if let Some(color) = self.data.route_colors[idx as usize] {
                return color.to_hex();
            }
        }
        String::new()
    }

    pub fn node_stop_name(&self, node_idx: u32) -> String {
        if let Some(&stop_idx) = self.data.node_to_stop.get(&node_idx) {
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

    /// Run sampled TDD over a time window. Returns individual full results.
    pub fn run_tdd_sampled_full_for_date(
        &self,
        source_node: u32,
        window_start: u32,
        window_end: u32,
        n_samples: u32,
        date: u32,
        transfer_slack: u32,
        max_time: u32,
    ) -> Vec<WasmSsspResult> {
        let pattern_indices = router::patterns_for_date(&self.data, date);
        let step = if n_samples > 1 {
            (window_end - window_start) / (n_samples - 1)
        } else {
            0
        };

        par_map_collect(0..n_samples, |i| {
            let dep_time = window_start + i * step;
            let (results, boarding_events) = router::run_tdd_multi(
                &self.data,
                source_node,
                dep_time,
                &pattern_indices,
                transfer_slack,
                max_time,
            );
            WasmSsspResult {
                inner: SsspResult {
                    results,
                    boarding_events,
                    departure_time: dep_time,
                },
            }
        })
    }

    /// Run TDD and return full SSSP result for path reconstruction.
    pub fn run_tdd_full_for_date(
        &self,
        source_node: u32,
        departure_time: u32,
        date: u32,
        transfer_slack: u32,
        max_time: u32,
    ) -> WasmSsspResult {
        let pat_indices = router::patterns_for_date(&self.data, date);
        let (results, boarding_events) = router::run_tdd_multi(
            &self.data,
            source_node,
            departure_time,
            &pat_indices,
            transfer_slack,
            max_time,
        );
        WasmSsspResult {
            inner: SsspResult {
                results,
                boarding_events,
                departure_time,
            },
        }
    }

    pub fn node_arrival_time(&self, sssp: &WasmSsspResult, node: u32) -> u32 {
        let r = &sssp.inner.results[node as usize];
        if r.arrival_delta == u16::MAX {
            u32::MAX
        } else {
            sssp.inner.departure_time + r.arrival_delta as u32
        }
    }

    pub fn node_leave_home(&self, _sssp: &WasmSsspResult, _node: u32) -> u32 {
        0 // leave_home is now transient and not retained after routing
    }

    pub fn node_boarding_time(&self, sssp: &WasmSsspResult, node: u32) -> u32 {
        let r = &sssp.inner.results[node as usize];
        if r.boarding_delta == u16::MAX {
            0 // walk edge or source — no boarding time
        } else {
            sssp.inner.departure_time + r.boarding_delta as u32
        }
    }

    pub fn sssp_departure_time(&self, sssp: &WasmSsspResult) -> u32 {
        sssp.inner.departure_time
    }

    /// Reconstruct the single optimal journey to `destination` as a
    /// JSON-serialized `Option<PathView>` (null when unreachable).
    ///
    /// Mirror of `WasmProfileRouting::optimal_paths`' element shape so the
    /// frontend can consume both modes through the same path of code.
    pub fn sssp_optimal_path(&self, sssp: &WasmSsspResult, destination: u32) -> String {
        let path = sssp_path::optimal_path(&self.data, &sssp.inner, destination);
        match path {
            Some(p) => {
                let view = path_display::PathView::new(&self.data, &p);
                serde_json::to_string(&view).unwrap_or_else(|_| "null".to_string())
            }
            None => "null".to_string(),
        }
    }

    /// Run profile routing over `[window_start, window_end]`. Returns an opaque
    /// handle containing the isochrone (for map rendering) and internal Pareto
    /// frontier state (for subsequent `optimal_paths` queries).
    pub fn compute_profile(
        &self,
        source_node: u32,
        window_start: u32,
        window_end: u32,
        date: u32,
        transfer_slack: u32,
        max_time: u32,
    ) -> WasmProfileRouting {
        let query = profile::ProfileQuery {
            source_node,
            window_start,
            window_end,
            date,
            transfer_slack,
            max_time,
        };
        WasmProfileRouting {
            inner: profile::ProfileRouting::compute(&self.data, &query),
        }
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
        let from_stop = match self.data.node_to_stop.get(&from_node) {
            Some(&s) => s,
            None => return Vec::new(),
        };
        let to_stop = match self.data.node_to_stop.get(&to_node) {
            Some(&s) => s,
            None => return Vec::new(),
        };

        let key = (route_idx, from_stop, to_stop);
        let idx = match self.data.leg_shape_keys.binary_search(&key) {
            Ok(i) => i,
            Err(_) => return Vec::new(),
        };

        let start = self.data.leg_shapes.offsets[idx] as usize;
        let end = self.data.leg_shapes.offsets[idx + 1] as usize;
        let compressed = &self.data.leg_shapes.data[start..end];
        if compressed.is_empty() {
            return Vec::new();
        }

        let coords_u32: Vec<u32> = match pco::standalone::simple_decompress(compressed) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let mut result = Vec::with_capacity(coords_u32.len());
        for chunk in coords_u32.chunks(2) {
            if chunk.len() == 2 {
                result.push(f32::from_bits(chunk[0]) as f64);
                result.push(f32::from_bits(chunk[1]) as f64);
            }
        }
        result
    }
}
