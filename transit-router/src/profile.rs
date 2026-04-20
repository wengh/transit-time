//! Profile routing: Pareto frontier of (arrival, home_departure) per node
//! over a departure-time window. One pass replaces N-sample Dijkstra.
//!
//! # Public interface
//!
//! [`ProfileRouter`] is the contract. The concrete type [`ProfileRouting`]
//! implements it. Callers hold `impl ProfileRouter` or the concrete type;
//! internal representation is free to change.

use std::{cmp::Reverse, collections::BinaryHeap, ops::ControlFlow};

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
    /// Mean travel time excluding unreachable departure times. `u32::MAX` if node is never reachable.
    pub mean_travel_time: Vec<u32>,
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
// Implementation
// ============================================================================

const ORIGIN_PREDECESSOR: u32 = u32::MAX;

const INITIAL_WALK: u16 = u16::MAX;
const PENDING_RELAXATION: u16 = INITIAL_WALK - 1;
const MAX_DELTA: u16 = PENDING_RELAXATION - 1;

/// A single entry for a node, representing a Pareto-optimal
/// (home_departure, arrival) pair.
///
/// Note:
/// - We can identify the predecessor entry by binary searching in prev node's entries for the one with the same home_departure_delta.
/// - We can determine whether an entry is a walk edge or a transit leg by checking if there's an edge and the time difference between predecessor entry's arrival and this entry's arrival is equal to the walk time of that edge.
/// - We can find the transit route by looking at all transit legs departing from predecessor node after predecessor's arrival time, and checking which one reaches this node at the correct arrival time.
#[derive(Debug, Copy, Clone)]
struct Entry {
    /// Predecessor node id
    /// - ORIGIN_PREDECESSOR if this is the source node
    prev: u32,

    /// Time leaving the source node (seconds since start of profile window)
    /// - INITIAL_WALK if all predecessors are walks
    /// - For Phase 2 (full transit routing) only:
    ///   - PENDING_RELAXATION if this entry is in the frontier but not yet finalized
    ///   - Only the last entry for each node can be PENDING_RELAXATION
    home_departure_delta: u16,

    /// Arrival time (seconds since start of profile window)
    /// - Total travel time if is initial walk
    arrival_delta: u16,
}

#[derive(Debug, Default)]
struct NodeEntries {
    /// Sorted by descending arrival time and descending home departure time,
    /// except the first entry which is a walk-only entry iff it's reachable by walk from the source within max_time.
    entries: Vec<Entry>,
}

#[derive(Debug)]
struct Frontier {
    nodes: Vec<NodeEntries>,
}

#[derive(Debug, Copy, Clone)]
struct TransitLeg {
    node_id: u32, // arrival node
    board_delta: u16,
    arrival_delta: u16,

    pattern_idx: u16,
    transit_ref: TransitRef,
}

/// Identifies the trip a [`TransitLeg`] belongs to. Always points at the
/// *boarding* stop's event/freq record (not the per-leg advancing index), so
/// backtracking can walk the forward chain to enumerate intermediate stops.
#[derive(Debug, Copy, Clone)]
enum TransitRef {
    Scheduled { event_idx: u32 },
    Frequency { freq_idx: u32 },
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

/// Opaque routing state. Internal representation is not part of the public
/// interface — swap freely as long as [`ProfileRouter`] is satisfied.
pub struct ProfileRouting {
    frontier: Frontier,
    isochrone: Isochrone,
}

impl ProfileRouter for ProfileRouting {
    fn compute(data: &PreparedData, query: &ProfileQuery) -> Self {
        assert!(
            query.window_start <= query.window_end,
            "Time window must have non-negative duration"
        );
        assert!(
            query.window_end - query.window_start + query.max_time < MAX_DELTA as u32,
            "Time values must fit in u16 deltas"
        );

        let n = data.num_nodes;
        let context = ProfileQueryContext::new(data, query);

        let mut frontier = Frontier {
            nodes: (0..n).map(|_| NodeEntries::default()).collect(),
        };

        // ── Phase 1: walk Dijkstra from source ───────────────────────────────
        // - Populate all walk-only entries (home_departure_delta = INITIAL_WALK),
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
                home_departure_delta: INITIAL_WALK,
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

            let _ = context.expand_transit_legs(
                ExpandTransitLegQuery {
                    node: node_id,
                    min_departure: query.window_start + arrival_delta as u32,
                    max_departure: query.window_end + arrival_delta as u32,
                    expand_headways: true,
                    max_arrival: None,
                },
                |leg| {
                    initial_transit_entries.push(PendingEntry {
                        node_id: leg.node_id,
                        entry: Entry {
                            prev: node_id,
                            home_departure_delta: leg.board_delta - arrival_delta,
                            arrival_delta: leg.arrival_delta,
                        },
                    });
                    ControlFlow::Continue(())
                },
            );

            for &(neighbor, distance) in &data.adj[node_id] {
                let new_arrival_delta = arrival_delta.saturating_add(get_walk_time(distance));

                if new_arrival_delta as u32 > query.max_time {
                    continue;
                }

                let new_entry = Entry {
                    prev: node_id,
                    home_departure_delta: INITIAL_WALK,
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

        // Sort by descending home_departure (= transit_departure − walk_time)
        initial_transit_entries.sort_by_key(|x| Reverse(x.entry.home_departure_delta));

        // ── Phase 2: main profile routing pass ───────────────────────────────

        // Iterate over initial transit entries in descending home departure order to guarantee that existing entries are never dominated.
        // Process all entries with the same home departure together as multi source Dijkstra search
        for chunk in initial_transit_entries
            .chunk_by(|a, b| a.entry.home_departure_delta == b.entry.home_departure_delta)
        {
            assert!(queue.is_empty());

            let home_departure_delta = chunk[0].entry.home_departure_delta;

            // Prime the Dijkstra search with current transit legs.
            for &transit_entry in chunk {
                relax(
                    &mut frontier,
                    &mut queue,
                    transit_entry.node_id,
                    transit_entry.entry,
                );
            }

            while let Some(Reverse(QueueEntry {
                arrival_delta,
                node_id,
            })) = queue.pop()
            {
                let entry = frontier.nodes[node_id as usize].entries.last_mut().unwrap();

                // Skip stale queue entry
                if arrival_delta != entry.arrival_delta {
                    continue;
                }

                entry.home_departure_delta = home_departure_delta; // finalize this entry

                // Relax walk edges
                for &(neighbor, distance) in &data.adj[node_id] {
                    let new_arrival_delta = arrival_delta.saturating_add(get_walk_time(distance));

                    if new_arrival_delta as u32 > query.max_time {
                        continue;
                    }

                    let new_entry = Entry {
                        prev: node_id,
                        home_departure_delta,
                        arrival_delta: new_arrival_delta,
                    };

                    relax(&mut frontier, &mut queue, neighbor, new_entry);
                }

                // Relax transit legs
                let min_departure_time =
                    query.window_start + arrival_delta as u32 + query.transfer_slack;
                let mut max_departure_time = query.window_end + query.max_time;
                if let Some(prev_entry) =
                    frontier.nodes[node_id as usize].entries.iter().nth_back(1)
                {
                    let prev_min_departure_time = query.window_start
                        + get_true_arrival_delta(home_departure_delta, prev_entry) as u32
                        + query.transfer_slack;
                    // This might cause repeated relaxation for the same transit leg
                    // For example maybe we already finalized a transit ride A -> B -> C boarding on A arriving to B at time 100
                    // but then we find a new walk route to B with arrival time 90, allowing us to board the same vehicle for the B -> C ride
                    // but the arrival at C will be dominated by the existing entry from A -> B -> C since the new walk route has earlier departure
                    // so this will be a bit of wasted effort but harmless.
                    max_departure_time = prev_min_departure_time
                        .saturating_sub(1)
                        .min(max_departure_time);
                }
                if min_departure_time > max_departure_time {
                    continue;
                }
                let _ = context.expand_transit_legs(
                    ExpandTransitLegQuery {
                        node: node_id,
                        min_departure: min_departure_time,
                        max_departure: max_departure_time,
                        expand_headways: true,
                        max_arrival: None,
                    },
                    |leg| {
                        relax(
                            &mut frontier,
                            &mut queue,
                            leg.node_id,
                            Entry {
                                prev: node_id,
                                home_departure_delta,
                                arrival_delta: leg.arrival_delta,
                            },
                        );
                        ControlFlow::Continue(())
                    },
                );
            }
        }

        // ── Phase 3: compute isochrone stats ───────────────────────────────

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

fn relax(
    frontier: &mut Frontier,
    queue: &mut BinaryHeap<Reverse<QueueEntry>>,
    node_id: u32,
    mut new_entry: Entry,
) {
    let neighbor_entries = &mut frontier.nodes[node_id as usize].entries;
    if let Some(best) = neighbor_entries.last_mut() {
        if is_new_entry_dominated(&new_entry, best) {
            return;
        }
        new_entry.home_departure_delta = PENDING_RELAXATION;
        if best.home_departure_delta == PENDING_RELAXATION {
            // `best` was an entry from the current home departure time
            *best = new_entry;
        } else {
            // `best` was an entry with a later home departure time
            neighbor_entries.push(new_entry);
        }
    } else {
        new_entry.home_departure_delta = PENDING_RELAXATION;
        neighbor_entries.push(new_entry);
    }

    queue.push(Reverse(QueueEntry {
        arrival_delta: new_entry.arrival_delta,
        node_id,
    }));
}

/// Get arrival delta for an entry with special handling for initial transit legs (which use arrival_delta field to store the total travel time since it has flexible home departure time)
fn get_true_arrival_delta(home_departure_delta: u16, entry: &Entry) -> u16 {
    entry.arrival_delta
        + if entry.home_departure_delta == INITIAL_WALK {
            home_departure_delta
        } else {
            0
        }
}

fn is_new_entry_dominated(new: &Entry, existing: &Entry) -> bool {
    new.arrival_delta >= get_true_arrival_delta(new.home_departure_delta, existing)
}

struct ExpandTransitLegQuery {
    node: u32,
    min_departure: u32,
    max_departure: u32,
    expand_headways: bool,
    max_arrival: Option<u32>,
}

struct ProfileQueryContext<'a> {
    data: &'a PreparedData,
    query: &'a ProfileQuery,
    active_patterns: Vec<usize>,
}

impl<'a> ProfileQueryContext<'a> {
    fn new(data: &'a PreparedData, query: &'a ProfileQuery) -> Self {
        let active_patterns = crate::router::patterns_for_date(data, query.date);
        Self {
            data,
            query,
            active_patterns,
        }
    }

    /// For each transit vehicle departing `node` within `[min_departure, max_departure]`,
    /// follow the trip forward and emit one [`TransitLeg`] per downstream stop, with
    /// `arrival_delta` filled in. `entry.prev` is set to `node` (the boarding stop) for
    /// path reconstruction. Covers both scheduled and frequency-based routes.
    fn expand_transit_legs<F>(&self, query: ExpandTransitLegQuery, mut visit: F) -> ControlFlow<()>
    where
        F: FnMut(TransitLeg) -> ControlFlow<()>,
    {
        let ExpandTransitLegQuery {
            node,
            min_departure,
            max_departure,
            expand_headways,
            max_arrival,
        } = query;

        let stop_idx = match self.data.node_to_stop.get(&node) {
            Some(&idx) => idx,
            None => return ControlFlow::Continue(()),
        };
        let max_arrival = max_arrival.unwrap_or(self.query.window_end + self.query.max_time);

        for &pat_idx in &self.active_patterns {
            let pat = &self.data.patterns[pat_idx];

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
                let board_delta = (event.time_offset - self.query.window_start) as u16;
                let board_event_idx = (base + start + j) as u32;
                let mut flat_idx = board_event_idx as usize;

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
                    visit(TransitLeg {
                        node_id: self.data.stop_to_node[next.stop_index as usize],
                        board_delta,
                        arrival_delta: (arrival - self.query.window_start) as u16,
                        pattern_idx: pat_idx as u16,
                        transit_ref: TransitRef::Scheduled {
                            event_idx: board_event_idx,
                        },
                    })?;
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
                    let board_delta = (board - self.query.window_start) as u16;
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
                        visit(TransitLeg {
                            node_id: self.data.stop_to_node[f.next_stop_index as usize],
                            board_delta,
                            arrival_delta: (arrival - self.query.window_start) as u16,
                            pattern_idx: pat_idx as u16,
                            transit_ref: TransitRef::Frequency { freq_idx: fi },
                        })?;
                        if f.next_freq_index == u32::MAX {
                            break;
                        }
                        freq_idx = f.next_freq_index;
                    }

                    board += freq.headway_secs;

                    if !expand_headways {
                        break;
                    }
                }
            }
        }
        ControlFlow::Continue(())
    }
}
