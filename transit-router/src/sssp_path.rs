//! Build a [`Path`] from a single-departure SSSP result.
//!
//! Mirror of the shape emitted by [`crate::profile::ProfileRouting::optimal_paths`]
//! so both routing modes share the same output contract. Callers (CLI, WASM,
//! tests) consume the same `Path` struct regardless of which solver produced it.

use crate::data::PreparedData;
use crate::profile::{Path, PathSegment, SegmentKind};
use crate::{FREQ_BOARDING_FLAG, SsspResult};

/// Reconstruct the unique optimal journey from source → `destination`.
/// Returns `None` when the destination was not reached within the SSSP budget.
pub fn optimal_path(data: &PreparedData, sssp: &SsspResult, destination: u32) -> Option<Path> {
    if sssp.results[destination as usize].arrival_delta == u16::MAX {
        return None;
    }

    // Walk `prev_node` chain back to the source, collecting the coarse path.
    // The coarse path hops one transit leg (boarding → alighting) per step, so
    // intermediate stops have to be expanded separately from `boarding_events`.
    let mut coarse: Vec<u32> = Vec::new();
    let mut cur = destination;
    loop {
        coarse.push(cur);
        let r = &sssp.results[cur as usize];
        if r.prev_node == u32::MAX || r.prev_node == cur {
            break;
        }
        cur = r.prev_node;
    }
    coarse.reverse();
    if coarse.len() < 2 {
        // Source == destination; no segments to emit.
        return Some(Path {
            home_departure: sssp.departure_time,
            arrival_time: sssp.departure_time,
            total_time: 0,
            segments: Vec::new(),
        });
    }

    let home_departure = sssp.departure_time;
    let arrival_time = home_departure + sssp.results[destination as usize].arrival_delta as u32;
    let total_time = arrival_time.saturating_sub(home_departure);

    let mut segments: Vec<PathSegment> = Vec::new();
    let mut i = 0;
    while i + 1 < coarse.len() {
        let from = coarse[i];
        let to = coarse[i + 1];
        let r_to = &sssp.results[to as usize];
        let is_transit = r_to.route_index != u32::MAX;

        if !is_transit {
            // Coalesce consecutive walk hops into a single segment.
            let mut nodes = vec![from, to];
            let mut j = i + 1;
            while j + 1 < coarse.len() {
                let nxt = coarse[j + 1];
                if sssp.results[nxt as usize].route_index == u32::MAX {
                    nodes.push(nxt);
                    j += 1;
                } else {
                    break;
                }
            }
            let last = *nodes.last().unwrap();
            let start_time = home_departure + sssp.results[from as usize].arrival_delta as u32;
            let end_time = home_departure + sssp.results[last as usize].arrival_delta as u32;
            segments.push(PathSegment {
                kind: SegmentKind::Walk,
                start_time,
                end_time,
                wait_time: 0,
                start_stop_name: stop_name_for_node(data, from),
                end_stop_name: stop_name_for_node(data, last),
                route_index: None,
                route_name: None,
                node_sequence: nodes,
            });
            i = j + 1;
        } else {
            // Single transit leg: from=boarding stop, to=alighting stop. Intermediate
            // stops come from the boarding event chain (scheduled or frequency).
            let route_idx = r_to.route_index;
            let boarding_delta = r_to.boarding_delta;
            let arr_at_board_abs =
                home_departure + sssp.results[from as usize].arrival_delta as u32;
            let vehicle_dep = if boarding_delta != u16::MAX {
                home_departure + boarding_delta as u32
            } else {
                arr_at_board_abs
            };
            let end_time = home_departure + r_to.arrival_delta as u32;
            let wait_time = vehicle_dep.saturating_sub(arr_at_board_abs);

            let mut nodes = vec![from];
            if let Some(be) = sssp.boarding_events.get(&to) {
                let pat = &data.patterns[be.pattern_index];
                if be.event_index & FREQ_BOARDING_FLAG != 0 {
                    let fi = be.event_index & !FREQ_BOARDING_FLAG;
                    let mut next_fi = fi;
                    loop {
                        let leg = &pat.frequency_routes[next_fi as usize];
                        let stop_node = data.stop_to_node[leg.next_stop_index as usize];
                        if stop_node == to {
                            break;
                        }
                        if stop_node != u32::MAX {
                            nodes.push(stop_node);
                        }
                        if leg.next_freq_index == u32::MAX {
                            break;
                        }
                        next_fi = leg.next_freq_index;
                    }
                } else {
                    let boarding_event =
                        &pat.stop_index.events_by_stop.data[be.event_index as usize];
                    let mut idx = boarding_event.next_event_index;
                    while idx != u32::MAX {
                        let e = &pat.stop_index.events_by_stop.data[idx as usize];
                        let stop_node = data.stop_to_node[e.stop_index as usize];
                        if stop_node == to {
                            break;
                        }
                        if stop_node != u32::MAX && e.travel_time > 0 {
                            nodes.push(stop_node);
                        }
                        idx = e.next_event_index;
                    }
                }
            }
            nodes.push(to);

            let route_name = data.route_names.get(route_idx as usize).cloned();
            segments.push(PathSegment {
                kind: SegmentKind::Transit,
                start_time: vehicle_dep,
                end_time,
                wait_time,
                start_stop_name: stop_name_for_node(data, from),
                end_stop_name: stop_name_for_node(data, to),
                route_index: Some(route_idx),
                route_name,
                node_sequence: nodes,
            });
            i += 1;
        }
    }

    Some(Path {
        home_departure,
        arrival_time,
        total_time,
        segments,
    })
}

fn stop_name_for_node(data: &PreparedData, node: u32) -> String {
    if let Some(&stop_idx) = data.node_to_stop.get(&node) {
        if let Some(s) = data.stops.get(stop_idx as usize) {
            return s.name.clone();
        }
    }
    String::new()
}
