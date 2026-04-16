//! Profile routing: Pareto frontier of (arrival, home_departure) per node
//! over a departure-time window. One pass replaces N-sample Dijkstra.
//!
//! Per-entry storage is `edge_dep_delta` (time inbound edge started firing),
//! NOT `home_dep_delta`. The journey's home_dep is stored out-of-band in
//! `ProfileResult.home_dep_deltas` (parallel Vec, same shape as frontier) for
//! fast isochrone/fraction queries and the TS hd-keyed API. Reconstruction
//! uses `edge_dep_delta` plus the per-entry `flag_prev_origin_walk` bit to
//! pick predecessors unambiguously — three-branch rule in
//! `find_predecessor_entry`.

use crate::data::{PatternData, PreparedData};
use std::ops::Index;
use crate::router::patterns_for_date;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

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
    walk_only_pass(data, source_node, max_time, window_len, &mut frontier, &mut home_dep_deltas);

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
            let start_transit = if f.first().map(|e| e.is_walk_only()).unwrap_or(false) { 1 } else { 0 };
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
            let edge_dep_delta = vehicle_dep.saturating_sub(window_start).min(u16::MAX as u32) as u16;
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
            hop_edge_dep = current_arrival.saturating_sub(window_start).min(u16::MAX as u32) as u16;
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
    let mut hop_edge_dep = vehicle_dep.saturating_sub(window_start).min(u16::MAX as u32) as u16;
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
        let route_u32: u32 = if entry.is_walk_edge() { u32::MAX } else { entry.route_index as u32 };
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
        let start = if f.first().map(|e| e.is_walk_only()).unwrap_or(false) { 1 } else { 0 };
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
        let start = if f.first().map(|e| e.is_walk_only()).unwrap_or(false) { 1 } else { 0 };
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
    let start = if f.first().map(|e| e.is_walk_only()).unwrap_or(false) { 1 } else { 0 };
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

    fn result_with(frontier: Vec<Vec<ProfileEntry>>, hd: Vec<Vec<u16>>, slack: u32) -> ProfileResult {
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
            e(80, 60, 99, 1, true),  // transit entry from earlier hop
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
