pub mod data;
pub mod router;

use data::PreparedData;
use wasm_bindgen::prelude::*;

use rayon::prelude::*;
pub use wasm_bindgen_rayon::init_thread_pool;

use std::collections::HashMap;

use pco;
use router::{BoardingEvent, NodeResult};

/// Whether the rayon thread pool has been initialized (via `initThreadPool` from JS).
/// When false, we fall back to sequential iteration.
static RAYON_INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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

/// Reconstruct path from source to destination.
/// Returns flat array: [node_index, edge_type, route_index, ...]
/// For transit segments, emits all intermediate stops by following the event chain.
pub fn reconstruct_path(data: &PreparedData, sssp: &SsspResult, destination: u32) -> Vec<u32> {
    // Follow prev_node chain to get the coarse path (boarding→alighting per transit leg)
    let mut coarse = Vec::new();
    let mut current = destination;
    loop {
        let r = &sssp.results[current as usize];
        if r.arrival_delta == u16::MAX {
            return Vec::new();
        }
        coarse.push(current);
        if r.prev_node == u32::MAX || r.prev_node == current {
            break;
        }
        current = r.prev_node;
    }
    coarse.reverse();

    let mut result = Vec::new();
    for (ci, &node) in coarse.iter().enumerate() {
        let r = &sssp.results[node as usize];
        let is_transit = r.route_index != u32::MAX;

        if is_transit {
            // Expand intermediate stops from boarding event chain
            if let Some(be) = sssp.boarding_events.get(&node) {
                let pat = &data.patterns[be.pattern_index];
                let boarding_event = &pat.stop_index.events_by_stop.data[be.event_index as usize];
                let mut idx = boarding_event.next_event_index;
                while idx != u32::MAX {
                    let e = &pat.stop_index.events_by_stop.data[idx as usize];
                    let stop_node = data.stop_node_map[e.stop_index as usize];
                    if stop_node == node {
                        break; // reached alighting stop
                    }
                    if stop_node != u32::MAX && e.travel_time > 0 {
                        result.extend_from_slice(&[stop_node, 1, r.route_index]);
                    }
                    idx = e.next_event_index;
                }
            }

            // Emit the alighting stop
            result.extend_from_slice(&[node, 1, r.route_index]);

            // At transit→walk transition, re-emit as walk for dotted line
            let next_is_walk = ci + 1 < coarse.len()
                && sssp.results[coarse[ci + 1] as usize].route_index == u32::MAX;
            if next_is_walk {
                result.extend_from_slice(&[node, 0, u32::MAX]);
            }
        } else {
            result.extend_from_slice(&[node, 0, u32::MAX]);
        }
    }
    result
}

// === WASM wrappers ===

#[wasm_bindgen]
pub struct WasmSsspResult {
    inner: SsspResult,
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
        self.data.stop_node_map[idx as usize]
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
        let idx = node_idx as usize;
        if idx < self.data.node_is_stop.len() && self.data.node_is_stop[idx] {
            if let Some(&stop_idx) = self.data.node_stop_indices.get(node_idx).first() {
                return self.data.stops[stop_idx as usize].name.clone();
            }
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

    pub fn reconstruct_path(&self, sssp: &WasmSsspResult, destination: u32) -> Vec<u32> {
        reconstruct_path(&self.data, &sssp.inner, destination)
    }

    /// Get the shape polyline for a single leg between two consecutive stops (by node index).
    /// Returns flat array [lat, lon, lat, lon, ...] of the pre-sliced sub-polyline, or empty.
    pub fn route_shape_between(&self, route_idx: u32, from_node: u32, to_node: u32) -> Vec<f64> {
        let from_stop = match self.data.node_stop_indices.get(from_node).first() {
            Some(&s) => s,
            None => return Vec::new(),
        };
        let to_stop = match self.data.node_stop_indices.get(to_node).first() {
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
