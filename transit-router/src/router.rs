use crate::data::PreparedData;
use std::collections::BinaryHeap;
use std::cmp::Reverse;

const WALKING_SPEED_MPS: f32 = 1.4; // ~5 km/h
const MAX_TRIP_SECONDS: u32 = 7200; // 2 hours

/// Snap lat/lon to nearest OSM node.
pub fn snap_to_node(data: &PreparedData, lat: f64, lon: f64) -> u32 {
    let mut best = 0u32;
    let mut best_dist = f64::MAX;
    for (i, node) in data.nodes.iter().enumerate() {
        let dlat = node.lat - lat;
        let dlon = node.lon - lon;
        let dist = dlat * dlat + dlon * dlon; // squared distance is fine for comparison
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
    let mut result = vec![[u32::MAX, u32::MAX, 0u32, u32::MAX]; n];
    result[source_node as usize] = [departure_time, u32::MAX, 0, u32::MAX];

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

        // Walking edges
        for &(neighbor, distance) in &data.adj[node as usize] {
            let walk_time = (distance / WALKING_SPEED_MPS) as u32;
            let arrival = t_current + walk_time;
            if arrival < result[neighbor as usize][0] {
                result[neighbor as usize] = [arrival, node, 0, u32::MAX];
                pq.push(Reverse((arrival, neighbor)));
            }
        }

        // Transit edges (only at stop nodes)
        if data.node_is_stop[node as usize] {
            if let Some(pat) = pattern {
                // For each stop at this node, check departures
                for &stop_idx in &data.node_stop_indices[node as usize] {
                    // Check frequency-based routes
                    for freq in &pat.frequency_routes {
                        if freq.stop_index == stop_idx {
                            if t_current >= freq.start_time && t_current < freq.end_time {
                                // Next departure: ceil to next headway
                                let elapsed = t_current - freq.start_time;
                                let wait = if elapsed % freq.headway_secs == 0 {
                                    0
                                } else {
                                    freq.headway_secs - (elapsed % freq.headway_secs)
                                };
                                let arrival = t_current + wait + freq.travel_time;
                                let dest_node = data.stop_node_map[freq.next_stop_index as usize];
                                if arrival < result[dest_node as usize][0] {
                                    result[dest_node as usize] =
                                        [arrival, node, 1, freq.route_index];
                                    pq.push(Reverse((arrival, dest_node)));
                                }
                            }
                        }
                    }

                    // Check direct-index event array
                    if !pat.events.is_empty() && t_current >= pat.min_time {
                        let start_idx = (t_current - pat.min_time) as usize;
                        let max_scan = 3600.min(pat.events.len().saturating_sub(start_idx)); // scan up to 1 hour

                        // Track which (stop, route) pairs we've already found departures for
                        // to avoid scanning further for those
                        let mut found_routes: Vec<(u32, u32)> = Vec::new();

                        for offset in 0..max_scan {
                            let idx = start_idx + offset;
                            for event in &pat.events[idx] {
                                if event.stop_index == stop_idx {
                                    // Check if we already found a departure for this route
                                    let key = (event.stop_index, event.route_index);
                                    if found_routes.contains(&key) {
                                        continue;
                                    }
                                    found_routes.push(key);

                                    let dep_time = pat.min_time + idx as u32;
                                    let arrival = dep_time + event.travel_time;
                                    let dest_node =
                                        data.stop_node_map[event.next_stop_index as usize];
                                    if arrival < result[dest_node as usize][0] {
                                        result[dest_node as usize] =
                                            [arrival, node, 1, event.route_index];
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
