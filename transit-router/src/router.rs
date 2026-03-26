use crate::data::PreparedData;
use std::collections::BinaryHeap;
use std::cmp::Reverse;

const WALKING_SPEED_MPS: f32 = 1.4; // ~5 km/h
const MAX_TRIP_SECONDS: u32 = 7200; // 2 hours
pub const DEFAULT_TRANSFER_SLACK: u32 = 60; // default minimum transfer time in seconds

/// Snap lat/lon to nearest OSM node.
pub fn snap_to_node(data: &PreparedData, lat: f64, lon: f64) -> u32 {
    let mut best = 0u32;
    let mut best_dist = f64::MAX;
    for (i, node) in data.nodes.iter().enumerate() {
        let dlat = node.lat - lat;
        let dlon = node.lon - lon;
        let dist = dlat * dlat + dlon * dlon;
        if dist < best_dist {
            best_dist = dist;
            best = i as u32;
        }
    }
    best
}

/// Run time-dependent Dijkstra from source_node at departure_time.
/// `transfer_slack`: minimum seconds required when switching between different routes.
/// Returns Vec of [arrival_time, prev_node, prev_edge_type, prev_route_index] per node.
/// edge_type: 0 = walk, 1 = transit. u32::MAX = unreached.
pub fn run_tdd(
    data: &PreparedData,
    source_node: u32,
    departure_time: u32,
    pattern_index: usize,
    transfer_slack: u32,
) -> Vec<[u32; 4]> {
    let n = data.num_nodes;
    let mut result = vec![[u32::MAX, u32::MAX, 0u32, u32::MAX]; n];
    result[source_node as usize] = [departure_time, u32::MAX, 0, u32::MAX];

    // Track the route that brought us to each node (u32::MAX = walked or source)
    let mut arrived_by_route = vec![u32::MAX; n];

    let mut pq: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    pq.push(Reverse((departure_time, source_node)));

    let pattern = if pattern_index < data.patterns.len() {
        Some(&data.patterns[pattern_index])
    } else {
        None
    };

    while let Some(Reverse((t_current, node))) = pq.pop() {
        if t_current > result[node as usize][0] {
            continue;
        }

        if t_current - departure_time > MAX_TRIP_SECONDS {
            continue;
        }

        let current_route = arrived_by_route[node as usize];

        // Walking edges
        for &(neighbor, distance) in &data.adj[node as usize] {
            let walk_time = (distance / WALKING_SPEED_MPS) as u32;
            let arrival = t_current + walk_time;
            if arrival < result[neighbor as usize][0] {
                result[neighbor as usize] = [arrival, node, 0, u32::MAX];
                arrived_by_route[neighbor as usize] = u32::MAX;
                pq.push(Reverse((arrival, neighbor)));
            }
        }

        // Transit edges (only at stop nodes)
        if data.node_is_stop[node as usize] {
            if let Some(pat) = pattern {
                for &stop_idx in &data.node_stop_indices[node as usize] {
                    // Check frequency-based routes
                    for freq in &pat.frequency_routes {
                        if freq.stop_index != stop_idx || freq.travel_time == 0 {
                            continue;
                        }

                        let is_transfer = current_route != u32::MAX
                            && current_route != freq.route_index;
                        let earliest = if is_transfer {
                            t_current + transfer_slack
                        } else {
                            t_current
                        };

                        if earliest >= freq.start_time && earliest < freq.end_time {
                            let elapsed = earliest - freq.start_time;
                            let wait = if elapsed % freq.headway_secs == 0 {
                                0
                            } else {
                                freq.headway_secs - (elapsed % freq.headway_secs)
                            };
                            let arrival = earliest + wait + freq.travel_time;
                            let dest_node = data.stop_node_map[freq.next_stop_index as usize];
                            if arrival < result[dest_node as usize][0] {
                                result[dest_node as usize] =
                                    [arrival, node, 1, freq.route_index];
                                arrived_by_route[dest_node as usize] = freq.route_index;
                                pq.push(Reverse((arrival, dest_node)));
                            }
                        }
                    }

                    // Check direct-index event array
                    if pat.events.is_empty() || t_current < pat.min_time {
                        continue;
                    }

                    // For each route at this stop, find the next departure.
                    // If transferring (different route), add transfer_slack to earliest boarding time.
                    // If continuing same route, no slack needed.
                    // We scan once from t_current; for transfer routes we skip events before t_current + slack.
                    let scan_start = (t_current - pat.min_time) as usize;
                    let max_scan = 3600.min(pat.events.len().saturating_sub(scan_start));

                    // Track: for each route_index, have we found its next departure yet?
                    // Two sets: one for same-route (no slack), one for transfer routes (with slack)
                    let mut found_same: Vec<u32> = Vec::new();
                    let mut found_transfer: Vec<u32> = Vec::new();

                    for offset in 0..max_scan {
                        let idx = scan_start + offset;
                        let dep_time = pat.min_time + idx as u32;

                        for event in &pat.events[idx] {
                            if event.stop_index != stop_idx || event.travel_time == 0 {
                                continue;
                            }

                            let is_same_route = current_route == event.route_index;
                            let is_transfer = current_route != u32::MAX && !is_same_route;

                            if is_same_route {
                                if found_same.contains(&event.route_index) {
                                    continue;
                                }
                                // No slack needed for same route
                                found_same.push(event.route_index);
                                let arrival = dep_time + event.travel_time;
                                let dest_node =
                                    data.stop_node_map[event.next_stop_index as usize];
                                if arrival < result[dest_node as usize][0] {
                                    result[dest_node as usize] =
                                        [arrival, node, 1, event.route_index];
                                    arrived_by_route[dest_node as usize] = event.route_index;
                                    pq.push(Reverse((arrival, dest_node)));
                                }
                            } else {
                                if found_transfer.contains(&event.route_index) {
                                    continue;
                                }
                                // Transfer: must wait at least transfer_slack
                                if is_transfer && dep_time < t_current + transfer_slack {
                                    continue;
                                }
                                found_transfer.push(event.route_index);
                                let arrival = dep_time + event.travel_time;
                                let dest_node =
                                    data.stop_node_map[event.next_stop_index as usize];
                                if arrival < result[dest_node as usize][0] {
                                    result[dest_node as usize] =
                                        [arrival, node, 1, event.route_index];
                                    arrived_by_route[dest_node as usize] = event.route_index;
                                    pq.push(Reverse((arrival, dest_node)));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    result
}
