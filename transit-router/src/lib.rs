pub mod data;
pub mod router;

use wasm_bindgen::prelude::*;
use data::PreparedData;

#[wasm_bindgen]
pub struct TransitRouter {
    data: PreparedData,
}

#[wasm_bindgen]
pub struct SsspResult {
    /// For each node: (arrival_time, prev_node, prev_edge_type, prev_route_index)
    /// edge_type: 0 = walk, 1 = transit
    /// u32::MAX means unreached
    results: Vec<[u32; 4]>,
}

#[wasm_bindgen]
pub struct SampledResult {
    results: Vec<SsspResult>,
    avg_times: Vec<f64>,
}

#[wasm_bindgen]
pub struct PathSegment {
    pub node_index: u32,
    pub edge_type: u32, // 0 = walk, 1 = transit
    pub route_index: u32,
}

#[wasm_bindgen]
impl TransitRouter {
    /// Load prepared data from compressed binary bytes.
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

    pub fn num_patterns(&self) -> u32 {
        self.data.patterns.len() as u32
    }

    pub fn pattern_day_mask(&self, idx: u32) -> u8 {
        self.data.patterns[idx as usize].day_mask
    }

    pub fn find_pattern_for_day(&self, day_of_week: u32) -> i32 {
        let bit = 1u8 << day_of_week;
        for (i, p) in self.data.patterns.iter().enumerate() {
            if p.day_mask & bit != 0 {
                return i as i32;
            }
        }
        -1
    }

    pub fn snap_to_node(&self, lat: f64, lon: f64) -> u32 {
        router::snap_to_node(&self.data, lat, lon)
    }

    /// Run single-departure TDD.
    /// `transfer_slack`: minimum seconds when switching routes (default 60).
    /// Returns travel times as Float64Array (NaN for unreached).
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
        result
            .iter()
            .map(|r| {
                if r[0] == u32::MAX {
                    f64::NAN
                } else {
                    (r[0] - departure_time) as f64
                }
            })
            .collect()
    }

    /// Run TDD and return full SSSP result for path reconstruction.
    pub fn run_tdd_full(
        &self,
        source_node: u32,
        departure_time: u32,
        pattern_index: u32,
        transfer_slack: u32,
    ) -> SsspResult {
        let results = router::run_tdd(
            &self.data, source_node, departure_time,
            pattern_index as usize, transfer_slack,
        );
        SsspResult { results }
    }

    /// Run sampled TDD over a time window. Returns average travel times as Float64Array.
    pub fn run_tdd_sampled(
        &self,
        source_node: u32,
        window_start: u32,
        window_end: u32,
        n_samples: u32,
        pattern_index: u32,
        transfer_slack: u32,
    ) -> Vec<f64> {
        let num_nodes = self.data.num_nodes;
        let mut sum_times = vec![0.0f64; num_nodes];
        let mut count = vec![0u32; num_nodes];

        let step = if n_samples > 1 {
            (window_end - window_start) / (n_samples - 1)
        } else {
            0
        };

        for i in 0..n_samples {
            let dep_time = window_start + i * step;
            let result = router::run_tdd(
                &self.data, source_node, dep_time,
                pattern_index as usize, transfer_slack,
            );
            for (j, r) in result.iter().enumerate() {
                if r[0] != u32::MAX {
                    sum_times[j] += (r[0] - dep_time) as f64;
                    count[j] += 1;
                }
            }
        }

        sum_times
            .iter()
            .zip(count.iter())
            .map(|(&s, &c)| {
                if c > 0 {
                    s / c as f64
                } else {
                    f64::NAN
                }
            })
            .collect()
    }

    /// Run sampled TDD and return all SSSP results for path display.
    pub fn run_tdd_sampled_full(
        &self,
        source_node: u32,
        window_start: u32,
        window_end: u32,
        n_samples: u32,
        pattern_index: u32,
        transfer_slack: u32,
    ) -> SampledResult {
        let num_nodes = self.data.num_nodes;
        let mut all_results = Vec::with_capacity(n_samples as usize);
        let mut sum_times = vec![0.0f64; num_nodes];
        let mut count = vec![0u32; num_nodes];

        let step = if n_samples > 1 {
            (window_end - window_start) / (n_samples - 1)
        } else {
            0
        };

        for i in 0..n_samples {
            let dep_time = window_start + i * step;
            let results = router::run_tdd(
                &self.data, source_node, dep_time,
                pattern_index as usize, transfer_slack,
            );
            for (j, r) in results.iter().enumerate() {
                if r[0] != u32::MAX {
                    sum_times[j] += (r[0] - dep_time) as f64;
                    count[j] += 1;
                }
            }
            all_results.push(SsspResult { results });
        }

        let avg_times: Vec<f64> = sum_times
            .iter()
            .zip(count.iter())
            .map(|(&s, &c)| if c > 0 { s / c as f64 } else { f64::NAN })
            .collect();

        SampledResult {
            results: all_results,
            avg_times,
        }
    }

    /// Reconstruct path from source to destination.
    /// Returns flat array: [node_index, edge_type, route_index, ...]
    pub fn reconstruct_path(&self, sssp: &SsspResult, destination: u32) -> Vec<u32> {
        let mut path = Vec::new();
        let mut current = destination;

        loop {
            let r = &sssp.results[current as usize];
            if r[0] == u32::MAX {
                return Vec::new();
            }
            let prev = r[1];
            let edge_type = r[2];
            let route_idx = r[3];
            path.push([current, edge_type, route_idx]);

            if prev == u32::MAX || prev == current {
                break;
            }
            current = prev;
        }

        path.reverse();
        path.into_iter().flat_map(|[n, e, r]| [n, e, r]).collect()
    }
}
