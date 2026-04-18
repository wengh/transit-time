//! Profile routing: Pareto frontier of (arrival, home_departure) per node
//! over a departure-time window. One pass replaces N-sample Dijkstra.
//!
//! # Public interface
//!
//! Two functions cross the boundary with the rest of the codebase:
//!
//! 1. [`ProfileRouting::compute`] — run routing from a source, get an opaque
//!    routing state containing the per-node [`Isochrone`] for map rendering.
//! 2. [`ProfileRouting::optimal_paths`] — given the state + a destination,
//!    get a `Vec<Path>` of all Pareto-optimal journeys with fully-resolved
//!    segment metadata (stop names, route names, times, waits, node
//!    sequences). No shapes — those come from [`crate::TransitRouter::segment_shape`].
//!
//! Everything below those signatures is implementation detail and may be
//! rewritten freely.

use crate::data::{PatternData, PreparedData};
use crate::router::patterns_for_date;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Index;

// ============================================================================
// Public interface: input query, isochrone, paths
// ============================================================================

/// Input to [`ProfileRouting::compute`].
#[derive(Clone, Copy, Debug)]
pub struct ProfileQuery {
    pub source_node: u32,
    /// Absolute seconds-of-day. Start of the departure-time window.
    pub window_start: u32,
    /// Absolute seconds-of-day. End of the departure-time window.
    pub window_end: u32,
    /// YYYYMMDD.
    pub date: u32,
    /// Seconds of transfer slack between transit legs.
    pub transfer_slack: u32,
    /// Isochrone budget in seconds. Nodes unreachable within `max_time` of
    /// departing home are reported as unreachable.
    pub max_time: u32,
}

/// Per-node isochrone summary for the map overlay.
#[derive(Debug, Clone)]
pub struct Isochrone {
    /// Length = `data.num_nodes`. `u32::MAX` = unreachable within `max_time`.
    /// `min_travel_time[v]` = min over all Pareto entries at `v` of
    /// `(arrival − home_departure)`. Walk-only entry contributes its walk time.
    pub min_travel_time: Vec<u32>,
    /// Length = `data.num_nodes`. In `[0.0, 1.0]`. Fraction of the query window
    /// during which `v` is reachable within `max_time`, computed as the
    /// normalised interval union over the per-node Pareto frontier.
    pub reachable_fraction: Vec<f32>,
    pub window_start: u32,
    pub window_end: u32,
}

/// One Pareto-optimal journey from source to a destination.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Path {
    /// Absolute seconds-of-day.
    pub home_departure: u32,
    /// Absolute seconds-of-day at destination.
    pub arrival_time: u32,
    /// `arrival_time − home_departure`.
    pub total_time: u32,
    pub segments: Vec<PathSegment>,
}

/// One edge of a [`Path`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathSegment {
    pub kind: SegmentKind,
    /// Absolute seconds-of-day. Transit: vehicle_dep at boarding. Walk: arrival
    /// at the first node.
    pub start_time: u32,
    /// Absolute seconds-of-day. Arrival at the last node.
    pub end_time: u32,
    /// Seconds between arriving at the boarding stop and vehicle_dep. `0` for
    /// walks.
    pub wait_time: u32,
    pub start_stop_name: String,
    pub end_stop_name: String,
    /// `None` for walks.
    pub route_index: Option<u16>,
    /// `None` for walks. Human label (e.g. "Blue Line").
    pub route_name: Option<String>,
    /// Node indices. Walk: `[start, end]` (len 2). Transit: `[boarding,
    /// intermediate…, final_alight]` (len ≥ 2). The first node is always the
    /// boarding/walk-start; the last the alight/walk-end.
    pub node_sequence: Vec<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SegmentKind {
    Walk,
    Transit,
}

/// Opaque routing state. Holds the Pareto frontier plus cached isochrone.
/// Internal representation is not part of the public interface.
pub struct ProfileRouting {
    isochrone: Isochrone,
    // Internal state the implementer needs for `optimal_paths`. Current backing
    // is a `ProfileResult`; swap freely.
    inner: ProfileResult,
}

impl ProfileRouting {
    /// Function 1: run profile routing from the query's source over the
    /// window. Returns the state + per-node isochrone.
    pub fn compute(data: &PreparedData, query: &ProfileQuery) -> Self {
        let inner = run_profile(
            data,
            query.source_node,
            query.window_start,
            query.window_end,
            query.date,
            query.transfer_slack,
            query.max_time,
        );
        let isochrone = build_isochrone(&inner, query.max_time);
        ProfileRouting { isochrone, inner }
    }

    pub fn isochrone(&self) -> &Isochrone {
        &self.isochrone
    }

    /// Function 2: enumerate all Pareto-optimal paths to `destination`, sorted
    /// ascending by `home_departure` (earliest-home-dep first). Route names,
    /// stop names, and dominant colors are looked up directly from `data` —
    /// no WASM router needed.
    pub fn optimal_paths(&self, data: &PreparedData, destination: u32) -> Vec<Path> {
        build_optimal_paths(data, &self.inner, destination)
    }
}

// ============================================================================
// Shape helper — pure function, testable without WASM
// ============================================================================

/// Chain per-leg GTFS shapes or fall back to straight lines from node coordinates.
///
/// - Walk segment: `route_index = None`, `nodes = [start, end]` (len 2).
///   Returns `[lat(start), lon(start), lat(end), lon(end)]`.
/// - Transit segment: `route_index = Some(r)`, `nodes = [board, stop_1, …, alight]`
///   (len ≥ 2). For each consecutive pair, looks up the GTFS leg shape; falls
///   back to a straight line when a per-leg shape is missing. Chains results,
///   dropping the duplicate endpoint between legs.
///
/// Returns flat `[lat0, lon0, lat1, lon1, …]`.
pub fn segment_shape(data: &PreparedData, route_index: Option<u16>, nodes: &[u32]) -> Vec<f32> {
    if nodes.len() < 2 {
        return Vec::new();
    }
    match route_index {
        None => {
            // Walk: straight line from node coordinates.
            let mut out = Vec::with_capacity(nodes.len() * 2);
            for &n in nodes {
                let n = n as usize;
                out.push(data.nodes[n].lat as f32);
                out.push(data.nodes[n].lon as f32);
            }
            out
        }
        Some(route_idx) => {
            // Transit: chain GTFS leg shapes per consecutive stop pair.
            let mut out: Vec<f32> = Vec::new();
            for pair in nodes.windows(2) {
                let leg = leg_shape_between(data, route_idx as u32, pair[0], pair[1]);
                let skip = if out.is_empty() { 0 } else { 2 };
                if leg.len() >= 4 {
                    let src = &leg[skip..];
                    out.extend(src.iter().map(|&f| f as f32));
                } else {
                    // Fallback: straight line between the two stop nodes.
                    if out.is_empty() {
                        out.push(data.nodes[pair[0] as usize].lat as f32);
                        out.push(data.nodes[pair[0] as usize].lon as f32);
                    }
                    out.push(data.nodes[pair[1] as usize].lat as f32);
                    out.push(data.nodes[pair[1] as usize].lon as f32);
                }
            }
            out
        }
    }
}

/// Look up a per-leg GTFS shape between two consecutive stop nodes on a route.
/// Returns flat `[lat0, lon0, lat1, lon1, …]` as f32, or empty if no shape is
/// indexed for this (route, from_node, to_node).
///
/// Shares decoding with [`crate::TransitRouter::route_shape_between`] so the
/// WASM-exposed version and this pure-Rust helper never drift.
fn leg_shape_between(
    data: &PreparedData,
    route_idx: u32,
    from_node: u32,
    to_node: u32,
) -> Vec<f32> {
    let from_stop = match data.node_stop_indices.get(from_node).first() {
        Some(&s) => s,
        None => return Vec::new(),
    };
    let to_stop = match data.node_stop_indices.get(to_node).first() {
        Some(&s) => s,
        None => return Vec::new(),
    };
    let key = (route_idx, from_stop, to_stop);
    let idx = match data.leg_shape_keys.binary_search(&key) {
        Ok(i) => i,
        Err(_) => return Vec::new(),
    };
    let start = data.leg_shapes.offsets[idx] as usize;
    let end = data.leg_shapes.offsets[idx + 1] as usize;
    let compressed = &data.leg_shapes.data[start..end];
    if compressed.is_empty() {
        return Vec::new();
    }
    let coords_u32: Vec<u32> = match pco::standalone::simple_decompress(compressed) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::with_capacity(coords_u32.len());
    for chunk in coords_u32.chunks(2) {
        if chunk.len() == 2 {
            out.push(f32::from_bits(chunk[0]));
            out.push(f32::from_bits(chunk[1]));
        }
    }
    out
}

// ============================================================================
// Isochrone + optimal paths (public-interface implementations)
// ============================================================================

/// Compute per-node [`Isochrone`] from a [`ProfileResult`].
fn build_isochrone(inner: &ProfileResult, max_time: u32) -> Isochrone {
    let n = inner.frontier.len();
    let window_len = inner.window_end.saturating_sub(inner.window_start);
    let mut min_travel_time = vec![u32::MAX; n];
    let mut reachable_fraction = vec![0.0f32; n];

    for v in 0..n {
        let f = &inner.frontier[v];
        if f.is_empty() {
            continue;
        }
        let hd = match inner.home_dep_deltas.get(v) {
            Some(h) => h.as_slice(),
            None => &[],
        };
        // Best travel time: min over entries of (arrival - home_dep). Walk-only
        // entry's travel_time = arrival_delta (journey-independent walk time).
        let mut best = u32::MAX;
        for (entry, &h) in f.iter().zip(hd.iter()) {
            let arr = entry.arrival_delta as u32;
            let t = if h == WALK_ONLY {
                arr
            } else {
                arr.saturating_sub(h as u32)
            };
            if t > max_time {
                continue;
            }
            if t < best {
                best = t;
            }
        }
        min_travel_time[v] = best;

        // Reachable fraction: union of intervals [arr - max_time, hd] over
        // transit entries; walk-only alone contributes full window if within budget.
        reachable_fraction[v] = interval_union_fraction(f, hd, window_len, max_time);
    }

    Isochrone {
        min_travel_time,
        reachable_fraction,
        window_start: inner.window_start,
        window_end: inner.window_end,
    }
}

fn interval_union_fraction(
    f: &[ProfileEntry],
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
                if hi > lo {
                    Some((lo, hi))
                } else {
                    None
                }
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

/// One node+entry pair collected during backward path reconstruction.
struct ForwardStep {
    node: u32,
    entry: ProfileEntry,
}

/// Walk backward from `(destination, entry_at_dest)` to the source, collecting
/// (node, entry) pairs. Handles the walk-only linked list and the three-branch
/// transit predecessor rule. Returns pairs in source→destination order.
fn trace_path_forward(
    inner: &ProfileResult,
    destination: u32,
    entry_at_dest: ProfileEntry,
) -> Vec<ForwardStep> {
    let mut rev = Vec::<ForwardStep>::new();
    let mut entry = entry_at_dest;
    let mut cur_node = destination;
    let mut steps = 0usize;

    loop {
        rev.push(ForwardStep {
            node: cur_node,
            entry,
        });
        if entry.prev_node == u32::MAX || steps >= 1_000_000 {
            break;
        }
        if entry.is_walk_only() {
            let prev = entry.prev_node;
            let f_prev = inner.frontier.get(prev as usize);
            let next = match f_prev.and_then(|f| f.first()).filter(|e| e.is_walk_only()) {
                Some(e) => *e,
                None => break,
            };
            cur_node = prev;
            entry = next;
        } else {
            let prev = entry.prev_node;
            let next = match find_predecessor_entry(inner, prev, &entry) {
                Some(e) => e,
                None => break,
            };
            cur_node = prev;
            entry = next;
        }
        steps += 1;
    }
    rev.reverse();
    rev
}

/// Compute absolute arrival time at a node for the specific journey. Walk-only
/// nodes are journey-adjusted by `home_dep_abs`; iter entries use the entry's
/// own arrival_delta.
fn arrival_abs(inner: &ProfileResult, step: &ForwardStep, home_dep_abs: u32) -> u32 {
    if step.entry.is_walk_only() {
        home_dep_abs + step.entry.arrival_delta as u32
    } else {
        inner.window_start + step.entry.arrival_delta as u32
    }
}

fn stop_name_for_node(data: &PreparedData, node: u32) -> String {
    if let Some(&stop_idx) = data.node_stop_indices.get(node).first() {
        if let Some(s) = data.stops.get(stop_idx as usize) {
            return s.name.clone();
        }
    }
    String::new()
}

/// Build `Vec<Path>` for every Pareto-optimal entry at `destination`.
fn build_optimal_paths(data: &PreparedData, inner: &ProfileResult, destination: u32) -> Vec<Path> {
    let f_dest = match inner.frontier.get(destination as usize) {
        Some(f) => f,
        None => return Vec::new(),
    };
    let hd_dest = match inner.home_dep_deltas.get(destination as usize) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let mut paths: Vec<Path> = Vec::with_capacity(f_dest.len());
    for (i, entry) in f_dest.iter().enumerate() {
        let hd = *hd_dest.get(i).unwrap_or(&WALK_ONLY);
        if let Some(p) = build_single_path(data, inner, destination, *entry, hd) {
            paths.push(p);
        }
    }
    paths.sort_by_key(|p| p.home_departure);
    paths
}

fn build_single_path(
    data: &PreparedData,
    inner: &ProfileResult,
    destination: u32,
    entry_at_dest: ProfileEntry,
    hd_at_dest: u16,
) -> Option<Path> {
    let steps = trace_path_forward(inner, destination, entry_at_dest);
    if steps.is_empty() {
        return None;
    }

    // Home departure: walk-only path anchors at window_start; transit at
    // window_start + hd_at_dest.
    let home_dep_abs = if entry_at_dest.is_walk_only() {
        inner.window_start
    } else {
        inner.window_start + hd_at_dest as u32
    };
    let arrival_time = arrival_abs(inner, steps.last().unwrap(), home_dep_abs);
    let total_time = arrival_time.saturating_sub(home_dep_abs);

    // Group consecutive steps by (edge_kind, route_index) into segments.
    let mut segments: Vec<PathSegment> = Vec::new();
    let mut group_start_idx: usize = 0;
    let n_steps = steps.len();
    while group_start_idx < n_steps {
        let s = &steps[group_start_idx];
        // The "edge into this node" is described by the entry. Source node (step 0)
        // has no incoming edge conceptually — represent it as a walk. Every step
        // has an edge type from its entry.
        let (kind, route_idx) = edge_kind_of(&s.entry);

        // Extend group while next step matches.
        let mut end_idx = group_start_idx;
        while end_idx + 1 < n_steps {
            let nxt = &steps[end_idx + 1];
            let (k2, r2) = edge_kind_of(&nxt.entry);
            if k2 == kind && r2 == route_idx {
                end_idx += 1;
            } else {
                break;
            }
        }

        // Build node_sequence for this segment.
        // For transit: prepend the previous segment's last node (boarding_stop).
        let mut node_sequence: Vec<u32> = Vec::new();
        if kind == SegmentKind::Transit {
            if let Some(prev_seg) = segments.last() {
                if let Some(&last) = prev_seg.node_sequence.last() {
                    node_sequence.push(last);
                }
            }
        }
        for step in &steps[group_start_idx..=end_idx] {
            node_sequence.push(step.node);
        }

        // For walks: include the starting node from the PRIOR step when group
        // doesn't start at step 0, so walk segments span the walked edge.
        if kind == SegmentKind::Walk && group_start_idx > 0 {
            // Only if the prior step is not already the start of node_sequence.
            let prior = steps[group_start_idx - 1].node;
            if node_sequence.first() != Some(&prior) {
                node_sequence.insert(0, prior);
            }
        }

        // Timing.
        let last_step = &steps[end_idx];
        let end_time = arrival_abs(inner, last_step, home_dep_abs);
        let first_node_in_seg = *node_sequence.first()?;
        let first_step_in_seg_arr = if kind == SegmentKind::Walk && group_start_idx > 0 {
            // first_node_in_seg is the PRIOR step's node
            arrival_abs(inner, &steps[group_start_idx - 1], home_dep_abs)
        } else if kind == SegmentKind::Transit {
            // first_node_in_seg is boarding_stop (from prior walk). Use that walk's arrival.
            if let Some(prev_seg) = segments.last() {
                prev_seg.end_time
            } else {
                inner.window_start + s.entry.edge_dep_delta as u32
            }
        } else {
            arrival_abs(inner, s, home_dep_abs)
        };

        let (start_time, wait_time) = if kind == SegmentKind::Transit {
            // vehicle_dep = edge_dep_delta of first entry in the group (window-relative)
            let vehicle_dep = inner.window_start + s.entry.edge_dep_delta as u32;
            let wait = vehicle_dep.saturating_sub(first_step_in_seg_arr);
            (vehicle_dep, wait)
        } else {
            (first_step_in_seg_arr, 0)
        };

        let (start_stop_name, end_stop_name) = (
            stop_name_for_node(data, first_node_in_seg),
            stop_name_for_node(data, last_step.node),
        );

        let route_name = route_idx.and_then(|ri| data.route_names.get(ri as usize).cloned());

        segments.push(PathSegment {
            kind,
            start_time,
            end_time,
            wait_time,
            start_stop_name,
            end_stop_name,
            route_index: route_idx,
            route_name,
            node_sequence,
        });

        group_start_idx = end_idx + 1;
    }

    Some(Path {
        home_departure: home_dep_abs,
        arrival_time,
        total_time,
        segments,
    })
}

fn edge_kind_of(entry: &ProfileEntry) -> (SegmentKind, Option<u16>) {
    // Source node (prev=MAX) is represented as a walk "edge" (no inbound edge).
    if entry.prev_node == u32::MAX {
        return (SegmentKind::Walk, None);
    }
    if entry.is_walk_edge() {
        (SegmentKind::Walk, None)
    } else {
        (SegmentKind::Transit, Some(entry.route_index))
    }
}

// ============================================================================
// Internal representation (implementation detail — rewritable)
// ============================================================================

/// Sentinel for `edge_dep_delta` on the origin-walk entry.
pub const WALK_ONLY: u16 = u16::MAX;
/// Sentinel for `route_index` on walk edges.
pub const WALK_ROUTE: u16 = u16::MAX;

const WALKING_SPEED_MPS: f32 = 1.4;

/// One Pareto-optimal entry at a node. 12 bytes.
#[derive(Clone, Copy, Debug)]
pub struct ProfileEntry {
    /// Absolute arrival time at this node minus window_start.
    pub arrival_delta: u16,
    /// Window-relative time the inbound edge started firing:
    ///   - transit edge: vehicle_dep at boarding stop
    ///   - walk edge:    arrival_delta at prev_node
    ///   - origin walk:  `WALK_ONLY` sentinel
    pub edge_dep_delta: u16,
    /// Predecessor node for path reconstruction. u32::MAX = source.
    pub prev_node: u32,
    /// Transit route index, or `WALK_ROUTE` for walk edges.
    pub route_index: u16,
    /// True iff this transit entry boarded directly from the origin walk-only
    /// entry at `prev_node` (no prior transit). Set only at seed_source_event.
    /// Meaningful only when `route_index != WALK_ROUTE`.
    pub flag_prev_origin_walk: bool,
    /// Reserved for future per-entry bool flags (e.g. is_loop_sentinel, etc).
    pub _pad: u8,
}

impl ProfileEntry {
    pub const UNSET: ProfileEntry = ProfileEntry {
        arrival_delta: u16::MAX,
        edge_dep_delta: u16::MAX,
        prev_node: u32::MAX,
        route_index: WALK_ROUTE,
        flag_prev_origin_walk: false,
        _pad: 0,
    };

    /// True iff this entry was created by the initial walk-only pass from source.
    #[inline]
    pub fn is_walk_only(&self) -> bool {
        self.edge_dep_delta == WALK_ONLY
    }

    /// True iff this entry's inbound edge is a walk (not transit).
    #[inline]
    pub fn is_walk_edge(&self) -> bool {
        self.route_index == WALK_ROUTE
    }
}

pub struct ProfileResult {
    /// frontier[v] = per-node Pareto frontier.
    ///  - If present, frontier[v][0] is the walk-only entry.
    ///  - Transit entries F[v][1..] (or F[v][0..] if no walk entry) are sorted
    ///    DESCENDING by arrival_delta (append-only under reverse-scan outer loop).
    pub frontier: Vec<Vec<ProfileEntry>>,
    /// Parallel to `frontier`: effective home departure (window-relative) of
    /// the journey each entry represents. `WALK_ONLY` sentinel for walk-only
    /// entries at index 0.
    ///
    /// Kept out-of-band (not on ProfileEntry) because it's not needed during hot
    /// relaxation — only for reachable-fraction computation and TS API hd-keyed
    /// lookups. 2 bytes per entry; cheaper than the alternatives (inline disjoint
    /// interval state would be ≥8 B/entry).
    pub home_dep_deltas: Vec<Vec<u16>>,
    pub window_start: u32,
    pub window_end: u32,
    pub transfer_slack: u32,
}

/// Run profile routing. `window_start` / `window_end` are absolute seconds-of-day.
pub fn run_profile(
    data: &PreparedData,
    source_node: u32,
    window_start: u32,
    window_end: u32,
    date: u32,
    transfer_slack: u32,
    max_time: u32,
) -> ProfileResult {
    let n = data.num_nodes;
    let window_len = window_end - window_start;
    debug_assert!(window_len <= u16::MAX as u32 - 1);

    let pattern_indices = patterns_for_date(data, date);
    let patterns: Vec<(usize, &PatternData)> = pattern_indices
        .iter()
        .filter_map(|&i| data.patterns.get(i).map(|p| (i, p)))
        .collect();

    let mut frontier: Vec<Vec<ProfileEntry>> = vec![Vec::new(); n];
    let mut home_dep_deltas: Vec<Vec<u16>> = vec![Vec::new(); n];

    // Phase 1: initial walk-only Dijkstra. Produces F[v][0] for walk-reachable v.
    walk_only_pass(
        data,
        source_node,
        max_time,
        window_len,
        &mut frontier,
        &mut home_dep_deltas,
    );

    // Phase 2: source event stream (sorted DESCENDING by T(s)).
    let source_events = build_source_events(data, &patterns, window_start, window_end, &frontier);

    // Phase 3: outer loop. Per-iter scratch; amortised via touched-list reset.
    let mut iter_arr: Vec<u32> = vec![u32::MAX; n];
    let mut iter_prev: Vec<u32> = vec![u32::MAX; n];
    let mut iter_route: Vec<u16> = vec![WALK_ROUTE; n];
    let mut iter_edge_dep: Vec<u16> = vec![0u16; n];
    // Per-iter "did the winning relaxation to this node come from walk-only?"
    // seed writes true; scan_pattern and walks write false; last writer wins.
    let mut iter_flag: Vec<bool> = vec![false; n];
    let mut iter_touched: Vec<u32> = Vec::new();

    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();

    for src in &source_events {
        for &v in &iter_touched {
            let idx = v as usize;
            iter_arr[idx] = u32::MAX;
            iter_prev[idx] = u32::MAX;
            iter_route[idx] = WALK_ROUTE;
            iter_edge_dep[idx] = 0;
            iter_flag[idx] = false;
        }
        iter_touched.clear();
        heap.clear();

        let home_dep_delta: u16 = (src.t_effective - window_start) as u16;

        seed_source_event(
            data,
            &patterns,
            src,
            window_start,
            home_dep_delta,
            max_time,
            &frontier,
            &mut iter_arr,
            &mut iter_prev,
            &mut iter_route,
            &mut iter_edge_dep,
            &mut iter_flag,
            &mut iter_touched,
            &mut heap,
        );

        // Phase A: drain heap.
        while let Some(Reverse((arr_abs, v))) = heap.pop() {
            if iter_arr[v as usize] < arr_abs {
                continue;
            }
            let elapsed = arr_abs.saturating_sub(window_start + home_dep_delta as u32);
            if elapsed > max_time {
                continue;
            }

            // Walk edges from v → neighbor. edge_dep = arr at v (window-relative).
            let walk_edge_dep = (arr_abs.saturating_sub(window_start)).min(u16::MAX as u32) as u16;
            for &(neighbor, distance) in data.adj.index(v) {
                let wt = (distance / WALKING_SPEED_MPS) as u32;
                let cand_arr = arr_abs + wt;
                relax_scratch(
                    cand_arr,
                    neighbor,
                    home_dep_delta,
                    v,
                    WALK_ROUTE,
                    walk_edge_dep,
                    false, // walks never set flag=true
                    window_start,
                    max_time,
                    &frontier,
                    &mut iter_arr,
                    &mut iter_prev,
                    &mut iter_route,
                    &mut iter_edge_dep,
                    &mut iter_flag,
                    &mut iter_touched,
                    &mut heap,
                );
            }

            // Transit boardings (from transit/walk iter-state → flag=false).
            if data.node_is_stop[v as usize] {
                for &stop_idx in data.node_stop_indices.get(v) {
                    for &(pat_idx, pat) in &patterns {
                        scan_pattern_profile(
                            data,
                            pat,
                            pat_idx,
                            stop_idx,
                            arr_abs,
                            v,
                            home_dep_delta,
                            transfer_slack,
                            window_start,
                            max_time,
                            &frontier,
                            &mut iter_arr,
                            &mut iter_prev,
                            &mut iter_route,
                            &mut iter_edge_dep,
                            &mut iter_flag,
                            &mut iter_touched,
                            &mut heap,
                        );
                    }
                }
            }
        }

        // Phase B: commit.
        for &v in &iter_touched {
            let idx = v as usize;
            let arr_abs = iter_arr[idx];
            if arr_abs == u32::MAX {
                continue;
            }
            let arr_delta = arr_abs.saturating_sub(window_start);
            if arr_delta > u16::MAX as u32 {
                continue;
            }
            let arr_delta = arr_delta as u16;

            let f = &frontier[idx];
            if let Some(walk) = f.first() {
                if walk.is_walk_only() && walk.arrival_delta <= arr_delta {
                    continue;
                }
            }
            let start_transit = if f.first().map(|e| e.is_walk_only()).unwrap_or(false) {
                1
            } else {
                0
            };
            if f.len() > start_transit {
                let tail = &f[f.len() - 1];
                if tail.arrival_delta <= arr_delta {
                    continue;
                }
                debug_assert!(
                    home_dep_delta < home_dep_deltas[idx][home_dep_deltas[idx].len() - 1],
                    "reverse-scan invariant: new home_dep must be strictly smaller than tail's"
                );
            }

            frontier[idx].push(ProfileEntry {
                arrival_delta: arr_delta,
                edge_dep_delta: iter_edge_dep[idx],
                prev_node: iter_prev[idx],
                route_index: iter_route[idx],
                flag_prev_origin_walk: iter_flag[idx],
                _pad: 0,
            });
            home_dep_deltas[idx].push(home_dep_delta);
        }
    }

    ProfileResult {
        frontier,
        home_dep_deltas,
        window_start,
        window_end,
        transfer_slack,
    }
}

// ============================================================================
// Phase 1: walk-only pass
// ============================================================================

fn walk_only_pass(
    data: &PreparedData,
    source_node: u32,
    max_time: u32,
    window_len: u32,
    frontier: &mut [Vec<ProfileEntry>],
    home_dep_deltas: &mut [Vec<u16>],
) {
    let n = data.num_nodes;
    let mut best: Vec<u32> = vec![u32::MAX; n];
    best[source_node as usize] = 0;

    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    heap.push(Reverse((0, source_node)));

    let mut prev: Vec<u32> = vec![u32::MAX; n];
    let budget = max_time.saturating_add(window_len);

    while let Some(Reverse((t, node))) = heap.pop() {
        if t > best[node as usize] {
            continue;
        }
        if t > budget {
            continue;
        }
        for &(neighbor, distance) in data.adj.index(node) {
            let wt = (distance / WALKING_SPEED_MPS) as u32;
            let nt = t + wt;
            if nt < best[neighbor as usize] && nt <= budget {
                best[neighbor as usize] = nt;
                prev[neighbor as usize] = node;
                heap.push(Reverse((nt, neighbor)));
            }
        }
    }

    for v in 0..n {
        let t = best[v];
        if t == u32::MAX {
            continue;
        }
        if t > u16::MAX as u32 {
            continue;
        }
        frontier[v].push(ProfileEntry {
            arrival_delta: t as u16,
            edge_dep_delta: WALK_ONLY,
            prev_node: prev[v],
            route_index: WALK_ROUTE,
            flag_prev_origin_walk: false,
            _pad: 0,
        });
        home_dep_deltas[v].push(WALK_ONLY);
    }
}

// ============================================================================
// Phase 2: source event stream
// ============================================================================

#[derive(Clone, Copy)]
struct SourceEvent {
    t_effective: u32,
    stop_node: u32,
    #[allow(dead_code)]
    stop_idx: u32,
    pattern_index: u32,
    kind: SourceKind,
}

#[derive(Clone, Copy)]
enum SourceKind {
    Scheduled {
        global_event_idx: u32,
        vehicle_dep: u32,
    },
    Frequency {
        freq_index: u32,
        vehicle_dep: u32,
    },
}

fn build_source_events(
    data: &PreparedData,
    patterns: &[(usize, &PatternData)],
    window_start: u32,
    window_end: u32,
    frontier: &[Vec<ProfileEntry>],
) -> Vec<SourceEvent> {
    let mut out: Vec<SourceEvent> = Vec::new();

    for (stop_idx, &stop_node) in data.stop_node_map.iter().enumerate() {
        if stop_node == u32::MAX {
            continue;
        }
        let walk_entry = frontier[stop_node as usize].first();
        let walk_time = match walk_entry {
            Some(e) if e.is_walk_only() => e.arrival_delta as u32,
            _ => continue,
        };

        let stop_idx = stop_idx as u32;

        for &(pat_idx, pat) in patterns {
            let evs = &pat.stop_index.events_by_stop[stop_idx];
            if !evs.is_empty() && window_start + walk_time >= pat.min_time {
                let scan_start = window_start + walk_time - pat.min_time;
                let scan_end = window_end + walk_time - pat.min_time;
                let start_pos = evs.partition_point(|e| e.time_offset < scan_start);
                let base_offset = pat.stop_index.events_by_stop.offsets[stop_idx as usize] as usize;
                for (local_i, e) in evs[start_pos..].iter().enumerate() {
                    if e.time_offset >= scan_end {
                        break;
                    }
                    if e.travel_time == 0 {
                        continue;
                    }
                    let vehicle_dep = pat.min_time + e.time_offset;
                    let t_effective = vehicle_dep - walk_time;
                    if t_effective < window_start || t_effective > window_end {
                        continue;
                    }
                    let global_event_idx = (base_offset + start_pos + local_i) as u32;
                    out.push(SourceEvent {
                        t_effective,
                        stop_node,
                        stop_idx,
                        pattern_index: pat_idx as u32,
                        kind: SourceKind::Scheduled {
                            global_event_idx,
                            vehicle_dep,
                        },
                    });
                }
            }

            for &fi in &pat.stop_index.freq_by_stop[stop_idx] {
                let freq = &pat.frequency_routes[fi as usize];
                if freq.travel_time == 0 || freq.headway_secs == 0 {
                    continue;
                }
                let earliest_board = (window_start + walk_time).max(freq.start_time);
                let latest_board = (window_end + walk_time).min(freq.end_time.saturating_sub(1));
                if earliest_board > latest_board {
                    continue;
                }
                let offset = earliest_board - freq.start_time;
                let aligned = if offset % freq.headway_secs == 0 {
                    earliest_board
                } else {
                    earliest_board + (freq.headway_secs - offset % freq.headway_secs)
                };
                let mut vehicle_dep = aligned;
                while vehicle_dep <= latest_board {
                    let t_effective = vehicle_dep - walk_time;
                    if t_effective >= window_start && t_effective <= window_end {
                        out.push(SourceEvent {
                            t_effective,
                            stop_node,
                            stop_idx,
                            pattern_index: pat_idx as u32,
                            kind: SourceKind::Frequency {
                                freq_index: fi,
                                vehicle_dep,
                            },
                        });
                    }
                    vehicle_dep += freq.headway_secs;
                }
            }
        }
    }

    out.sort_by(|a, b| b.t_effective.cmp(&a.t_effective));
    out
}

// ============================================================================
// Phase 3: relaxation helpers
// ============================================================================

#[inline]
#[allow(clippy::too_many_arguments)]
fn relax_scratch(
    arr_abs: u32,
    u: u32,
    home_dep_delta: u16,
    prev: u32,
    route: u16,
    edge_dep_delta: u16,
    flag_prev_origin_walk: bool,
    window_start: u32,
    max_time: u32,
    frontier: &[Vec<ProfileEntry>],
    iter_arr: &mut [u32],
    iter_prev: &mut [u32],
    iter_route: &mut [u16],
    iter_edge_dep: &mut [u16],
    iter_flag: &mut [bool],
    iter_touched: &mut Vec<u32>,
    heap: &mut BinaryHeap<Reverse<(u32, u32)>>,
) {
    let home_dep_abs = window_start + home_dep_delta as u32;
    if arr_abs < home_dep_abs {
        return;
    }
    if arr_abs - home_dep_abs > max_time {
        return;
    }
    let arr_delta_u32 = arr_abs.saturating_sub(window_start);
    if arr_delta_u32 > u16::MAX as u32 {
        return;
    }
    let arr_delta = arr_delta_u32 as u16;

    let f = &frontier[u as usize];
    if let Some(walk) = f.first() {
        if walk.is_walk_only() && walk.arrival_delta <= arr_delta {
            return;
        }
    }
    if let Some(tail) = f.last() {
        if !tail.is_walk_only() && tail.arrival_delta <= arr_delta {
            return;
        }
    }

    if iter_arr[u as usize] <= arr_abs {
        return;
    }
    if iter_arr[u as usize] == u32::MAX {
        iter_touched.push(u);
    }
    iter_arr[u as usize] = arr_abs;
    iter_prev[u as usize] = prev;
    iter_route[u as usize] = route;
    iter_edge_dep[u as usize] = edge_dep_delta;
    iter_flag[u as usize] = flag_prev_origin_walk;
    heap.push(Reverse((arr_abs, u)));
}

// ============================================================================
// Seed + scan
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn seed_source_event(
    data: &PreparedData,
    patterns: &[(usize, &PatternData)],
    src: &SourceEvent,
    window_start: u32,
    home_dep_delta: u16,
    max_time: u32,
    frontier: &[Vec<ProfileEntry>],
    iter_arr: &mut [u32],
    iter_prev: &mut [u32],
    iter_route: &mut [u16],
    iter_edge_dep: &mut [u16],
    iter_flag: &mut [bool],
    iter_touched: &mut Vec<u32>,
    heap: &mut BinaryHeap<Reverse<(u32, u32)>>,
) {
    let pat = patterns
        .iter()
        .find(|(pi, _)| *pi as u32 == src.pattern_index)
        .map(|(_, p)| *p);
    let pat = match pat {
        Some(p) => p,
        None => return,
    };
    let boarding_node = src.stop_node;
    match src.kind {
        SourceKind::Scheduled {
            global_event_idx,
            vehicle_dep,
        } => {
            let ev = &pat.stop_index.events_by_stop.data[global_event_idx as usize];
            let route_index = sentinel_route_for(pat, ev.next_event_index);
            let edge_dep_delta = vehicle_dep
                .saturating_sub(window_start)
                .min(u16::MAX as u32) as u16;
            ride_trip_profile(
                data,
                pat,
                route_index,
                edge_dep_delta,
                true, // seed: predecessor at boarding_node is walk-only
                home_dep_delta,
                boarding_node,
                ev.next_event_index,
                vehicle_dep + ev.travel_time,
                window_start,
                max_time,
                frontier,
                iter_arr,
                iter_prev,
                iter_route,
                iter_edge_dep,
                iter_flag,
                iter_touched,
                heap,
            );
        }
        SourceKind::Frequency {
            freq_index,
            vehicle_dep,
        } => {
            ride_freq_profile(
                data,
                pat,
                freq_index,
                vehicle_dep,
                true, // seed: walk-only predecessor
                home_dep_delta,
                boarding_node,
                window_start,
                max_time,
                frontier,
                iter_arr,
                iter_prev,
                iter_route,
                iter_edge_dep,
                iter_flag,
                iter_touched,
                heap,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_pattern_profile(
    data: &PreparedData,
    pat: &PatternData,
    _pat_idx: usize,
    stop_idx: u32,
    t_current: u32,
    node: u32,
    home_dep_delta: u16,
    transfer_slack: u32,
    window_start: u32,
    max_time: u32,
    frontier: &[Vec<ProfileEntry>],
    iter_arr: &mut [u32],
    iter_prev: &mut [u32],
    iter_route: &mut [u16],
    iter_edge_dep: &mut [u16],
    iter_flag: &mut [bool],
    iter_touched: &mut Vec<u32>,
    heap: &mut BinaryHeap<Reverse<(u32, u32)>>,
) {
    // Frequency boardings.
    for &fi in &pat.stop_index.freq_by_stop[stop_idx] {
        let freq = &pat.frequency_routes[fi as usize];
        if freq.travel_time == 0 {
            continue;
        }
        let earliest = t_current + transfer_slack;
        if earliest < freq.start_time || earliest >= freq.end_time {
            continue;
        }
        let elapsed = earliest - freq.start_time;
        let wait = if elapsed % freq.headway_secs == 0 {
            0
        } else {
            freq.headway_secs - (elapsed % freq.headway_secs)
        };
        let vehicle_dep = earliest + wait;
        ride_freq_profile(
            data,
            pat,
            fi,
            vehicle_dep,
            false, // re-boarding from transit iter state
            home_dep_delta,
            node,
            window_start,
            max_time,
            frontier,
            iter_arr,
            iter_prev,
            iter_route,
            iter_edge_dep,
            iter_flag,
            iter_touched,
            heap,
        );
    }

    // Scheduled events.
    let evs = &pat.stop_index.events_by_stop[stop_idx];
    if evs.is_empty() || t_current < pat.min_time {
        return;
    }
    let scan_start = (t_current + transfer_slack).saturating_sub(pat.min_time);
    let scan_end = scan_start + 3600;
    let start_pos = evs.partition_point(|e| e.time_offset < scan_start);
    for (_local_i, e) in evs[start_pos..].iter().enumerate() {
        if e.time_offset >= scan_end {
            break;
        }
        if e.travel_time == 0 {
            continue;
        }
        let dep_time = pat.min_time + e.time_offset;
        if dep_time < t_current + transfer_slack {
            continue;
        }
        let route_index = sentinel_route_for(pat, e.next_event_index);
        let edge_dep_delta = dep_time.saturating_sub(window_start).min(u16::MAX as u32) as u16;
        ride_trip_profile(
            data,
            pat,
            route_index,
            edge_dep_delta,
            false, // re-boarding from transit iter state
            home_dep_delta,
            node,
            e.next_event_index,
            dep_time + e.travel_time,
            window_start,
            max_time,
            frontier,
            iter_arr,
            iter_prev,
            iter_route,
            iter_edge_dep,
            iter_flag,
            iter_touched,
            heap,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn ride_trip_profile(
    data: &PreparedData,
    pat: &PatternData,
    route_index: u16,
    edge_dep_delta: u16,
    flag_prev_origin_walk: bool,
    home_dep_delta: u16,
    boarding_node: u32,
    mut next_event_idx: u32,
    mut current_arrival: u32,
    window_start: u32,
    max_time: u32,
    frontier: &[Vec<ProfileEntry>],
    iter_arr: &mut [u32],
    iter_prev: &mut [u32],
    iter_route: &mut [u16],
    iter_edge_dep: &mut [u16],
    iter_flag: &mut [bool],
    iter_touched: &mut Vec<u32>,
    heap: &mut BinaryHeap<Reverse<(u32, u32)>>,
) {
    // Chain prev_node through intermediate stops so reconstruction can produce
    // consecutive stop pairs (needed for per-leg shape lookup in the TS consumer).
    // First alight: prev=boarding_node, edge_dep=vehicle_dep, flag=caller's.
    // Subsequent alights: prev=previous_alight, edge_dep=arrival_at_prev, flag=false.
    let mut prev_on_trip = boarding_node;
    let mut hop_edge_dep = edge_dep_delta;
    let mut hop_flag = flag_prev_origin_walk;
    let mut is_first = true;

    while next_event_idx != u32::MAX {
        let event = &pat.stop_index.events_by_stop.data[next_event_idx as usize];
        let dest_node = data.stop_node_map[event.stop_index as usize];
        if dest_node != u32::MAX {
            relax_scratch(
                current_arrival,
                dest_node,
                home_dep_delta,
                prev_on_trip,
                route_index,
                hop_edge_dep,
                hop_flag,
                window_start,
                max_time,
                frontier,
                iter_arr,
                iter_prev,
                iter_route,
                iter_edge_dep,
                iter_flag,
                iter_touched,
                heap,
            );
            // After first alight, chain through intermediate stops.
            if is_first {
                is_first = false;
            }
            prev_on_trip = dest_node;
            // Edge dep for next hop = arrival at this stop (vehicle departed here).
            hop_edge_dep = current_arrival
                .saturating_sub(window_start)
                .min(u16::MAX as u32) as u16;
            hop_flag = false;
        }
        if event.travel_time > 0 {
            current_arrival = pat.min_time + event.time_offset + event.travel_time;
            next_event_idx = event.next_event_index;
        } else {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ride_freq_profile(
    data: &PreparedData,
    pat: &PatternData,
    fi: u32,
    vehicle_dep: u32,
    flag_prev_origin_walk: bool,
    home_dep_delta: u16,
    boarding_node: u32,
    window_start: u32,
    max_time: u32,
    frontier: &[Vec<ProfileEntry>],
    iter_arr: &mut [u32],
    iter_prev: &mut [u32],
    iter_route: &mut [u16],
    iter_edge_dep: &mut [u16],
    iter_flag: &mut [bool],
    iter_touched: &mut Vec<u32>,
    heap: &mut BinaryHeap<Reverse<(u32, u32)>>,
) {
    let mut next_fi = fi;
    let mut cumulative = 0u32;
    let route_index_u32 = pat.frequency_routes[fi as usize].route_index;
    let route_index = route_index_u32.min(u16::MAX as u32 - 1) as u16;
    let mut hop_edge_dep = vehicle_dep
        .saturating_sub(window_start)
        .min(u16::MAX as u32) as u16;
    let mut hop_flag = flag_prev_origin_walk;
    let mut prev_on_trip = boarding_node;
    loop {
        let leg = &pat.frequency_routes[next_fi as usize];
        cumulative += leg.travel_time;
        let arrival = vehicle_dep + cumulative;
        let dest_node = data.stop_node_map[leg.next_stop_index as usize];
        if dest_node != u32::MAX {
            relax_scratch(
                arrival,
                dest_node,
                home_dep_delta,
                prev_on_trip,
                route_index,
                hop_edge_dep,
                hop_flag,
                window_start,
                max_time,
                frontier,
                iter_arr,
                iter_prev,
                iter_route,
                iter_edge_dep,
                iter_flag,
                iter_touched,
                heap,
            );
            prev_on_trip = dest_node;
            hop_edge_dep = arrival.saturating_sub(window_start).min(u16::MAX as u32) as u16;
            hop_flag = false;
        }
        if leg.next_freq_index == u32::MAX {
            break;
        }
        next_fi = leg.next_freq_index;
    }
}

// ============================================================================
// Path reconstruction
// ============================================================================

/// Reconstruct path for frontier[destination][entry_index].
/// Returns flat [(node, edge_type, route_idx)] triples in source→dest order.
/// edge_type: 0=walk, 1=transit.
pub fn reconstruct_profile_path(
    result: &ProfileResult,
    destination: u32,
    entry_index: usize,
) -> Vec<u32> {
    let mut rev: Vec<(u32, u32, u32)> = Vec::new();
    let f_dest = match result.frontier.get(destination as usize) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let mut entry = match f_dest.get(entry_index) {
        Some(e) => *e,
        None => return Vec::new(),
    };
    let mut cur_node = destination;

    let mut steps = 0usize;
    while entry.prev_node != u32::MAX && steps < 1_000_000 {
        // Walk-only entries form their own linked list via prev_node from the
        // initial walk Dijkstra. Follow the chain directly — don't use
        // find_predecessor_entry (which can't match the WALK_ONLY sentinel).
        if entry.is_walk_only() {
            rev.push((cur_node, 0, u32::MAX));
            cur_node = entry.prev_node;
            let f_prev = result.frontier.get(cur_node as usize);
            entry = match f_prev.and_then(|f| f.first()).filter(|e| e.is_walk_only()) {
                Some(e) => *e,
                None => break,
            };
            steps += 1;
            continue;
        }

        let edge_type: u32 = if entry.is_walk_edge() { 0 } else { 1 };
        let route_u32: u32 = if entry.is_walk_edge() {
            u32::MAX
        } else {
            entry.route_index as u32
        };
        rev.push((cur_node, edge_type, route_u32));
        let prev = entry.prev_node;
        entry = match find_predecessor_entry(result, prev, &entry) {
            Some(e) => e,
            None => {
                rev.push((prev, 0, u32::MAX));
                cur_node = prev;
                break;
            }
        };
        cur_node = prev;
        steps += 1;
    }
    rev.push((cur_node, 0, u32::MAX));

    let mut out = Vec::with_capacity(rev.len() * 3);
    for (n, et, ri) in rev.into_iter().rev() {
        out.push(n);
        out.push(et);
        out.push(ri);
    }
    out
}

/// Given current entry at `v` and its prev_node `p`, find the predecessor entry
/// in F[p] that was used at insertion time.
///
/// Three-branch rule:
///   1. `current.is_walk_edge()` → exact arrival match: find F[p][i] where
///      arrival_delta(F[p][i]) == current.edge_dep_delta.
///   2. `current` is transit with `flag_prev_origin_walk=true` → predecessor
///      is F[p][0] (walk-only).
///   3. `current` is transit with `flag_prev_origin_walk=false` → predecessor
///      is transit entry in F[p][start_transit..] with largest arrival_delta ≤
///      edge_dep_delta − transfer_slack.
fn find_predecessor_entry(
    result: &ProfileResult,
    prev_node: u32,
    current: &ProfileEntry,
) -> Option<ProfileEntry> {
    let f = result.frontier.get(prev_node as usize)?;
    if f.is_empty() {
        return None;
    }

    // Branch 1: walk edge — exact arrival match.
    if current.is_walk_edge() {
        // F[p] sorted descending by arrival_delta (with walk-only at [0] if present,
        // whose arrival_delta fits the same descending order? NO — walk-only arrival
        // may be anywhere). Walk edges into this `current` were produced in main
        // loop from some popped node at time `edge_dep_delta`. That pop corresponds
        // to an entry at prev whose arrival_delta = edge_dep_delta.
        let target = current.edge_dep_delta;
        // The walk-only entry at F[p][0] might have arrival_delta == target too —
        // if so, that's the predecessor. Otherwise search transit entries.
        if let Some(first) = f.first() {
            if first.is_walk_only() && first.arrival_delta == target {
                return Some(*first);
            }
        }
        let start = if f.first().map(|e| e.is_walk_only()).unwrap_or(false) {
            1
        } else {
            0
        };
        let slice = &f[start..];
        // Transit entries sorted DESCENDING by arrival_delta. binary_search_by with
        // reverse-order comparator.
        if let Ok(i) = slice.binary_search_by(|e| target.cmp(&e.arrival_delta)) {
            return Some(slice[i]);
        }
        return None;
    }

    // Branch 2: transit, try exact arrival match first (handles ride-through on
    // same trip — intermediate stops have edge_dep_delta = arrival_delta(prev_stop),
    // so exact match finds the predecessor without needing slack logic).
    {
        let target = current.edge_dep_delta;
        if let Some(first) = f.first() {
            if first.is_walk_only() && first.arrival_delta == target {
                return Some(*first);
            }
        }
        let start = if f.first().map(|e| e.is_walk_only()).unwrap_or(false) {
            1
        } else {
            0
        };
        let slice = &f[start..];
        if let Ok(i) = slice.binary_search_by(|e| target.cmp(&e.arrival_delta)) {
            return Some(slice[i]);
        }
    }

    // Branch 3: transit, flag=true → predecessor is walk-only at prev.
    if current.flag_prev_origin_walk {
        let first = *f.first()?;
        if first.is_walk_only() {
            return Some(first);
        }
        return None;
    }

    // Branch 4: transit, flag=false → largest transit arr ≤ edge_dep - slack.
    let slack = result.transfer_slack as u32;
    let budget = (current.edge_dep_delta as u32).saturating_sub(slack);
    let start = if f.first().map(|e| e.is_walk_only()).unwrap_or(false) {
        1
    } else {
        0
    };
    let slice = &f[start..];
    let idx = slice.partition_point(|e| (e.arrival_delta as u32) > budget);
    if idx < slice.len() {
        return Some(slice[idx]);
    }
    None
}

fn sentinel_route_for(pat: &PatternData, mut idx: u32) -> u16 {
    while idx != u32::MAX {
        let e = &pat.stop_index.events_by_stop.data[idx as usize];
        if e.travel_time == 0 {
            let r = *pat.sentinel_routes.get(&idx).unwrap_or(&0);
            return r.min(u16::MAX as u32 - 1) as u16;
        }
        idx = e.next_event_index;
    }
    0
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn e(arr: u16, edge_dep: u16, prev: u32, route: u16, flag: bool) -> ProfileEntry {
        ProfileEntry {
            arrival_delta: arr,
            edge_dep_delta: edge_dep,
            prev_node: prev,
            route_index: route,
            flag_prev_origin_walk: flag,
            _pad: 0,
        }
    }

    fn walk_only(arr: u16) -> ProfileEntry {
        ProfileEntry {
            arrival_delta: arr,
            edge_dep_delta: WALK_ONLY,
            prev_node: u32::MAX,
            route_index: WALK_ROUTE,
            flag_prev_origin_walk: false,
            _pad: 0,
        }
    }

    fn result_with(
        frontier: Vec<Vec<ProfileEntry>>,
        hd: Vec<Vec<u16>>,
        slack: u32,
    ) -> ProfileResult {
        ProfileResult {
            frontier,
            home_dep_deltas: hd,
            window_start: 0,
            window_end: 3600,
            transfer_slack: slack,
        }
    }

    #[test]
    fn entry_layout_is_12_bytes() {
        assert_eq!(std::mem::size_of::<ProfileEntry>(), 12);
    }

    #[test]
    fn predecessor_walk_edge_exact_match() {
        // F[prev] = [walk-only(arr=20), transit(arr=95), transit(arr=80)]
        // current at v: walk edge with edge_dep_delta=80 ⇒ pred is transit arr=80.
        let f_prev = vec![
            walk_only(20),
            e(95, 60, 99, 1, false),
            e(80, 50, 99, 1, false),
        ];
        let res = result_with(vec![f_prev], vec![vec![WALK_ONLY, 40, 20]], 30);
        let current = e(100, 80, 0, WALK_ROUTE, false); // walk edge, edge_dep=80
        let pred = find_predecessor_entry(&res, 0, &current).unwrap();
        assert_eq!(pred.arrival_delta, 80);
        assert!(!pred.is_walk_only());
    }

    #[test]
    fn predecessor_walk_edge_matches_walk_only() {
        // F[prev] = [walk-only(arr=50), transit(arr=40)]
        // current at v: walk edge with edge_dep_delta=50 ⇒ pred is walk-only.
        let f_prev = vec![walk_only(50), e(40, 20, 99, 1, false)];
        let res = result_with(vec![f_prev], vec![vec![WALK_ONLY, 30]], 30);
        let current = e(80, 50, 0, WALK_ROUTE, false);
        let pred = find_predecessor_entry(&res, 0, &current).unwrap();
        assert!(pred.is_walk_only());
        assert_eq!(pred.arrival_delta, 50);
    }

    #[test]
    fn predecessor_transit_flag_true_uses_walk_only() {
        // F[prev] = [walk-only(arr=55), transit(arr=40)]
        // current at v: transit, flag=true ⇒ pred is walk-only (even though transit is feasible under slack).
        let f_prev = vec![walk_only(55), e(40, 20, 99, 1, false)];
        let res = result_with(vec![f_prev], vec![vec![WALK_ONLY, 30]], 10);
        let current = e(160, 60, 0, 7, true); // transit, edge_dep=60, flag=true
        let pred = find_predecessor_entry(&res, 0, &current).unwrap();
        assert!(pred.is_walk_only());
        assert_eq!(pred.arrival_delta, 55);
    }

    #[test]
    fn predecessor_transit_flag_false_upper_bound() {
        // F[prev] = [walk-only(arr=55), transit(arr=50), transit(arr=40), transit(arr=30)]
        // current at v: transit, flag=false, edge_dep=60, slack=10 ⇒ need arr ≤ 50.
        // Largest qualifying transit arr = 50.
        let f_prev = vec![
            walk_only(55),
            e(50, 30, 99, 1, false),
            e(40, 20, 99, 1, false),
            e(30, 10, 99, 1, false),
        ];
        let res = result_with(vec![f_prev], vec![vec![WALK_ONLY, 40, 30, 20]], 10);
        let current = e(160, 60, 0, 7, false);
        let pred = find_predecessor_entry(&res, 0, &current).unwrap();
        assert!(!pred.is_walk_only());
        assert_eq!(pred.arrival_delta, 50);
    }

    #[test]
    fn predecessor_transit_ride_through_exact_match() {
        // Ride-through: stop_2's edge_dep = arrival_delta(stop_1) = 80.
        // F[stop_1] has transit entry with arr=80. Exact match finds it.
        let f_prev = vec![
            walk_only(20),
            e(80, 60, 99, 1, true), // transit entry from earlier hop
        ];
        let res = result_with(vec![f_prev], vec![vec![WALK_ONLY, 50]], 30);
        let current = e(100, 80, 0, 1, false); // transit, edge_dep=80 = arr(prev)
        let pred = find_predecessor_entry(&res, 0, &current).unwrap();
        assert_eq!(pred.arrival_delta, 80);
        assert!(!pred.is_walk_only());
    }

    #[test]
    fn predecessor_transit_flag_false_skips_walk_only() {
        // Walk-only's arr is largest and WITHIN edge_dep, but slack excludes it and
        // flag=false says "skip walk-only". Expect the next-largest transit.
        let f_prev = vec![walk_only(55), e(45, 20, 99, 1, false)];
        let res = result_with(vec![f_prev], vec![vec![WALK_ONLY, 30]], 10);
        let current = e(160, 60, 0, 7, false); // transit, edge_dep=60
                                               // Walk-only arr=55 > budget=50, so it wouldn't qualify anyway.
                                               // Transit arr=45 ≤ 50 ✓. Expect arr=45.
        let pred = find_predecessor_entry(&res, 0, &current).unwrap();
        assert!(!pred.is_walk_only());
        assert_eq!(pred.arrival_delta, 45);
    }
}
