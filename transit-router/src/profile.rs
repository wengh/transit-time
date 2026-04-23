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
    /// Length = `data.num_nodes`. Mean of `(arrival − home_departure)` in
    /// seconds, *conditioned on reachability*: integrate travel time over the
    /// home-departure sub-window where `v` is reachable within `max_time`,
    /// divide by the length of that sub-window. Pairs with `reachable_fraction`
    /// as the orthogonal "how often" signal. Undefined when
    /// `reachable_fraction[v] == 0` (consumers must check that first).
    pub mean_travel_time: Vec<u16>,
    /// Length = `data.num_nodes`. Fraction of the query window during which
    /// `v` is reachable within `max_time`, quantized over `u16::MAX`
    /// (i.e. fraction = `value / u16::MAX as f32`). Computed as the normalised
    /// interval union over the per-node Pareto frontier.
    pub reachable_fraction: Vec<u16>,
    pub query: ProfileQuery,
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
    // Empty string for walks.
    pub start_stop_name: String,
    pub end_stop_name: String,
    /// `None` for walks.
    pub route_index: Option<u32>,
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
const MAX_DELTA: u16 = INITIAL_WALK - 1;

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

#[derive(Debug, Copy, Clone)]
struct DestinationStats {
    mean_travel_time: u16,
    reachable_fraction: u16,
}

struct Index {
    /// Inverted index: per global stop_idx, the active patterns that actually
    /// serve that stop (have ≥1 scheduled event or ≥1 frequency entry there).
    /// Replaces the per-pop `O(active_patterns)` scan in `expand_transit_legs`
    /// with `O(patterns_serving_this_stop)`. Populated once per query in `new`.
    patterns_at_stop: Vec<Vec<u32>>,
}

/// Opaque routing state. Internal representation is not part of the public
/// interface — swap freely as long as [`ProfileRouter`] is satisfied.
pub struct ProfileRouting {
    frontier: Frontier,
    isochrone: Isochrone,
    patterns: Index,
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

        let t_total = std::time::Instant::now();

        let n = data.num_nodes;
        let t_setup = std::time::Instant::now();
        let index = Index::new(data, query);
        let context = ProfileQueryContext {
            data,
            query,
            index: &index,
        };

        let mut frontier = Frontier {
            nodes: (0..n).map(|_| NodeEntries::default()).collect(),
        };
        let setup_ms = t_setup.elapsed().as_secs_f64() * 1e3;

        let t_phase1 = std::time::Instant::now();

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

            let window_length = (query.window_end - query.window_start) as u16;
            let _ = context.expand_transit_legs(
                ExpandTransitLegQuery {
                    node: node_id,
                    min_departure: query.window_start + arrival_delta as u32,
                    max_departure: query.window_end + query.max_time,
                    expand_headways: true,
                    max_arrival: None,
                },
                |leg| {
                    initial_transit_entries.push(PendingEntry {
                        node_id: leg.node_id,
                        entry: Entry {
                            prev: node_id,
                            // Clamp home departure to be within the query window
                            home_departure_delta: window_length
                                .min(leg.board_delta - arrival_delta),
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

        let phase1_ms = t_phase1.elapsed().as_secs_f64() * 1e3;
        let initial_transit_count = initial_transit_entries.len();
        let t_phase2 = std::time::Instant::now();

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

                // Relax walk edges
                for &(neighbor, distance) in &data.adj[node_id] {
                    let new_arrival_delta = arrival_delta.saturating_add(get_walk_time(distance));

                    if (new_arrival_delta - home_departure_delta) as u32 > query.max_time {
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
                        expand_headways: false,
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

        let phase2_ms = t_phase2.elapsed().as_secs_f64() * 1e3;
        let t_phase3 = std::time::Instant::now();

        // ── Phase 3: compute isochrone stats ───────────────────────────────
        let mut isochrone = Isochrone {
            mean_travel_time: vec![0; n],
            reachable_fraction: vec![0; n],
            query: query.clone(),
        };
        for ((node_entries, mean_travel_time), reachable_fraction) in frontier
            .nodes
            .iter()
            .zip(&mut isochrone.mean_travel_time)
            .zip(&mut isochrone.reachable_fraction)
        {
            let stats = context.compute_destination_stats(&node_entries.entries);
            *mean_travel_time = stats.mean_travel_time;
            *reachable_fraction = stats.reachable_fraction;
        }

        let phase3_ms = t_phase3.elapsed().as_secs_f64() * 1e3;
        let total_ms = t_total.elapsed().as_secs_f64() * 1e3;
        eprintln!(
            "[profile] setup={:.1}ms phase1(walk)={:.1}ms phase2(transit)={:.1}ms phase3(stats)={:.1}ms total={:.1}ms initial_transit_entries={}",
            setup_ms, phase1_ms, phase2_ms, phase3_ms, total_ms, initial_transit_count,
        );

        Self {
            frontier,
            isochrone,
            patterns: index,
        }
    }

    fn isochrone(&self) -> &Isochrone {
        &self.isochrone
    }

    fn optimal_paths(&self, data: &PreparedData, destination: u32) -> Vec<Path> {
        let context = ProfileQueryContext {
            data,
            query: &self.isochrone.query,
            index: &self.patterns,
        };
        self.frontier.nodes[destination as usize]
            .entries
            .iter()
            .map(|entry| self.reconstruct_path(data, &context, destination, *entry))
            .collect()
    }
}

impl ProfileRouting {
    fn reconstruct_path(
        &self,
        data: &PreparedData,
        context: &ProfileQueryContext,
        destination: u32,
        entry: Entry,
    ) -> Path {
        let home_departure_delta = if entry.home_departure_delta == INITIAL_WALK {
            0 // For walk-only entries, just use window_start as home departure since it doesn't matter
        } else {
            entry.home_departure_delta
        };

        let delta_to_time = |delta: u16| delta as u32 + self.isochrone.query.window_start;

        let mut segments: Vec<PathSegment> = Vec::new();

        let source = self.isochrone.query.source_node;
        let mut curr_node = destination;
        let mut curr = entry;
        while curr_node != source {
            let prev_node = curr.prev;

            // Find predecessor entry
            let prev_entries = &self.frontier.nodes[prev_node as usize].entries;
            assert!(!prev_entries.is_empty());
            let prev_entry_idx = prev_entries
                .binary_search_by_key(&Reverse(curr.home_departure_delta), |e| {
                    Reverse(e.home_departure_delta)
                })
                // If not found, it means we reached this from initial walk, so switch to the initial walk entry at index 0
                .unwrap_or(0);
            let prev = prev_entries[prev_entry_idx];
            assert!(
                prev.home_departure_delta == INITIAL_WALK
                    || prev.home_departure_delta == entry.home_departure_delta
            );

            // Determine whether this entry is walk or transit
            let prev_arrival_delta = get_true_arrival_delta(home_departure_delta, &prev);
            let is_walk = curr.home_departure_delta == INITIAL_WALK || {
                let edge_weight = data.adj[prev_node]
                    .iter()
                    .find(|&&(neighbor, _)| neighbor == curr_node)
                    .map(|&(_, w)| w);
                match edge_weight {
                    Some(w) => {
                        prev_arrival_delta.saturating_add(get_walk_time(w)) == curr.arrival_delta
                    }
                    None => false,
                }
            };

            if is_walk {
                if let Some(segment) = segments.last_mut()
                    && segment.kind == SegmentKind::Walk
                {
                    // Merge into next walk segment
                    segment.start_time = delta_to_time(prev_arrival_delta);
                    segment.node_sequence.push(curr_node);
                } else {
                    let curr_arrival_time = get_true_arrival_delta(home_departure_delta, &curr);
                    segments.push(PathSegment {
                        kind: SegmentKind::Walk,
                        start_time: delta_to_time(prev_arrival_delta),
                        end_time: delta_to_time(curr_arrival_time),
                        wait_time: 0,
                        start_stop_name: String::new(),
                        end_stop_name: String::new(),
                        route_index: None,
                        route_name: None,
                        node_sequence: vec![curr_node, prev_node], // For walk we have the sequence in reverse order so insert is fast. Flip to correct order at the end.
                    });
                }
            } else {
                // Is transit
                // Find the transit leg taken
                // Require transfer slack unless this is from initial walk
                let min_departure = delta_to_time(prev_arrival_delta)
                    + if prev.home_departure_delta == INITIAL_WALK {
                        0
                    } else {
                        self.isochrone.query.transfer_slack
                    };
                let max_departure = delta_to_time(curr.arrival_delta);
                let mut found = None;
                let _ = context.expand_transit_legs(
                    ExpandTransitLegQuery {
                        node: prev_node,
                        min_departure,
                        max_departure,
                        expand_headways: false,
                        max_arrival: Some(max_departure),
                    },
                    |leg| {
                        if leg.node_id == curr_node && leg.arrival_delta == curr.arrival_delta {
                            found = Some(leg);
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    },
                );
                let leg = found.unwrap_or_else(|| {
                    panic!(
                        "No transit leg found from {} to {} departing between t={} and t={}\nprev_entries={:#?}\ncurr={:#?}",
                        context.get_stop_name(prev_node),
                        context.get_stop_name(curr_node),
                        min_departure,
                        max_departure,
                        prev_entries,
                        curr,
                    )
                });

                // Find the route and the stops
                let pat = &data.patterns[leg.pattern_idx as usize];
                let mut node_sequence = Vec::new();
                let end_stop = data.node_to_stop[&curr_node];
                let route_index = match leg.transit_ref {
                    TransitRef::Scheduled { event_idx } => {
                        let events = &pat.stop_index.events_by_stop.data;
                        let mut curr_event_idx = event_idx;
                        let mut reached_end_stop = false;
                        let route_index = loop {
                            let event = &events[curr_event_idx as usize];
                            if !reached_end_stop {
                                node_sequence.push(data.stop_to_node[event.stop_index as usize]);
                            }
                            if event.stop_index == end_stop {
                                reached_end_stop = true;
                            }
                            if event.next_event_index == u32::MAX {
                                // Last event
                                break pat.sentinel_routes[&(curr_event_idx as u32)];
                            }
                            curr_event_idx = event.next_event_index;
                        };
                        assert!(reached_end_stop);
                        route_index
                    }
                    TransitRef::Frequency { freq_idx } => {
                        let freqs = &pat.frequency_routes;
                        let mut curr_idx = freq_idx;
                        loop {
                            let freq = &freqs[curr_idx as usize];
                            node_sequence.push(data.stop_to_node[freq.stop_index as usize]);
                            if freq.next_stop_index == end_stop {
                                // Last hop: the alighting stop only appears as
                                // `next_stop_index`, never as a subsequent `stop_index`.
                                node_sequence
                                    .push(data.stop_to_node[freq.next_stop_index as usize]);
                                break;
                            }
                            curr_idx = freq.next_freq_index;
                        }
                        freqs[freq_idx as usize].route_index
                    }
                };
                let route_name = data.route_names[route_index as usize].clone();
                segments.push(PathSegment {
                    kind: SegmentKind::Transit,
                    start_time: delta_to_time(leg.board_delta),
                    end_time: delta_to_time(leg.arrival_delta),
                    wait_time: leg.board_delta as u32 - prev_arrival_delta as u32,
                    start_stop_name: context.get_stop_name(prev_node).to_string(),
                    end_stop_name: context.get_stop_name(curr_node).to_string(),
                    route_index: Some(route_index as u32),
                    route_name: Some(route_name),
                    node_sequence,
                });
            }

            curr_node = prev_node;
            curr = prev;
        }

        segments.reverse();
        for segment in &mut segments {
            if segment.kind == SegmentKind::Walk {
                // We built it in reverse order so now flip it back
                segment.node_sequence.reverse();
            }
        }

        Path {
            home_departure: delta_to_time(home_departure_delta),
            arrival_time: delta_to_time(entry.arrival_delta),
            total_time: entry.arrival_delta as u32 - home_departure_delta as u32,
            segments,
        }
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
    new_entry: Entry,
) {
    let neighbor_entries = &mut frontier.nodes[node_id as usize].entries;
    if let Some(first) = neighbor_entries.first()
        && first.home_departure_delta == INITIAL_WALK
        && is_new_entry_dominated(&new_entry, first)
    {
        return;
    }
    if let Some(best) = neighbor_entries.last_mut() {
        if is_new_entry_dominated(&new_entry, best) {
            return;
        }
        if best.home_departure_delta == new_entry.home_departure_delta {
            // `best` was an entry in the current round, so relax it
            *best = new_entry;
        } else {
            // `best` was an entry with a later home departure time, so add a new frontier entry
            neighbor_entries.push(new_entry);
        }
    } else {
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
    index: &'a Index,
}

impl Index {
    fn new(data: &PreparedData, query: &ProfileQuery) -> Self {
        let active_patterns = crate::router::patterns_for_date(data, query.date);
        let num_stops = data.stops.len();
        let mut patterns_at_stop: Vec<Vec<u32>> = vec![Vec::new(); num_stops];
        for &pat_idx in &active_patterns {
            let pat = &data.patterns[pat_idx];
            let evt_off = &pat.stop_index.events_by_stop.offsets;
            let freq_off = &pat.stop_index.freq_by_stop.offsets;
            for s in 0..num_stops {
                if evt_off[s + 1] > evt_off[s] || freq_off[s + 1] > freq_off[s] {
                    patterns_at_stop[s].push(pat_idx as u32);
                }
            }
        }
        Self { patterns_at_stop }
    }
}

impl<'a> ProfileQueryContext<'a> {
    fn get_stop_name(&self, node_id: u32) -> &'a str {
        let stop_idx = self.data.node_to_stop[&node_id];
        self.data.stops[stop_idx as usize].name.as_str()
    }

    fn compute_destination_stats(&self, mut entries: &[Entry]) -> DestinationStats {
        if entries.is_empty() {
            return DestinationStats {
                mean_travel_time: 0,
                reachable_fraction: 0,
            };
        }

        let mut time_limit = self.query.max_time + 1; // exclusive limit
        let walk_entry = match entries {
            [walk, rest @ ..] if walk.home_departure_delta == INITIAL_WALK => {
                entries = rest;
                time_limit = walk.arrival_delta as u32;
                Some(*walk)
            }
            _ => None,
        };

        // How many integer-second home_departure values fit in the window.
        let window_length = self.query.window_end - self.query.window_start + 1;

        // Iterating .rev() walks entries in ascending home_departure / ascending
        // arrival. Each entry covers home-departure values t ∈ (prev_departure,
        // departure]; for t in that segment travel(t) = arrival − t, ranging
        // from `travel_min = arrival − departure` (at t = departure) up to
        // `travel_min + segment_len − 1` (at t = prev_departure + 1).
        //
        // prev_departure starts at -1 (exclusive lower bound) so the first
        // segment correctly includes t = 0.
        let mut numerator: u64 = 0;
        let mut denominator: u32 = 0;
        let mut prev_departure: i32 = -1;

        // Iterate in ascending arrival & departure order
        for entry in entries.iter().rev() {
            let arrival = entry.arrival_delta as u32;
            let departure = entry.home_departure_delta as u32;
            let travel = arrival - departure;
            if travel >= time_limit {
                continue;
            }

            let segment_len = (departure as i32 - prev_departure) as u32;
            // Trim segment from the LEFT (largest travel times) when travel
            // exceeds the limit: only the rightmost `time_limit − travel` t's
            // are reachable via this entry.
            let reachable = (time_limit - travel).min(segment_len);
            denominator += reachable;
            // Sum of travel(t) over the reachable t's:
            //   reachable * travel_min + (0 + 1 + … + reachable − 1)
            numerator += reachable as u64 * travel as u64;
            numerator += (reachable as u64) * (reachable as u64 - 1) / 2;

            prev_departure = departure as i32;
        }

        if let Some(walk) = walk_entry {
            // Walk fills every t not claimed by a transit entry above.
            numerator += walk.arrival_delta as u64 * (window_length - denominator) as u64;
            denominator = window_length;
        }

        // Quantize fraction over u16::MAX. `denominator <= window_length` by
        // construction, so the ratio fits in u16 without saturation.
        let fraction_q = (denominator as u64 * u16::MAX as u64 / window_length as u64) as u16;
        let mean = if denominator > 0 {
            (numerator / denominator as u64) as u16
        } else {
            0
        };
        DestinationStats {
            mean_travel_time: mean,
            reachable_fraction: fraction_q,
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

        for &pat_idx in &self.index.patterns_at_stop[stop_idx as usize] {
            let pat = &self.data.patterns[pat_idx as usize];

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
