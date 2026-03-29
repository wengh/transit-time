use crate::data::{PreparedData, PatternData};
use std::collections::BinaryHeap;
use std::cmp::Reverse;

const WALKING_SPEED_MPS: f32 = 1.4; // ~5 km/h
pub const DEFAULT_TRANSFER_SLACK: u32 = 60; // default minimum transfer time in seconds
pub const DEFAULT_MAX_TIME: u32 = 7200; // 2 hours

#[derive(Clone, Copy)]
pub struct NodeResult {
    pub arrival_time: u32,
    pub prev_node: u32,
    pub edge_type: u32,   // 0 = walk, 1 = transit
    pub route_index: u32, // u32::MAX if walk
    /// Latest time you could leave home and still reach this node at arrival_time.
    /// Computed as first_transit_departure - walk_to_first_stop when boarding transit.
    /// 0 means no transit taken yet (still walking from source).
    pub leave_home: u32,
}

impl NodeResult {
    pub const UNREACHED: NodeResult = NodeResult {
        arrival_time: u32::MAX,
        prev_node: u32::MAX,
        edge_type: 0,
        route_index: u32::MAX,
        leave_home: 0,
    };

    /// Returns true if `self` is a strictly better path than `other`.
    /// Better = earlier arrival, or same arrival with later leave_home
    /// (= you can leave home later and still make it).
    fn is_better_than(&self, other: &NodeResult) -> bool {
        self.arrival_time < other.arrival_time
            || (self.arrival_time == other.arrival_time
                && self.leave_home > other.leave_home)
    }
}

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
) -> Vec<NodeResult> {
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
) -> Vec<NodeResult> {
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
) -> Vec<NodeResult> {
    let n = data.num_nodes;
    let mut result = vec![NodeResult::UNREACHED; n];
    result[source_node as usize] = NodeResult {
        arrival_time: departure_time, prev_node: u32::MAX, edge_type: 0, route_index: u32::MAX,
        leave_home: 0,
    };

    let mut arrived_by_route = vec![u32::MAX; n];

    let mut pq: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    pq.push(Reverse((departure_time, source_node)));

    while let Some(Reverse((t_current, node))) = pq.pop() {
        if t_current > result[node as usize].arrival_time {
            continue;
        }

        if t_current - departure_time > max_time {
            continue;
        }

        let current_route = arrived_by_route[node as usize];
        let current_leave_home = result[node as usize].leave_home;

        // Walking edges — leave_home propagates unchanged
        for &(neighbor, distance) in &data.adj[node as usize] {
            let wt = (distance / WALKING_SPEED_MPS) as u32;
            let arrival = t_current + wt;
            let candidate = NodeResult {
                arrival_time: arrival, prev_node: node, edge_type: 0, route_index: u32::MAX,
                leave_home: current_leave_home,
            };
            if candidate.is_better_than(&result[neighbor as usize]) {
                result[neighbor as usize] = candidate;
                arrived_by_route[neighbor as usize] = u32::MAX;
                pq.push(Reverse((arrival, neighbor)));
            }
        }

        // Transit edges (only at stop nodes)
        if data.node_is_stop[node as usize] {
            for &stop_idx in &data.node_stop_indices[node as usize] {
                for pat in patterns {
                    scan_pattern_at_stop(
                        data, pat, stop_idx, t_current, current_route, current_leave_home,
                        departure_time, transfer_slack, node, &mut result, &mut arrived_by_route, &mut pq,
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
    current_leave_home: u32,
    departure_time: u32,
    transfer_slack: u32,
    node: u32,
    result: &mut Vec<NodeResult>,
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
            let boarding_time = earliest + wait;
            let arrival = boarding_time + freq.travel_time;
            let dest_node = data.stop_node_map[freq.next_stop_index as usize];
            // If first transit leg, compute leave_home = boarding - walk_to_stop
            let leave_home = if current_leave_home == 0 {
                let walk_to_stop = t_current - departure_time;
                boarding_time.saturating_sub(walk_to_stop)
            } else {
                current_leave_home
            };
            let candidate = NodeResult {
                arrival_time: arrival, prev_node: node, edge_type: 1,
                route_index: freq.route_index, leave_home,
            };
            if candidate.is_better_than(&result[dest_node as usize]) {
                result[dest_node as usize] = candidate;
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
                let leave_home = if current_leave_home == 0 {
                    let walk_to_stop = t_current - departure_time;
                    dep_time.saturating_sub(walk_to_stop)
                } else {
                    current_leave_home
                };
                let candidate = NodeResult {
                    arrival_time: arrival, prev_node: node, edge_type: 1,
                    route_index: event.route_index, leave_home,
                };
                if candidate.is_better_than(&result[dest_node as usize]) {
                    result[dest_node as usize] = candidate;
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
                let leave_home = if current_leave_home == 0 {
                    let walk_to_stop = t_current - departure_time;
                    dep_time.saturating_sub(walk_to_stop)
                } else {
                    current_leave_home
                };
                let candidate = NodeResult {
                    arrival_time: arrival, prev_node: node, edge_type: 1,
                    route_index: event.route_index, leave_home,
                };
                if candidate.is_better_than(&result[dest_node as usize]) {
                    result[dest_node as usize] = candidate;
                    arrived_by_route[dest_node as usize] = event.route_index;
                    pq.push(Reverse((arrival, dest_node)));
                }
            }
        }
    }
}
