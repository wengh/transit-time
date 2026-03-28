pub mod data;
pub mod router;

use wasm_bindgen::prelude::*;
use data::PreparedData;

use router::NodeResult;

/// SSSP result from a TDD query. Usable from both WASM and native code.
pub struct SsspResult {
    pub results: Vec<NodeResult>,
    pub departure_time: u32,
}

/// Reconstruct path from source to destination.
/// Returns flat array: [node_index, edge_type, route_index, ...]
/// Each node is labeled with the *outgoing* edge (edge to next node),
/// so a boarding stop gets the transit edge type, not walk.
/// The final node keeps its incoming edge type.
/// Reconstruct path from source to destination.
/// Returns flat array: [node_index, edge_type, route_index, ...]
/// Each node is labeled with the *incoming* edge (how we arrived at it).
/// For transit segments, the boarding stop is the last node of the
/// preceding walk segment — callers should use that for display.
pub fn reconstruct_path(_data: &PreparedData, sssp: &SsspResult, destination: u32) -> Vec<u32> {
    let mut path = Vec::new();
    let mut current = destination;

    loop {
        let r = &sssp.results[current as usize];
        if r.arrival_time == u32::MAX {
            return Vec::new();
        }
        path.push([current, r.edge_type, r.route_index]);

        if r.prev_node == u32::MAX || r.prev_node == current {
            break;
        }
        current = r.prev_node;
    }

    path.reverse();
    path.into_iter().flat_map(|[n, e, r]| [n, e, r]).collect()
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

    pub fn node_stop_name(&self, node_idx: u32) -> String {
        let idx = node_idx as usize;
        if idx < self.data.node_is_stop.len() && self.data.node_is_stop[idx] {
            if let Some(&stop_idx) = self.data.node_stop_indices[idx].first() {
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

    pub fn snap_to_node(&self, lat: f64, lon: f64) -> u32 {
        router::snap_to_node(&self.data, lat, lon)
    }

    /// Run single-departure TDD. Returns travel times (NaN for unreached).
    pub fn run_tdd(
        &self,
        source_node: u32,
        departure_time: u32,
        pattern_index: u32,
        transfer_slack: u32,
    ) -> Vec<f64> {
        let result = router::run_tdd(
            &self.data, source_node, departure_time,
            pattern_index as usize, transfer_slack,
        );
        result.iter().map(|r| {
            if r.arrival_time == u32::MAX { f64::NAN } else { (r.arrival_time - departure_time) as f64 }
        }).collect()
    }

    /// Run TDD for a specific day of week. Returns travel times (NaN for unreached).
    pub fn run_tdd_for_day(
        &self,
        source_node: u32,
        departure_time: u32,
        day_of_week: u8,
        transfer_slack: u32,
        max_time: u32,
    ) -> Vec<f64> {
        let pattern_indices = router::patterns_for_day(&self.data, day_of_week);
        let result = router::run_tdd_multi(
            &self.data, source_node, departure_time,
            &pattern_indices, transfer_slack, max_time,
        );
        result.iter().map(|r| {
            if r.arrival_time == u32::MAX { f64::NAN } else { (r.arrival_time - departure_time) as f64 }
        }).collect()
    }

    /// Run sampled TDD over a time window. Returns averaged travel times.
    pub fn run_tdd_sampled_for_day(
        &self,
        source_node: u32,
        window_start: u32,
        window_end: u32,
        n_samples: u32,
        day_of_week: u8,
        transfer_slack: u32,
        max_time: u32,
    ) -> Vec<f64> {
        let pattern_indices = router::patterns_for_day(&self.data, day_of_week);
        let num_nodes = self.data.num_nodes;
        let mut sum_times = vec![0.0f64; num_nodes];
        let mut count = vec![0u32; num_nodes];

        let step = if n_samples > 1 {
            (window_end - window_start) / (n_samples - 1)
        } else { 0 };

        for i in 0..n_samples {
            let dep_time = window_start + i * step;
            let result = router::run_tdd_multi(
                &self.data, source_node, dep_time,
                &pattern_indices, transfer_slack, max_time,
            );
            for (j, r) in result.iter().enumerate() {
                if r.arrival_time != u32::MAX {
                    sum_times[j] += (r.arrival_time - dep_time) as f64;
                    count[j] += 1;
                }
            }
        }

        sum_times.iter().zip(count.iter())
            .map(|(&s, &c)| if c > 0 { s / c as f64 } else { f64::NAN })
            .collect()
    }

    /// Run TDD and return full SSSP result for path reconstruction.
    pub fn run_tdd_full_for_day(
        &self,
        source_node: u32,
        departure_time: u32,
        day_of_week: u8,
        transfer_slack: u32,
        max_time: u32,
    ) -> WasmSsspResult {
        let pat_indices = router::patterns_for_day(&self.data, day_of_week);
        let results = router::run_tdd_multi(
            &self.data, source_node, departure_time,
            &pat_indices, transfer_slack, max_time,
        );
        WasmSsspResult { inner: SsspResult { results, departure_time } }
    }

    pub fn node_arrival_time(&self, sssp: &WasmSsspResult, node: u32) -> u32 {
        sssp.inner.results[node as usize].arrival_time
    }

    pub fn sssp_departure_time(&self, sssp: &WasmSsspResult) -> u32 {
        sssp.inner.departure_time
    }

    pub fn reconstruct_path(&self, sssp: &WasmSsspResult, destination: u32) -> Vec<u32> {
        reconstruct_path(&self.data, &sssp.inner, destination)
    }

    /// Get the shape polyline for a route between two stops (identified by node indices).
    /// Returns flat array [lat, lon, lat, lon, ...] of the sub-polyline, or empty if no shape.
    pub fn route_shape_between(&self, route_idx: u32, from_node: u32, to_node: u32) -> Vec<f64> {
        let ri = route_idx as usize;
        if ri >= self.data.route_shapes.len() {
            return Vec::new();
        }
        let shape_id = &self.data.route_shapes[ri];
        if shape_id.is_empty() {
            return Vec::new();
        }
        let points = match self.data.shapes.get(shape_id) {
            Some(p) if p.len() >= 2 => p,
            _ => return Vec::new(),
        };

        let from_lat = self.data.nodes[from_node as usize].lat;
        let from_lon = self.data.nodes[from_node as usize].lon;
        let to_lat = self.data.nodes[to_node as usize].lat;
        let to_lon = self.data.nodes[to_node as usize].lon;

        // Find closest point on shape to each stop
        let mut best_from = 0usize;
        let mut best_from_d = f64::MAX;
        let mut best_to = 0usize;
        let mut best_to_d = f64::MAX;
        for (i, &(lat, lon)) in points.iter().enumerate() {
            let df = (lat - from_lat).powi(2) + (lon - from_lon).powi(2);
            let dt = (lat - to_lat).powi(2) + (lon - to_lon).powi(2);
            if df < best_from_d { best_from_d = df; best_from = i; }
            if dt < best_to_d { best_to_d = dt; best_to = i; }
        }

        // Ensure from < to along shape (swap if reversed)
        let (start, end) = if best_from <= best_to {
            (best_from, best_to)
        } else {
            (best_to, best_from)
        };

        let mut result = Vec::with_capacity((end - start + 1) * 2);
        for i in start..=end {
            result.push(points[i].0);
            result.push(points[i].1);
        }
        result
    }
}
