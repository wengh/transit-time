use crate::data::{PatternData, PreparedData};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

/// Info about which event chain a transit node was reached from.
#[derive(Clone, Copy)]
pub struct BoardingEvent {
    pub pattern_index: usize,
    pub event_index: u32,
}

/// Sentinel: node was reached by walking from source (no transit yet).
/// Boarding any route from this state requires no transfer slack.
const ARRIVED_BY_WALK: u32 = u32::MAX;
/// Sentinel: node was reached via a frequency-based route (no event chain).
/// Treated as always requiring transfer slack (can never "continue" a freq trip).
const ARRIVED_BY_FREQ: u32 = u32::MAX - 1;

const WALKING_SPEED_MPS: f32 = 1.4; // ~5 km/h
pub const DEFAULT_TRANSFER_SLACK: u32 = 60; // default minimum transfer time in seconds
pub const DEFAULT_MAX_TIME: u32 = 7200; // 2 hours

#[derive(Clone, Copy)]
pub struct NodeResult {
    /// Seconds elapsed since departure_time. u16::MAX = unreached.
    pub arrival_delta: u16,
    /// Seconds since departure_time when the transit vehicle left the boarding stop.
    /// u16::MAX = no boarding (walk edge or source). Used to compute wait times.
    pub boarding_delta: u16,
    /// u32::MAX = walk edge, otherwise the route index.
    /// edge_type (0=walk/1=transit) is derived from this at reconstruction time.
    pub route_index: u32,
    pub prev_node: u32,
}

impl NodeResult {
    pub const UNREACHED: NodeResult = NodeResult {
        arrival_delta: u16::MAX,
        boarding_delta: u16::MAX,
        prev_node: u32::MAX,
        route_index: u32::MAX,
    };

    /// Returns true if `self` is a strictly better path than `other`.
    /// Better = earlier arrival, or same arrival with later leave_home
    /// (= you can leave home later and still make it).
    /// leave_home is tracked in a parallel transient Vec, not stored in NodeResult.
    fn is_better_than(&self, self_lh: u32, other: &NodeResult, other_lh: u32) -> bool {
        self.arrival_delta < other.arrival_delta
            || (self.arrival_delta == other.arrival_delta && self_lh > other_lh)
    }
}

/// Snap lat/lon to nearest OSM node using spatial grid index.
/// Snap lat/lon to nearest OSM node using spatial grid index.
pub fn snap_to_node(data: &PreparedData, lat: f64, lon: f64) -> Option<u32> {
    const CELL_SIZE_LAT: f64 = 0.0045;
    const CELL_SIZE_LON: f64 = 0.006;

    let cell_lat = (lat / CELL_SIZE_LAT).floor() as i32;
    let cell_lon = (lon / CELL_SIZE_LON).floor() as i32;
    let cos_lat = lat.to_radians().cos();

    let mut best: Option<u32> = None;
    let mut best_dist = f64::MAX;

    // Search 3x3 neighborhood of cells
    for dlat in -1..=1 {
        for dlon in -1..=1 {
            if let Some(indices) = data.node_grid.get(&(cell_lat + dlat, cell_lon + dlon)) {
                for &i in indices {
                    let node = &data.nodes[i as usize];
                    let dlat_val = node.lat - lat;
                    let dlon_val = (node.lon - lon) * cos_lat;
                    let dist = dlat_val * dlat_val + dlon_val * dlon_val;
                    if dist < best_dist {
                        best_dist = dist;
                        best = Some(i);
                    }
                }
            }
        }
    }

    best
}

/// Convert a YYYYMMDD date to day of week (0=Mon..6=Sun).
fn date_to_day_of_week(date: u32) -> u8 {
    let y = (date / 10000) as i32;
    let m = ((date / 100) % 100) as i32;
    let d = (date % 100) as i32;
    // Tomohiko Sakamoto's algorithm (returns 0=Sun..6=Sat)
    let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let dow = (y + y / 4 - y / 100 + y / 400 + t[(m - 1) as usize] + d) % 7;
    // Convert from 0=Sun..6=Sat to 0=Mon..6=Sun
    ((dow + 6) % 7) as u8
}

/// Find pattern indices active on a given date (YYYYMMDD).
/// Checks day-of-week mask, start/end date range, and date exceptions.
pub fn patterns_for_date(data: &PreparedData, date: u32) -> Vec<usize> {
    let day_of_week = date_to_day_of_week(date);
    let bit = 1u8 << day_of_week;
    data.patterns
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            if p.stop_index.events_by_stop.is_empty() && p.frequency_routes.is_empty() {
                return false;
            }
            // Explicitly removed on this date
            if p.date_exceptions_remove.contains(&date) {
                return false;
            }
            // Explicitly added on this date
            if p.date_exceptions_add.contains(&date) {
                return true;
            }
            // Check day-of-week mask
            if p.day_mask & bit == 0 {
                return false;
            }
            // Check date range (0 means unbounded)
            if p.start_date != 0 && date < p.start_date {
                return false;
            }
            if p.end_date != 0 && date > p.end_date {
                return false;
            }
            true
        })
        .map(|(i, _)| i)
        .collect()
}

/// Run time-dependent Dijkstra scanning events from multiple patterns.
pub fn run_tdd_multi(
    data: &PreparedData,
    source_node: u32,
    departure_time: u32,
    pattern_indices: &[usize],
    transfer_slack: u32,
    max_time: u32,
) -> (Vec<NodeResult>, HashMap<u32, BoardingEvent>) {
    let patterns: Vec<(usize, &PatternData)> = pattern_indices
        .iter()
        .filter_map(|&i| data.patterns.get(i).map(|p| (i, p)))
        .collect();
    run_tdd_inner(
        data,
        source_node,
        departure_time,
        &patterns,
        transfer_slack,
        max_time,
    )
}

fn run_tdd_inner(
    data: &PreparedData,
    source_node: u32,
    departure_time: u32,
    patterns: &[(usize, &PatternData)],
    transfer_slack: u32,
    max_time: u32,
) -> (Vec<NodeResult>, HashMap<u32, BoardingEvent>) {
    let n = data.num_nodes;
    let mut result = vec![NodeResult::UNREACHED; n];
    result[source_node as usize] = NodeResult {
        arrival_delta: 0,
        boarding_delta: u16::MAX,
        prev_node: u32::MAX,
        route_index: u32::MAX,
    };

    // Tracks which flat event index (into events_by_stop.data) last put us at each node.
    // ARRIVED_BY_WALK = walked here from source (no prior transit, no transfer slack).
    // ARRIVED_BY_FREQ = arrived via a frequency route (always requires transfer slack).
    // Any other value = flat event index; only that specific event's continuation is free.
    let mut arrived_by_event = vec![ARRIVED_BY_WALK; n];

    // leave_home: latest departure time from origin that still makes the connection.
    // Kept as a transient parallel array — not stored in NodeResult to save memory.
    let mut leave_home = vec![0u32; n];

    let mut boarding_events: HashMap<u32, BoardingEvent> = HashMap::new();

    let mut pq: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    pq.push(Reverse((departure_time, source_node)));

    while let Some(Reverse((t_current, node))) = pq.pop() {
        if (t_current - departure_time) > result[node as usize].arrival_delta as u32 {
            continue;
        }

        if t_current - departure_time > max_time {
            continue;
        }

        let current_event = arrived_by_event[node as usize];
        let current_leave_home = leave_home[node as usize];

        // Walking edges — leave_home and current_event propagate unchanged.
        // Propagating current_event means: if you were mid-trip and walk to an
        // adjacent node, you can still continue the same trip from there.
        for &(neighbor, distance) in &data.adj[node] {
            let wt = (distance / WALKING_SPEED_MPS) as u32;
            let arrival = t_current + wt;
            let candidate = NodeResult {
                arrival_delta: (arrival - departure_time) as u16,
                boarding_delta: u16::MAX,
                prev_node: node,
                route_index: u32::MAX,
            };
            if candidate.is_better_than(
                current_leave_home,
                &result[neighbor as usize],
                leave_home[neighbor as usize],
            ) {
                result[neighbor as usize] = candidate;
                leave_home[neighbor as usize] = current_leave_home;
                arrived_by_event[neighbor as usize] = current_event;
                pq.push(Reverse((arrival, neighbor)));
            }
        }

        // Transit edges (only at stop nodes)
        if data.node_is_stop[node as usize] {
            for &stop_idx in data.node_stop_indices.get(node) {
                for &(pat_idx, pat) in patterns {
                    scan_pattern_at_stop(
                        data,
                        pat,
                        pat_idx,
                        stop_idx,
                        t_current,
                        current_event,
                        current_leave_home,
                        departure_time,
                        transfer_slack,
                        node,
                        &mut result,
                        &mut leave_home,
                        &mut arrived_by_event,
                        &mut boarding_events,
                        &mut pq,
                    );
                }
            }
        }
    }

    (result, boarding_events)
}

#[inline(never)]
fn scan_pattern_at_stop(
    data: &PreparedData,
    pat: &PatternData,
    pat_idx: usize,
    stop_idx: u32,
    t_current: u32,
    current_event: u32,
    current_leave_home: u32,
    departure_time: u32,
    transfer_slack: u32,
    node: u32,
    result: &mut Vec<NodeResult>,
    leave_home: &mut Vec<u32>,
    arrived_by_event: &mut Vec<u32>,
    boarding_events: &mut HashMap<u32, BoardingEvent>,
    pq: &mut BinaryHeap<Reverse<(u32, u32)>>,
) {
    // --- Frequency-based routes ---
    // Board a freq trip at this stop and ride through all subsequent stops in the
    // trip's chain (via next_freq_index), so through-passengers don't pay transfer
    // slack at every intermediate stop.
    let freq_indices = &pat.stop_index.freq_by_stop[stop_idx];
    for &fi in freq_indices {
        let freq = &pat.frequency_routes[fi as usize];
        if freq.travel_time == 0 {
            continue;
        }

        let is_transfer = current_event != ARRIVED_BY_WALK;
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

            let lh = if current_leave_home == 0 {
                let walk_to_stop = t_current - departure_time;
                boarding_time.saturating_sub(walk_to_stop)
            } else {
                current_leave_home
            };

            // Ride through all subsequent stops in this trip's chain.
            let mut next_fi = fi;
            let mut cumulative_travel = 0u32;
            loop {
                let leg = &pat.frequency_routes[next_fi as usize];
                cumulative_travel += leg.travel_time;
                let arrival = boarding_time + cumulative_travel;
                let arrival_delta = arrival.saturating_sub(departure_time);
                if arrival_delta > u16::MAX as u32 {
                    break;
                }
                let dest_node = data.stop_node_map[leg.next_stop_index as usize];
                if dest_node != u32::MAX {
                    let candidate = NodeResult {
                        arrival_delta: arrival_delta as u16,
                        boarding_delta: (boarding_time - departure_time) as u16,
                        prev_node: node,
                        route_index: freq.route_index,
                    };
                    if candidate.is_better_than(
                        lh,
                        &result[dest_node as usize],
                        leave_home[dest_node as usize],
                    ) {
                        result[dest_node as usize] = candidate;
                        leave_home[dest_node as usize] = lh;
                        arrived_by_event[dest_node as usize] = ARRIVED_BY_FREQ;
                        // Record which freq entry was the boarding point so
                        // reconstruct_path can expand intermediate stops.
                        boarding_events.insert(
                            dest_node,
                            BoardingEvent {
                                pattern_index: pat_idx,
                                event_index: fi | crate::FREQ_BOARDING_FLAG,
                            },
                        );
                        pq.push(Reverse((arrival, dest_node)));
                    }
                }
                if leg.next_freq_index == u32::MAX {
                    break;
                }
                next_fi = leg.next_freq_index;
            }
        }
    }

    // --- Scheduled events ---
    let stop_events = &pat.stop_index.events_by_stop[stop_idx];
    if stop_events.is_empty() || t_current < pat.min_time {
        return;
    }

    let scan_start = t_current - pat.min_time;
    let scan_end = scan_start + 3600;

    let start_pos = stop_events.partition_point(|e| e.time_offset < scan_start);

    // Base offset of this stop's bucket in the flat events_by_stop.data array,
    // used to compute global flat indices for same-trip continuation checks.
    let base_offset = pat.stop_index.events_by_stop.offsets[stop_idx as usize] as usize;

    for (local_i, event) in stop_events[start_pos..].iter().enumerate() {
        if event.time_offset >= scan_end {
            break;
        }
        if event.travel_time == 0 {
            continue;
        }

        let dep_time = pat.min_time + event.time_offset;

        // "Continuing" = this event is exactly the next step in the trip that
        // brought us to this node (stored as its flat index in arrived_by_event).
        // Any other event — including a later trip on the same route — is a transfer.
        let global_idx = (base_offset + start_pos + local_i) as u32;
        let is_continuing = current_event == global_idx;
        let is_transfer = current_event != ARRIVED_BY_WALK && !is_continuing;

        if is_transfer && dep_time < t_current + transfer_slack {
            continue;
        }

        // Extract route_index from sentinel event (which is reached by following next_event_index)
        let mut route_index = 0u32;
        let mut idx = event.next_event_index;
        while idx != u32::MAX {
            let e = &pat.stop_index.events_by_stop.data[idx as usize];
            if e.travel_time == 0 {
                // Found sentinel event — look up its route_index
                if let Some(&r) = pat.sentinel_routes.get(&idx) {
                    route_index = r;
                }
                break;
            }
            idx = e.next_event_index;
        }

        let lh = if current_leave_home == 0 {
            let walk_to_stop = t_current - departure_time;
            dep_time.saturating_sub(walk_to_stop)
        } else {
            current_leave_home
        };

        let boarding_info = BoardingEvent {
            pattern_index: pat_idx,
            event_index: global_idx,
        };

        ride_trip(
            data,
            pat,
            route_index,
            boarding_info,
            lh,
            node,
            event.next_event_index,
            dep_time + event.travel_time,
            dep_time,
            departure_time,
            result,
            leave_home,
            arrived_by_event,
            boarding_events,
            pq,
        );
    }
}

/// Follow a trip through all its downstream stops after boarding.
fn ride_trip(
    data: &PreparedData,
    pat: &PatternData,
    route_index: u32,
    boarding_info: BoardingEvent,
    trip_leave_home: u32,
    boarding_node: u32,
    mut next_event_idx: u32,
    mut current_arrival: u32,
    boarding_time: u32,
    departure_time: u32,
    result: &mut Vec<NodeResult>,
    leave_home: &mut Vec<u32>,
    arrived_by_event: &mut Vec<u32>,
    boarding_events: &mut HashMap<u32, BoardingEvent>,
    pq: &mut BinaryHeap<Reverse<(u32, u32)>>,
) {
    while next_event_idx != u32::MAX {
        let event = &pat.stop_index.events_by_stop.data[next_event_idx as usize];
        let dest_node = data.stop_node_map[event.stop_index as usize];
        if dest_node != u32::MAX {
            let candidate = NodeResult {
                arrival_delta: (current_arrival - departure_time) as u16,
                boarding_delta: (boarding_time - departure_time) as u16,
                // Always point back to boarding_node. Intermediate stops may be
                // overwritten by other paths later, so we can't use them as
                // stable predecessors. Path reconstruction shows the boarding
                // stop (from the previous walk segment) and the alighting stop.
                prev_node: boarding_node,
                route_index,
            };
            if candidate.is_better_than(
                trip_leave_home,
                &result[dest_node as usize],
                leave_home[dest_node as usize],
            ) {
                result[dest_node as usize] = candidate;
                leave_home[dest_node as usize] = trip_leave_home;
                arrived_by_event[dest_node as usize] = next_event_idx;
                boarding_events.insert(dest_node, boarding_info);
                pq.push(Reverse((current_arrival, dest_node)));
            }
        }

        if event.travel_time > 0 {
            current_arrival = pat.min_time + event.time_offset + event.travel_time;
            next_event_idx = event.next_event_index;
        } else {
            break;
        }
    }
}
