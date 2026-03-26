use crate::data::PreparedData;
use std::collections::BinaryHeap;
use std::cmp::Reverse;

const WALKING_SPEED_MPS: f32 = 1.4; // ~5 km/h
const MAX_TRIP_SECONDS: u32 = 7200; // 2 hours
const MIN_TRANSFER_SECONDS: u32 = 60; // minimum time to transfer between routes

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
/// Returns Vec of [arrival_time, prev_node, prev_edge_type, prev_route_index] per node.
/// edge_type: 0 = walk, 1 = transit. u32::MAX = unreached.
pub fn run_tdd(
    data: &PreparedData,
    source_node: u32,
    departure_time: u32,
    pattern_index: usize,
) -> Vec<[u32; 4]> {
    let n = data.num_nodes;
    // result: [arrival_time, prev_node, prev_edge_type, prev_route_index]
    let mut result = vec![[u32::MAX, u32::MAX, 0u32, u32::MAX]; n];
    result[source_node as usize] = [departure_time, u32::MAX, 0, u32::MAX];

    // Track the route that brought us to each node (u32::MAX = walked or source)
    // Used for transfer penalty: if arriving by route X, boarding route Y costs extra wait
    let mut arrived_by_route = vec![u32::MAX; n];

    // Min-heap: (arrival_time, node_index)
    let mut pq: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    pq.push(Reverse((departure_time, source_node)));

    let pattern = if pattern_index < data.patterns.len() {
        Some(&data.patterns[pattern_index])
    } else {
        None
    };

    while let Some(Reverse((t_current, node))) = pq.pop() {
        // Skip if we already found a better path
        if t_current > result[node as usize][0] {
            continue;
        }

        // Cutoff
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
                arrived_by_route[neighbor as usize] = u32::MAX; // walked
                pq.push(Reverse((arrival, neighbor)));
            }
        }

        // Transit edges (only at stop nodes)
        if data.node_is_stop[node as usize] {
            if let Some(pat) = pattern {
                for &stop_idx in &data.node_stop_indices[node as usize] {
                    // Check frequency-based routes
                    for freq in &pat.frequency_routes {
                        if freq.stop_index == stop_idx && freq.travel_time > 0 {
                            // Apply transfer penalty if switching routes
                            let effective_time = if current_route != u32::MAX
                                && current_route != freq.route_index
                            {
                                t_current + MIN_TRANSFER_SECONDS
                            } else {
                                t_current
                            };

                            if effective_time >= freq.start_time && effective_time < freq.end_time {
                                let elapsed = effective_time - freq.start_time;
                                let wait = if elapsed % freq.headway_secs == 0 {
                                    0
                                } else {
                                    freq.headway_secs - (elapsed % freq.headway_secs)
                                };
                                let arrival = effective_time + wait + freq.travel_time;
                                let dest_node = data.stop_node_map[freq.next_stop_index as usize];
                                if arrival < result[dest_node as usize][0] {
                                    result[dest_node as usize] =
                                        [arrival, node, 1, freq.route_index];
                                    arrived_by_route[dest_node as usize] = freq.route_index;
                                    pq.push(Reverse((arrival, dest_node)));
                                }
                            }
                        }
                    }

                    // Check direct-index event array
                    if !pat.events.is_empty() && t_current >= pat.min_time {
                        // Apply transfer penalty for route search start time
                        let search_time = if current_route != u32::MAX {
                            t_current + MIN_TRANSFER_SECONDS
                        } else {
                            t_current
                        };

                        if search_time < pat.min_time {
                            continue;
                        }

                        let start_idx = (search_time - pat.min_time) as usize;
                        let max_scan =
                            3600.min(pat.events.len().saturating_sub(start_idx));

                        let mut found_routes: Vec<(u32, u32)> = Vec::new();

                        for offset in 0..max_scan {
                            let idx = start_idx + offset;
                            for event in &pat.events[idx] {
                                if event.stop_index == stop_idx && event.travel_time > 0 {
                                    let key = (event.stop_index, event.route_index);
                                    if found_routes.contains(&key) {
                                        continue;
                                    }
                                    found_routes.push(key);

                                    let dep_time = pat.min_time + idx as u32;

                                    // If continuing on same route, no transfer penalty needed
                                    // (penalty was already applied to search_time for other routes)
                                    // But if this IS the same route, we should have started
                                    // searching from t_current, not search_time
                                    let actual_dep = if current_route == event.route_index {
                                        // Same route: re-check from t_current
                                        if t_current >= pat.min_time {
                                            let real_start =
                                                (t_current - pat.min_time) as usize;
                                            // Find first event for this stop+route from real_start
                                            let mut found_dep = dep_time;
                                            for o2 in 0..(idx - real_start.min(idx)) {
                                                let idx2 = real_start + o2;
                                                if idx2 >= pat.events.len() {
                                                    break;
                                                }
                                                for ev2 in &pat.events[idx2] {
                                                    if ev2.stop_index == stop_idx
                                                        && ev2.route_index == event.route_index
                                                        && ev2.travel_time > 0
                                                    {
                                                        found_dep = pat.min_time + idx2 as u32;
                                                        break;
                                                    }
                                                }
                                                if found_dep < dep_time {
                                                    break;
                                                }
                                            }
                                            found_dep
                                        } else {
                                            dep_time
                                        }
                                    } else {
                                        dep_time
                                    };

                                    let arrival = actual_dep + event.travel_time;
                                    let dest_node =
                                        data.stop_node_map[event.next_stop_index as usize];
                                    if arrival < result[dest_node as usize][0] {
                                        result[dest_node as usize] =
                                            [arrival, node, 1, event.route_index];
                                        arrived_by_route[dest_node as usize] =
                                            event.route_index;
                                        pq.push(Reverse((arrival, dest_node)));
                                    }
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
