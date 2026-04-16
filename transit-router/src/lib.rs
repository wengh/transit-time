pub mod data;
pub mod profile;
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
            // Expand intermediate stops from boarding event chain.
            if let Some(be) = sssp.boarding_events.get(&node) {
                let pat = &data.patterns[be.pattern_index];
                if be.event_index & FREQ_BOARDING_FLAG != 0 {
                    // Frequency-based route: follow the FreqData chain from the boarding
                    // entry until we reach the alighting node, emitting intermediate stops.
                    let fi = be.event_index & !FREQ_BOARDING_FLAG;
                    let mut next_fi = fi;
                    loop {
                        let leg = &pat.frequency_routes[next_fi as usize];
                        let stop_node = data.stop_node_map[leg.next_stop_index as usize];
                        if stop_node == node {
                            break; // reached alighting stop
                        }
                        if stop_node != u32::MAX {
                            result.extend_from_slice(&[stop_node, 1, r.route_index]);
                        }
                        if leg.next_freq_index == u32::MAX {
                            break;
                        }
                        next_fi = leg.next_freq_index;
                    }
                } else {
                    // Scheduled route: follow the EventData chain from after the boarding
                    // event until we reach the alighting node.
                    let boarding_event =
                        &pat.stop_index.events_by_stop.data[be.event_index as usize];
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
pub struct WasmProfileResult {
    inner: profile::ProfileResult,
}

#[wasm_bindgen]
impl WasmProfileResult {
    pub fn window_start(&self) -> u32 {
        self.inner.window_start
    }
    pub fn window_end(&self) -> u32 {
        self.inner.window_end
    }
    /// Per-node frontier length (includes walk-only entry at index 0 when present).
    pub fn frontier_len(&self, node: u32) -> u32 {
        self.inner
            .frontier
            .get(node as usize)
            .map_or(0, |v| v.len() as u32)
    }
    /// Returns the i-th frontier entry as [arr_delta, home_dep_delta, prev_node, route_index].
    /// home_dep_delta comes from the parallel `home_dep_deltas` array (not the entry struct).
    /// home_dep_delta == u16::MAX (= WALK_ONLY) signals a walk-only entry.
    /// route_index: walk edges are returned as u32::MAX for backward compat with TS consumer.
    pub fn frontier_entry(&self, node: u32, i: u32) -> Vec<u32> {
        let f = match self.inner.frontier.get(node as usize) {
            Some(v) => v,
            None => return Vec::new(),
        };
        let e = match f.get(i as usize) {
            Some(e) => e,
            None => return Vec::new(),
        };
        let hd = self
            .inner
            .home_dep_deltas
            .get(node as usize)
            .and_then(|v| v.get(i as usize))
            .copied()
            .unwrap_or(u16::MAX);
        let route_u32 = if e.is_walk_edge() { u32::MAX } else { e.route_index as u32 };
        vec![e.arrival_delta as u32, hd as u32, e.prev_node, route_u32]
    }
    /// Smallest arrival_delta across all frontier entries at `node`, or u32::MAX if unreached.
    /// Use this as the "best reachable time" for isochrone rendering at a single color.
    pub fn node_best_arrival_delta(&self, node: u32) -> u32 {
        let f = match self.inner.frontier.get(node as usize) {
            Some(v) => v,
            None => return u32::MAX,
        };
        f.iter().map(|e| e.arrival_delta as u32).min().unwrap_or(u32::MAX)
    }
    /// Fraction of the window [window_start, window_end] during which `node` is
    /// reachable within `max_time` from the origin. Returns a value in [0, 1].
    /// Walk-only reachability (walk_time ≤ max_time) gives 1.0 trivially.
    pub fn node_reachable_fraction(&self, node: u32, max_time: u32) -> f32 {
        let f = match self.inner.frontier.get(node as usize) {
            Some(v) => v,
            None => return 0.0,
        };
        let hd = match self.inner.home_dep_deltas.get(node as usize) {
            Some(v) => v.as_slice(),
            None => return 0.0,
        };
        profile_reachable_fraction(f, hd, self.inner.window_end - self.inner.window_start, max_time)
    }
    /// Total frontier entries across all nodes (diagnostic).
    pub fn total_entries(&self) -> u32 {
        self.inner
            .frontier
            .iter()
            .map(|f| f.len() as u32)
            .sum()
    }

    /// Reconstruct the path for frontier[destination][entry_index], as a flat
    /// u32 array `[node, edge_type, route_idx]` repeating, in boarding→alighting order.
    /// edge_type: 0 = walk, 1 = transit.
    pub fn reconstruct_profile_path(&self, destination: u32, entry_index: u32) -> Vec<u32> {
        profile::reconstruct_profile_path(&self.inner, destination, entry_index as usize)
    }

    /// Absolute arrival time at `node` along the specific journey identified by
    /// `home_dep_delta`, or u32::MAX if no such entry exists. Looks up hd in the
    /// parallel `home_dep_deltas` array (not on the entry struct).
    pub fn node_arrival_for_home_dep(&self, node: u32, home_dep_delta: u32) -> u32 {
        let f = match self.inner.frontier.get(node as usize) {
            Some(v) => v,
            None => return u32::MAX,
        };
        let hd_vec = match self.inner.home_dep_deltas.get(node as usize) {
            Some(v) => v,
            None => return u32::MAX,
        };
        let hd = home_dep_delta as u16;
        if hd == WALK_ONLY {
            if let Some(e) = f.first() {
                if e.is_walk_only() {
                    return self.inner.window_start + e.arrival_delta as u32;
                }
            }
            return u32::MAX;
        }
        // Transit entries sorted DESCENDING by home_dep_delta in hd_vec[walk_offset..].
        let start = if f.first().map(|e| e.is_walk_only()).unwrap_or(false) { 1 } else { 0 };
        let slice = &hd_vec[start..];
        match slice.binary_search_by(|x| hd.cmp(x)) {
            Ok(i) => self.inner.window_start + f[start + i].arrival_delta as u32,
            Err(_) => {
                // Walk-only nodes (source, walk intermediates) have no transit entry
                // with matching hd. Compute the journey-specific walk arrival:
                // leave_home + walk_time = (window_start + hd) + walk_only.arrival_delta.
                if start == 1 {
                    self.inner.window_start + hd as u32 + f[0].arrival_delta as u32
                } else {
                    u32::MAX
                }
            }
        }
    }
}

const WALK_ONLY: u16 = u16::MAX;

fn profile_reachable_fraction(
    f: &[profile::ProfileEntry],
    hd_vec: &[u16],
    window_len: u32,
    max_time: u32,
) -> f32 {
    if f.is_empty() || window_len == 0 {
        return 0.0;
    }
    let has_walk = f[0].is_walk_only();
    if has_walk && (f[0].arrival_delta as u32) <= max_time {
        return 1.0;
    }
    // Transit entries: each defines an interval of T ∈ [arr_i - max_time, hd_i]
    // where that entry is reachable within budget. Union them.
    let start = if has_walk { 1 } else { 0 };
    let mut intervals: Vec<(u32, u32)> = f[start..]
        .iter()
        .zip(hd_vec[start..].iter())
        .filter_map(|(e, hd_u16)| {
            let arr = e.arrival_delta as u32;
            let hd = *hd_u16 as u32;
            if arr < max_time + hd {
                let lo = arr.saturating_sub(max_time).min(window_len);
                let hi = hd.min(window_len);
                if hi > lo { Some((lo, hi)) } else { None }
            } else {
                None
            }
        })
        .collect();
    if intervals.is_empty() {
        return 0.0;
    }
    intervals.sort();
    let mut total = 0u32;
    let mut cur_end = 0u32;
    for (lo, hi) in intervals {
        let lo = lo.max(cur_end);
        if hi > lo {
            total += hi - lo;
            cur_end = hi;
        }
    }
    total as f32 / window_len as f32
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

    /// Run profile routing over [window_start, window_end].
    /// One Dijkstra-style sweep produces the exact Pareto frontier per node
    /// of (arrival, home_departure) pairs — no sampling.
    pub fn run_profile_for_date(
        &self,
        source_node: u32,
        window_start: u32,
        window_end: u32,
        date: u32,
        transfer_slack: u32,
        max_time: u32,
    ) -> WasmProfileResult {
        let inner = profile::run_profile(
            &self.data,
            source_node,
            window_start,
            window_end,
            date,
            transfer_slack,
            max_time,
        );
        WasmProfileResult { inner }
    }

    /// Vehicle departure time from the boarding stop for the transit leg that
    /// alights at `alight_node` along the journey with `home_dep_delta`. Returns 0
    /// for walk-only/walk-edge entries or if the journey doesn't exist.
    ///
    /// Reads directly from `edge_dep_delta` on the frontier entry — same value for
    /// scheduled and frequency boardings (no more sidecar lookups or FREQ_FLAG hack).
    pub fn profile_boarding_time(
        &self,
        profile: &WasmProfileResult,
        alight_node: u32,
        home_dep_delta: u32,
    ) -> u32 {
        let f = match profile.inner.frontier.get(alight_node as usize) {
            Some(v) => v,
            None => return 0,
        };
        let hd_vec = match profile.inner.home_dep_deltas.get(alight_node as usize) {
            Some(v) => v,
            None => return 0,
        };
        let hd = home_dep_delta as u16;
        // Skip walk-only entry at index 0; find transit entry whose hd matches.
        let start = if f.first().map(|e| e.is_walk_only()).unwrap_or(false) { 1 } else { 0 };
        let slice = &hd_vec[start..];
        let i = match slice.binary_search_by(|x| hd.cmp(x)) {
            Ok(i) => start + i,
            Err(_) => return 0,
        };
        let e = &f[i];
        if e.is_walk_edge() {
            return 0;
        }
        profile.inner.window_start + e.edge_dep_delta as u32
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
