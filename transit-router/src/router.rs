use crate::data::{PreparedData, PatternData};
use std::collections::BinaryHeap;
use std::cmp::Reverse;

const WALKING_SPEED_MPS: f32 = 1.4; // ~5 km/h
pub const DEFAULT_TRANSFER_SLACK: u32 = 60; // default minimum transfer time in seconds
pub const DEFAULT_MAX_TIME: u32 = 7200; // 2 hours

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

/// Find pattern indices matching a given day of week (0=Mon..6=Sun).
pub fn patterns_for_day(data: &PreparedData, day_of_week: u8) -> Vec<usize> {
    let bit = 1u8 << day_of_week;
    data.patterns
        .iter()
        .enumerate()
        .filter(|(_, p)| p.day_mask & bit != 0 && !p.events.is_empty())
        .map(|(i, _)| i)
        .collect()
}

/// Run time-dependent Dijkstra from source_node at departure_time.
/// Uses a single pattern.
pub fn run_tdd(
    data: &PreparedData,
    source_node: u32,
    departure_time: u32,
    pattern_index: usize,
    transfer_slack: u32,
) -> Vec<[u32; 4]> {
    let patterns: Vec<&PatternData> = if pattern_index < data.patterns.len() {
        vec![&data.patterns[pattern_index]]
    } else {
        vec![]
    };
    run_tdd_inner(data, source_node, departure_time, &patterns, transfer_slack, DEFAULT_MAX_TIME)
}

/// Run time-dependent Dijkstra scanning events from multiple patterns.
pub fn run_tdd_multi(
    data: &PreparedData,
    source_node: u32,
    departure_time: u32,
    pattern_indices: &[usize],
    transfer_slack: u32,
    max_time: u32,
) -> Vec<[u32; 4]> {
    let patterns: Vec<&PatternData> = pattern_indices
        .iter()
        .filter_map(|&i| data.patterns.get(i))
        .collect();
    run_tdd_inner(data, source_node, departure_time, &patterns, transfer_slack, max_time)
}

fn run_tdd_inner(
    data: &PreparedData,
    source_node: u32,
    departure_time: u32,
    patterns: &[&PatternData],
    transfer_slack: u32,
    max_time: u32,
) -> Vec<[u32; 4]> {
    let n = data.num_nodes;
    let mut result = vec![[u32::MAX, u32::MAX, 0u32, u32::MAX]; n];
    result[source_node as usize] = [departure_time, u32::MAX, 0, u32::MAX];

    let mut arrived_by_route = vec![u32::MAX; n];

    let mut pq: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    pq.push(Reverse((departure_time, source_node)));

    while let Some(Reverse((t_current, node))) = pq.pop() {
        if t_current > result[node as usize][0] {
            continue;
        }

        if t_current - departure_time > max_time {
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
            for &stop_idx in &data.node_stop_indices[node as usize] {
                for pat in patterns {
                    scan_pattern_at_stop(
                        data, pat, stop_idx, t_current, current_route,
                        transfer_slack, node, &mut result, &mut arrived_by_route, &mut pq,
                    );
                }
            }
        }
    }

    result
}

fn scan_pattern_at_stop(
    data: &PreparedData,
    pat: &PatternData,
    stop_idx: u32,
    t_current: u32,
    current_route: u32,
    transfer_slack: u32,
    node: u32,
    result: &mut Vec<[u32; 4]>,
    arrived_by_route: &mut Vec<u32>,
    pq: &mut BinaryHeap<Reverse<(u32, u32)>>,
) {
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
        return;
    }

    let scan_start = (t_current - pat.min_time) as usize;
    let max_scan = 3600.min(pat.events.len().saturating_sub(scan_start));

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
                found_same.push(event.route_index);
                let arrival = dep_time + event.travel_time;
                let dest_node = data.stop_node_map[event.next_stop_index as usize];
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
                if is_transfer && dep_time < t_current + transfer_slack {
                    continue;
                }
                found_transfer.push(event.route_index);
                let arrival = dep_time + event.travel_time;
                let dest_node = data.stop_node_map[event.next_stop_index as usize];
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
