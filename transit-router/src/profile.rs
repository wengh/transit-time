//! Profile routing: Pareto frontier of (arrival, home_departure) per node
//! over a departure-time window. One pass replaces N-sample Dijkstra.
//!
//! # Public interface
//!
//! [`ProfileRouter`] is the contract. The concrete type [`ProfileRouting`]
//! implements it. Callers hold `impl ProfileRouter` or the concrete type;
//! internal representation is free to change.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashSet},
};

use crate::data::PreparedData;
use serde::Serialize;

// ============================================================================
// Input / output types
// ============================================================================

/// Input to [`ProfileRouter::compute`].
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

// ============================================================================
// Trait
// ============================================================================

/// Contract for profile routing. Implement this to replace the routing engine
/// without touching callers in `lib.rs` or tests.
pub trait ProfileRouter: Sized {
    /// Run profile routing from `query.source_node` over the departure window.
    fn compute(data: &PreparedData, query: &ProfileQuery) -> Self;

    /// Per-node isochrone for map rendering.
    fn isochrone(&self) -> &Isochrone;

    /// All Pareto-optimal paths to `destination`, sorted ascending by
    /// `home_departure`. Stop and route names resolved from `data`.
    fn optimal_paths(&self, data: &PreparedData, destination: u32) -> Vec<Path>;
}

// ============================================================================
// Stub implementation (replace with real algorithm)
// ============================================================================

/// Opaque routing state. Internal representation is not part of the public
/// interface — swap freely as long as [`ProfileRouter`] is satisfied.
pub struct ProfileRouting {
    isochrone: Isochrone,
}

const ORIGIN_PREDECESSOR: u32 = u32::MAX;
const INITIAL_WALK: u16 = u16::MAX;

// A single entry for a node, representing a Pareto-optimal
// (home_departure, arrival) pair.
#[derive(Debug, Copy, Clone)]
struct Entry {
    // Predecessor node id
    // ORIGIN_PREDECESSOR if this is the source node
    prev: u32,
    // Departure time from the transit leg or the walk edge
    // (seconds since start of profile window)
    // INITIAL_WALK if all predecessors are walks
    departure_delta: u16,
    // Arrival time (seconds since start of profile window)
    // Total travel time if is initial walk
    arrival_delta: u16,
    // We can identify whether an entry is a walk edge or a transit leg by checking if an edge exists between `prev` and the node, and if the time difference matches the walk time. This allows us to avoid storing the kind of each entry explicitly, saving space.
    // Similarly, we can identify the transit route for a transit leg by looking up the boarding stop and departure time, so we don't need to store route indices in the entries.
}

#[derive(Debug, Default)]
struct NodeEntries {
    // Sorted by descending arrival time and descending departure time,
    // except the first entry which may be a walk-only entry
    entries: Vec<Entry>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
struct EntryRef {
    node_id: u32,
    entry_index: u16,
}

#[derive(Debug)]
struct Frontier {
    nodes: Vec<NodeEntries>,
    // Which transit entries have initial walk as the predecessor
    initial_transit: HashSet<EntryRef>,
}

#[derive(Debug, Copy, Clone)]
struct PendingEntry {
    node_id: u32,
    entry: Entry,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct QueueEntry {
    arrival_delta: u16,
    node_id: u32,
}

impl ProfileRouter for ProfileRouting {
    fn compute(data: &PreparedData, query: &ProfileQuery) -> Self {
        assert!(query.window_start < query.window_end);
        assert!(query.window_end - query.window_start + query.max_time < u32::MAX);

        let n = data.num_nodes;
        let active_patterns = crate::router::patterns_for_date(data, query.date);

        let mut frontier = Frontier {
            nodes: (0..n).map(|_| NodeEntries::default()).collect(),
            initial_transit: HashSet::new(),
        };

        // ── Phase 1: walk Dijkstra from source ───────────────────────────────
        // - Populate all walk-only entries (departure_delta = INITIAL_WALK),
        //   valid for any home departure across the whole window.
        // - Gather all transit legs available from walk-reachable stops:
        //   departure time in [window_start + walk_time, window_end + walk_time],
        //   i.e. home departure (= transit dep − walk_time) in [window_start, window_end].
        // - Sort the transit legs by home departure time.

        let mut initial_transit_entries: Vec<PendingEntry> = Vec::new();

        let mut queue: BinaryHeap<Reverse<QueueEntry>> = BinaryHeap::new();
        queue.push(Reverse(QueueEntry {
            arrival_delta: 0,
            node_id: query.source_node,
        }));
        frontier.nodes[query.source_node as usize]
            .entries
            .push(Entry {
                prev: ORIGIN_PREDECESSOR,
                departure_delta: INITIAL_WALK,
                arrival_delta: 0,
            });

        while let Some(Reverse(QueueEntry {
            arrival_delta,
            node_id,
        })) = queue.pop()
        {
            // Skip stale queue entry
            if arrival_delta > frontier.nodes[node_id as usize].entries[0].arrival_delta {
                continue;
            }

            initial_transit_entries.extend(expand_transit_legs(
                data,
                node_id,
                query.window_start + arrival_delta as u32,
                query.window_end + arrival_delta as u32,
                query,
                &active_patterns,
            ));

            for &(neighbor, distance) in &data.adj[node_id] {
                let new_arrival_delta = arrival_delta + get_walk_time(distance);

                if new_arrival_delta as u32 > query.max_time {
                    continue;
                }

                let new_entry = Entry {
                    prev: node_id,
                    departure_delta: INITIAL_WALK,
                    arrival_delta: new_arrival_delta,
                };

                let neighbor_entries = &mut frontier.nodes[neighbor as usize].entries;
                if let Some(best) = neighbor_entries.first_mut() {
                    if new_arrival_delta >= best.arrival_delta {
                        continue;
                    }
                    *best = new_entry;
                } else {
                    neighbor_entries.push(new_entry);
                }

                queue.push(Reverse(QueueEntry {
                    arrival_delta: new_arrival_delta,
                    node_id: neighbor,
                }));
            }
        }

        // Sort by home_departure (= transit_departure − walk_time) so the main
        // pass sweeps entries in chronological order.
        // e.entry.prev = boarding node (walk-settled); its arrival_delta = walk_time.
        // departure_delta − walk_time = transit_dep − window_start − walk_time = home_dep − window_start.
        initial_transit_entries.sort_by_key(|e| {
            e.entry.departure_delta - frontier.nodes[e.entry.prev as usize].entries[0].arrival_delta
        });

        // ── Phase 2: main profile routing pass ───────────────────────────────

        todo!();
    }

    fn isochrone(&self) -> &Isochrone {
        &self.isochrone
    }

    fn optimal_paths(&self, _data: &PreparedData, _destination: u32) -> Vec<Path> {
        todo!();
    }
}

fn get_walk_time(distance: f32) -> u16 {
    const WALKING_SPEED_MPS: f32 = 1.4;
    ((distance / WALKING_SPEED_MPS).round() as u16).max(1)
}

/// For each transit vehicle departing `node` within `[min_departure, max_departure]`,
/// follow the trip forward and emit one [`PendingEntry`] per downstream stop, with
/// `arrival_delta` filled in. `entry.prev` is set to `node` (the boarding stop) for
/// path reconstruction. Covers both scheduled and frequency-based routes.
///
/// Reusable for Phase 2 transfer expansion by varying `min_departure`/`max_departure`.
fn expand_transit_legs(
    data: &PreparedData,
    node: u32,
    min_departure: u32,
    max_departure: u32,
    query: &ProfileQuery,
    active_patterns: &[usize],
) -> Vec<PendingEntry> {
    if !data.node_is_stop[node as usize] {
        return Vec::new();
    }
    let max_arrival = query.window_end + query.max_time;
    let mut entries = Vec::new();

    for &stop_idx in data.node_stop_indices.get(node) {
        for &pat_idx in active_patterns {
            let pat = &data.patterns[pat_idx];

            // ── Scheduled ────────────────────────────────────────────────────
            let stop_events = &pat.stop_index.events_by_stop[stop_idx];
            let base = pat.stop_index.events_by_stop.offsets[stop_idx as usize] as usize;
            let start = stop_events.partition_point(|e| e.time_offset < min_departure);

            for (j, event) in stop_events[start..].iter().enumerate() {
                if event.time_offset > max_departure {
                    break;
                }
                if event.travel_time == 0 {
                    continue; // sentinel
                }
                let departure_delta = (event.time_offset - query.window_start) as u16;
                let mut flat_idx = base + start + j;

                loop {
                    let cur = &pat.stop_index.events_by_stop.data[flat_idx];
                    if cur.travel_time == 0 || cur.next_event_index == u32::MAX {
                        break;
                    }
                    let arrival = cur.time_offset + cur.travel_time;
                    if arrival > max_arrival {
                        break;
                    }
                    let next = &pat.stop_index.events_by_stop.data[cur.next_event_index as usize];
                    entries.push(PendingEntry {
                        node_id: data.stop_node_map[next.stop_index as usize],
                        entry: Entry {
                            prev: node,
                            departure_delta,
                            arrival_delta: (arrival - query.window_start) as u16,
                        },
                    });
                    flat_idx = cur.next_event_index as usize;
                }
            }

            // ── Frequency-based ───────────────────────────────────────────────
            for &fi in &pat.stop_index.freq_by_stop[stop_idx] {
                let freq = &pat.frequency_routes[fi as usize];
                if freq.travel_time == 0 {
                    continue;
                }
                if min_departure >= freq.end_time {
                    continue;
                }
                let effective_start = freq.start_time.max(min_departure);
                if effective_start > max_departure {
                    continue;
                }
                let elapsed_to_eff = effective_start.saturating_sub(freq.start_time);
                let wait = if elapsed_to_eff % freq.headway_secs == 0 {
                    0
                } else {
                    freq.headway_secs - (elapsed_to_eff % freq.headway_secs)
                };
                let first_board = effective_start + wait;
                if first_board > max_departure {
                    continue;
                }

                let mut board = first_board;
                while board <= max_departure && board < freq.end_time {
                    let departure_delta = (board - query.window_start) as u16;
                    let mut freq_idx = fi;
                    let mut elapsed = 0u32;

                    loop {
                        let f = &pat.frequency_routes[freq_idx as usize];
                        if f.travel_time == 0 {
                            break;
                        }
                        elapsed += f.travel_time;
                        let arrival = board + elapsed;
                        if arrival > max_arrival {
                            break;
                        }
                        entries.push(PendingEntry {
                            node_id: data.stop_node_map[f.next_stop_index as usize],
                            entry: Entry {
                                prev: node,
                                departure_delta,
                                arrival_delta: (arrival - query.window_start) as u16,
                            },
                        });
                        if f.next_freq_index == u32::MAX {
                            break;
                        }
                        freq_idx = f.next_freq_index;
                    }

                    board += freq.headway_secs;
                }
            }
        }
    }
    entries
}
