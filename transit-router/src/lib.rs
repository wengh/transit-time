pub mod data;
pub mod router;

use data::PreparedData;
use wasm_bindgen::prelude::*;

use rayon::prelude::*;
pub use wasm_bindgen_rayon::init_thread_pool;

use pco;
use router::NodeResult;

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
    pub departure_time: u32,
}

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
        let edge_type = if r.route_index == u32::MAX {
            0u32
        } else {
            1u32
        };
        path.push([current, edge_type, r.route_index]);

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
            &self.data,
            source_node,
            departure_time,
            pattern_index as usize,
            transfer_slack,
        );
        result
            .iter()
            .map(|r| {
                if r.arrival_time == u32::MAX {
                    f64::NAN
                } else {
                    (r.arrival_time - departure_time) as f64
                }
            })
            .collect()
    }

    /// Run TDD for a specific date (YYYYMMDD). Returns travel times (NaN for unreached).
    pub fn run_tdd_for_date(
        &self,
        source_node: u32,
        departure_time: u32,
        date: u32,
        transfer_slack: u32,
        max_time: u32,
    ) -> Vec<f64> {
        let pattern_indices = router::patterns_for_date(&self.data, date);
        let result = router::run_tdd_multi(
            &self.data,
            source_node,
            departure_time,
            &pattern_indices,
            transfer_slack,
            max_time,
        );
        result
            .iter()
            .map(|r| {
                if r.arrival_time == u32::MAX {
                    f64::NAN
                } else {
                    (r.arrival_time - departure_time) as f64
                }
            })
            .collect()
    }

    /// Run sampled TDD over a time window. Returns averaged travel times.
    pub fn run_tdd_sampled_for_date(
        &self,
        source_node: u32,
        window_start: u32,
        window_end: u32,
        n_samples: u32,
        date: u32,
        transfer_slack: u32,
        max_time: u32,
    ) -> Vec<f64> {
        let pattern_indices = router::patterns_for_date(&self.data, date);
        let num_nodes = self.data.num_nodes;

        let step = if n_samples > 1 {
            (window_end - window_start) / (n_samples - 1)
        } else {
            0
        };

        let per_sample = par_map_collect(0..n_samples, |i| {
            let dep_time = window_start + i * step;
            let result = router::run_tdd_multi(
                &self.data,
                source_node,
                dep_time,
                &pattern_indices,
                transfer_slack,
                max_time,
            );

            let mut sum_times = vec![0u32; num_nodes];
            let mut count = vec![0u32; num_nodes];

            for (j, r) in result.iter().enumerate() {
                if r.arrival_time != u32::MAX {
                    sum_times[j] += r.arrival_time - dep_time;
                    count[j] += 1;
                }
            }

            (sum_times, count)
        });

        // Reduce across samples
        let mut total_sum = vec![0u32; num_nodes];
        let mut total_count = vec![0u32; num_nodes];
        for (sum_times, count) in &per_sample {
            for j in 0..num_nodes {
                total_sum[j] += sum_times[j];
                total_count[j] += count[j];
            }
        }

        total_sum
            .iter()
            .zip(total_count.iter())
            .map(|(&s, &c)| if c > 0 { s as f64 / c as f64 } else { f64::NAN })
            .collect()
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
            let results = router::run_tdd_multi(
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
        let results = router::run_tdd_multi(
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
                departure_time,
            },
        }
    }

    pub fn node_arrival_time(&self, sssp: &WasmSsspResult, node: u32) -> u32 {
        sssp.inner.results[node as usize].arrival_time
    }

    pub fn node_leave_home(&self, _sssp: &WasmSsspResult, _node: u32) -> u32 {
        0 // leave_home is now transient and not retained after routing
    }

    pub fn node_boarding_time(&self, sssp: &WasmSsspResult, node: u32) -> u32 {
        sssp.inner.results[node as usize].boarding_time
    }

    pub fn sssp_departure_time(&self, sssp: &WasmSsspResult) -> u32 {
        sssp.inner.departure_time
    }

    pub fn reconstruct_path(&self, sssp: &WasmSsspResult, destination: u32) -> Vec<u32> {
        reconstruct_path(&self.data, &sssp.inner, destination)
    }

    /// Get the shape polyline for a route between two stops (identified by node indices).
    /// Returns flat array [lat, lon, lat, lon, ...] of the sub-polyline, or empty if no shape.
    /// Shapes are stored compressed and decompressed on-demand.
    pub fn route_shape_between(&self, route_idx: u32, from_node: u32, to_node: u32) -> Vec<f64> {
        let ri = route_idx as usize;
        if ri >= self.data.route_shapes.len() {
            return Vec::new();
        }
        let shape_indices = &self.data.route_shapes[ri];
        if shape_indices.is_empty() {
            return Vec::new();
        }

        let from_lat = self.data.nodes[from_node as usize].lat;
        let from_lon = self.data.nodes[from_node as usize].lon;
        let to_lat = self.data.nodes[to_node as usize].lat;
        let to_lon = self.data.nodes[to_node as usize].lon;

        // Try all shapes for this route; pick the one where both stops are closest
        let mut best_result: Vec<f64> = Vec::new();
        let mut best_worst_d = f64::MAX;

        for shape_idx in shape_indices {
            // Get compressed data from JaggedArray
            let shape_idx_usize = *shape_idx as usize;
            if shape_idx_usize >= self.data.shapes.offsets.len() - 1 {
                panic!(
                    "Shape {} referenced by route {} is out of bounds",
                    shape_idx, route_idx
                );
            }
            let start = self.data.shapes.offsets[shape_idx_usize] as usize;
            let end = self.data.shapes.offsets[shape_idx_usize + 1] as usize;
            let compressed = &self.data.shapes.data[start..end];

            // Decompress PCO data
            let coords_u32: Vec<u32> = match pco::standalone::simple_decompress(compressed) {
                Ok(c) => c,
                Err(e) => panic!("Failed to decompress shape {}: {}", shape_idx, e),
            };

            if coords_u32.len() < 4 {
                panic!("Shape {} has invalid compressed data: expected at least 4 u32s (2 points), got {}", shape_idx, coords_u32.len());
            }

            // Convert u32 bits back to f64 (via f32)
            let mut points: Vec<(f64, f64)> = Vec::with_capacity(coords_u32.len() / 2);
            for chunk in coords_u32.chunks(2) {
                if chunk.len() == 2 {
                    let lat_f32 = f32::from_bits(chunk[0]);
                    let lon_f32 = f32::from_bits(chunk[1]);
                    points.push((lat_f32 as f64, lon_f32 as f64));
                }
            }

            if points.len() < 2 {
                panic!(
                    "Shape {} decompressed to {} points, expected at least 2",
                    shape_idx,
                    points.len()
                );
            }

            let mut best_from = 0usize;
            let mut best_from_d = f64::MAX;
            let mut best_to = 0usize;
            let mut best_to_d = f64::MAX;
            for (i, &(lat, lon)) in points.iter().enumerate() {
                let df = (lat - from_lat).powi(2) + (lon - from_lon).powi(2);
                let dt = (lat - to_lat).powi(2) + (lon - to_lon).powi(2);
                if df < best_from_d {
                    best_from_d = df;
                    best_from = i;
                }
                if dt < best_to_d {
                    best_to_d = dt;
                    best_to = i;
                }
            }

            // Score: the worse of the two distances (both stops must be well-covered)
            let worst_d = best_from_d.max(best_to_d);
            if worst_d < best_worst_d {
                best_worst_d = worst_d;

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
                best_result = result;
            }
        }

        best_result
    }
}
