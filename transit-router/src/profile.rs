//! Profile routing: Pareto frontier of (arrival, home_departure) per node
//! over a departure-time window. One pass replaces N-sample Dijkstra.
//!
//! # Public interface
//!
//! [`ProfileRouter`] is the contract. The concrete type [`ProfileRouting`]
//! implements it. Callers hold `impl ProfileRouter` or the concrete type;
//! internal representation is free to change.

use std::{cmp::Reverse, ops::ControlFlow};

use radix_heap::RadixHeapMap;

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use crate::data::PreparedData;
use serde::Serialize;

/// Zero-cost no-op Instant for wasm32 where std::time::Instant panics.
#[cfg(target_arch = "wasm32")]
struct Instant;
#[cfg(target_arch = "wasm32")]
impl Instant {
    fn now() -> Self {
        Instant
    }
    fn elapsed(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}

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

const LAST_ENTRY: u32 = u32::MAX;
const INVALID_PREV: u32 = u32::MAX;
const MAX_DELTA: u16 = u16::MAX;

/// Sentinel `ArenaEntry` for an empty inline head.
/// `prev = INVALID_PREV` signals "no head".
/// The sibling pointer is `LAST_ENTRY` so that walking the chain
/// of an empty head is a no-op (used by `iter` paths that don't pre-check).
const EMPTY_ARENA_ENTRY: ArenaEntry = ArenaEntry {
    entry: Entry {
        prev: INVALID_PREV,
        home_departure_delta: 0,
        arrival_delta: 0,
    },
    sibling_entry_idx: LAST_ENTRY,
};

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
    prev: u32,

    /// Time leaving the source node (seconds since start of profile window)
    home_departure_delta: u16,

    /// Arrival time (seconds since start of profile window)
    arrival_delta: u16,
}

#[derive(Debug, Copy, Clone)]
struct ArenaEntry {
    entry: Entry,

    /// Index of next entry for the same node
    /// - LAST_ENTRY if no more sibling entries
    ///
    /// The chain of entries is sorted by ascending arrival time
    /// and home departure time.
    sibling_entry_idx: u32,
}

/// Per-node frontier state. The head Pareto entry is stored inline as a full
/// `ArenaEntry` (entry + sibling pointer), uniform with the entries it points
/// to in the arena. "No head" is encoded by the sentinel `prev == INVALID_PREV`
/// (guaranteed unused by the `MAX_DELTA` assert in `compute`), not
/// by an `Option` discriminant — that's what gets us to 16 bytes.
///
/// Layout: `ArenaEntry` (12B = Entry 8B + u32) + `Option<u16>` (4B) =
/// 16 bytes, align 4. Verified by the const assert below.
#[derive(Debug)]
struct NodeFrontier {
    /// Most-recent (smallest-arrival) Pareto entry plus its sibling pointer
    /// into the arena tail. Empty iff `head.entry.arrival_delta == INVALID_DELTA`.
    head: ArenaEntry,

    /// Time it takes to walk from source to this node, or `None` if not
    /// walk-reachable within `max_time`.
    walk_only_time: Option<u16>,
}

impl NodeFrontier {
    fn has_head(&self) -> bool {
        self.head.entry.prev != INVALID_PREV
    }
}

const _: () = assert!(std::mem::size_of::<NodeFrontier>() == 16);

#[derive(Debug)]
struct Frontier {
    // Arena for entry linked-list tails. The head of each chain lives
    // inline on its `NodeFrontier`; the arena holds only the 2nd-and-later
    // entries.
    arena: Vec<ArenaEntry>,
    nodes: Vec<NodeFrontier>,
}

impl Frontier {
    /// Insert `entry` as the new head of `node_id`'s chain. Any existing
    /// head is evicted into the arena and becomes the new sibling.
    fn push(&mut self, node_id: u32, entry: Entry) {
        let nf = &mut self.nodes[node_id as usize];
        let sibling = if nf.has_head() {
            // Evict old head into arena; new head's sibling points there.
            let new_arena_idx = self.arena.len() as u32;
            self.arena.push(nf.head);
            new_arena_idx
        } else {
            LAST_ENTRY
        };
        nf.head = ArenaEntry {
            entry,
            sibling_entry_idx: sibling,
        };
    }

    fn head(&self, node_id: u32) -> Option<&Entry> {
        let nf = &self.nodes[node_id as usize];
        if nf.has_head() {
            Some(&nf.head.entry)
        } else {
            None
        }
    }

    fn head_next(&self, node_id: u32) -> Option<&Entry> {
        let nf = &self.nodes[node_id as usize];
        if nf.head.sibling_entry_idx != LAST_ENTRY {
            Some(&self.arena[nf.head.sibling_entry_idx as usize].entry)
        } else {
            None
        }
    }

    fn head_mut(&mut self, node_id: u32) -> Option<&mut Entry> {
        let nf = &mut self.nodes[node_id as usize];
        if nf.has_head() {
            Some(&mut nf.head.entry)
        } else {
            None
        }
    }

    fn iter(&self, node_id: u32) -> impl Iterator<Item = &Entry> {
        let nf = &self.nodes[node_id as usize];
        let (head, mut next_idx) = if nf.has_head() {
            (Some(&nf.head.entry), nf.head.sibling_entry_idx)
        } else {
            (None, LAST_ENTRY)
        };
        let arena = &self.arena;
        head.into_iter().chain(std::iter::from_fn(move || {
            if next_idx == LAST_ENTRY {
                None
            } else {
                let arena_entry = &arena[next_idx as usize];
                next_idx = arena_entry.sibling_entry_idx;
                Some(&arena_entry.entry)
            }
        }))
    }
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
            query.window_end - query.window_start + query.max_time <= MAX_DELTA as u32,
            "Time values must fit in u16 deltas"
        );

        let t_total = Instant::now();

        let n = data.num_nodes;
        let t_setup = Instant::now();
        let index = Index::new(data, query);
        let context = ProfileQueryContext {
            data,
            query,
            index: &index,
        };

        let mut frontier = Frontier {
            nodes: (0..n)
                .map(|_| NodeFrontier {
                    head: EMPTY_ARENA_ENTRY,
                    walk_only_time: None,
                })
                .collect(),
            arena: Vec::new(),
        };
        let setup_ms = t_setup.elapsed().as_secs_f64() * 1e3;

        let t_phase1 = Instant::now();

        // ── Phase 1: walk Dijkstra from source ───────────────────────────────
        // - Populate all walk-only entries (home_departure_delta = INITIAL_WALK),
        //   valid for any home departure across the whole window.
        // - Gather all transit legs available from walk-reachable stops:
        //   departure time in [window_start + walk_time, window_end + walk_time],
        //   i.e. home departure (= transit dep − walk_time) in [window_start, window_end].
        // - Sort the transit legs by home departure time.

        let mut initial_transit_entries: Vec<PendingEntry> = Vec::new();

        // Our workload has monotonic pop so we can use a radix heap instead of a binary heap for better performance.
        let mut queue: RadixHeapMap<Reverse<u16>, u32> = RadixHeapMap::new();
        queue.push(Reverse(0u16), query.source_node);
        frontier.nodes[query.source_node as usize].walk_only_time = Some(0);

        while let Some((Reverse(walk_time), node_id)) = queue.pop() {
            // Skip stale queue entry
            if walk_time > frontier.nodes[node_id as usize].walk_only_time.unwrap() {
                continue;
            }

            let window_length = (query.window_end - query.window_start) as u16;
            let _ = context.expand_transit_legs(
                ExpandTransitLegQuery {
                    node: node_id,
                    min_departure: query.window_start + walk_time as u32,
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
                            home_departure_delta: window_length.min(leg.board_delta - walk_time),
                            arrival_delta: leg.arrival_delta,
                        },
                    });
                    ControlFlow::Continue(())
                },
            );

            for &(neighbor, edge_walk_time) in &data.adj[node_id] {
                let new_walk_time = walk_time.saturating_add(edge_walk_time);

                if new_walk_time as u32 > query.max_time {
                    continue;
                }

                if let Some(existing_walk_time) = frontier.nodes[neighbor as usize].walk_only_time
                    && new_walk_time >= existing_walk_time
                {
                    continue;
                }

                frontier.nodes[neighbor as usize].walk_only_time = Some(new_walk_time);

                queue.push(Reverse(new_walk_time), neighbor);
            }
        }

        // Sort by descending home_departure (= transit_departure − walk_time)
        initial_transit_entries.sort_by_key(|x| Reverse(x.entry.home_departure_delta));

        let phase1_ms = t_phase1.elapsed().as_secs_f64() * 1e3;
        let initial_transit_count = initial_transit_entries.len();
        let t_phase2 = Instant::now();

        // ── Phase 2: main profile routing pass ───────────────────────────────

        // Iterate over initial transit entries in descending home departure order to guarantee that existing entries are never dominated.
        // Process all entries with the same home departure together as multi source Dijkstra search
        for chunk in initial_transit_entries
            .chunk_by(|a, b| a.entry.home_departure_delta == b.entry.home_departure_delta)
        {
            queue.clear();

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

            while let Some((Reverse(arrival_delta), node_id)) = queue.pop() {
                let entry = frontier.head(node_id).unwrap();

                // Skip stale queue entry
                if arrival_delta != entry.arrival_delta {
                    continue;
                }

                // Relax walk edges
                for &(neighbor, edge_walk_time) in &data.adj[node_id] {
                    let new_arrival_delta = arrival_delta.saturating_add(edge_walk_time);

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

                // Skip unnecessary work if not a transit stop
                if data.node_to_stop[node_id as usize] == u32::MAX {
                    continue;
                }

                // Relax transit legs
                let min_departure_time =
                    query.window_start + arrival_delta as u32 + query.transfer_slack;
                let mut max_departure_time = query.window_end + query.max_time;

                let next_min_departure_time = if let Some(next_entry) = frontier.head_next(node_id)
                {
                    // This might cause repeated relaxation for the same transit leg
                    // For example maybe we already finalized a transit ride A -> B -> C boarding on A arriving to B at time 100
                    // but then we find a new walk route to B with arrival time 90, allowing us to board the same vehicle for the B -> C ride
                    // but the arrival at C will be dominated by the existing entry from A -> B -> C since the new walk route has earlier departure
                    // so this will be a bit of wasted effort but harmless.
                    Some(
                        query.window_start + next_entry.arrival_delta as u32 + query.transfer_slack,
                    )
                } else if let Some(walk_time) = frontier.nodes[node_id as usize].walk_only_time {
                    Some(query.window_start + home_departure_delta as u32 + walk_time as u32)
                } else {
                    None
                };
                if let Some(next_min_departure_time) = next_min_departure_time {
                    max_departure_time = next_min_departure_time
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
        let t_phase3 = Instant::now();

        // ── Phase 3: compute isochrone stats ───────────────────────────────
        let mut isochrone = Isochrone {
            mean_travel_time: vec![0; n],
            reachable_fraction: vec![0; n],
            query: query.clone(),
        };
        for ((node_id, mean_travel_time), reachable_fraction) in (0..n as u32)
            .zip(&mut isochrone.mean_travel_time)
            .zip(&mut isochrone.reachable_fraction)
        {
            let stats = context.compute_destination_stats(&frontier, node_id);
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
        self.frontier
            .iter(destination)
            .map(|entry| self.reconstruct_path(&context, destination, Some(*entry)))
            .chain(
                self.frontier.nodes[destination as usize]
                    .walk_only_time
                    .map(|_| self.reconstruct_path(&context, destination, None)),
            )
            .collect()
    }
}

impl ProfileRouting {
    fn reconstruct_path(
        &self,
        context: &ProfileQueryContext,
        destination: u32,
        entry: Option<Entry>, // None for getting walk path
    ) -> Path {
        let home_departure_delta = if entry.is_none() {
            0 // For walk-only entries, just use window_start as home departure since it doesn't matter
        } else {
            entry.unwrap().home_departure_delta
        };

        // Helper functions
        let delta_to_time = |delta: u16| delta as u32 + self.isochrone.query.window_start;
        let get_true_arrival_delta = |node: u32, entry: &Option<Entry>| {
            if let Some(entry) = entry {
                entry.arrival_delta
            } else {
                // Walk-only entry, arrival delta is just departure delta + walk time
                home_departure_delta + self.frontier.nodes[node as usize].walk_only_time.unwrap()
            }
        };

        let mut segments: Vec<PathSegment> = Vec::new();

        let source = self.isochrone.query.source_node;
        let mut curr_node = destination;
        let mut curr = entry;
        while curr_node != source {
            let prev_node = if let Some(curr) = curr {
                curr.prev
            } else {
                // Walk-only entry, find predecessor from neighbours
                let curr_walk_time = self.frontier.nodes[curr_node as usize]
                    .walk_only_time
                    .unwrap();
                context.data.adj[curr_node]
                    .iter()
                    .find(|&&(neighbor, edge_walk_time)| {
                        let Some(walk_time) = self.frontier.nodes[neighbor as usize].walk_only_time else {
                            return false;
                        };
                        walk_time.saturating_add(edge_walk_time) == curr_walk_time
                    })
                    .map(|&(neighbor, _)| neighbor)
                    .unwrap_or_else(|| panic!("No walk predecessor found for node {} in walk-only path reconstruction", curr_node))
            };

            // Find predecessor entry
            let prev = if let Some(curr) = curr {
                // Find the predecessor entry with the same home_departure_delta
                self.frontier
                    .iter(prev_node)
                    .find(|e| e.home_departure_delta == curr.home_departure_delta)
                    .map(|e| Some(*e))
                    .unwrap_or(None) // Predecessor is walk-only
            } else {
                // Walk-only
                None
            };

            // Determine whether this entry is walk or transit
            let prev_arrival_delta = get_true_arrival_delta(prev_node, &prev);

            let is_walk = if let Some(curr) = curr {
                let edge_weight = context.data.adj[prev_node]
                    .iter()
                    .find(|&&(neighbor, _)| neighbor == curr_node)
                    .map(|&(_, w)| w);
                match edge_weight {
                    Some(w) => prev_arrival_delta.saturating_add(w) == curr.arrival_delta,
                    None => false,
                }
            } else {
                true
            };

            if is_walk {
                if let Some(segment) = segments.last_mut()
                    && segment.kind == SegmentKind::Walk
                {
                    // Merge into next walk segment
                    segment.start_time = delta_to_time(prev_arrival_delta);
                    segment.node_sequence.push(curr_node);
                } else {
                    let curr_arrival_time = get_true_arrival_delta(curr_node, &curr);
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
                let curr = curr.unwrap();
                // Find the transit leg taken
                // Require transfer slack unless this is from initial walk
                let min_departure = delta_to_time(prev_arrival_delta)
                    + if prev.is_none() {
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
                        self.frontier.iter(prev_node).collect::<Vec<_>>(),
                        curr,
                    )
                });

                // Find the route and the stops
                let pat = &context.data.patterns[leg.pattern_idx as usize];
                let mut node_sequence = Vec::new();
                let end_stop = context.data.node_to_stop[curr_node as usize];
                let route_index = match leg.transit_ref {
                    TransitRef::Scheduled { event_idx } => {
                        let events = &pat.stop_index.events_by_stop.data;
                        let mut curr_event_idx = event_idx;
                        let mut reached_end_stop = false;
                        let route_index = loop {
                            let event = &events[curr_event_idx as usize];
                            if !reached_end_stop {
                                node_sequence
                                    .push(context.data.stop_to_node[event.stop_index as usize]);
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
                            node_sequence.push(context.data.stop_to_node[freq.stop_index as usize]);
                            if freq.next_stop_index == end_stop {
                                // Last hop: the alighting stop only appears as
                                // `next_stop_index`, never as a subsequent `stop_index`.
                                node_sequence
                                    .push(context.data.stop_to_node[freq.next_stop_index as usize]);
                                break;
                            }
                            curr_idx = freq.next_freq_index;
                        }
                        freqs[freq_idx as usize].route_index
                    }
                };
                let route_name = context.data.route_names[route_index as usize].clone();
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

        let arrival_delta = get_true_arrival_delta(destination, &entry);
        Path {
            home_departure: delta_to_time(home_departure_delta),
            arrival_time: delta_to_time(arrival_delta),
            total_time: arrival_delta as u32 - home_departure_delta as u32,
            segments,
        }
    }
}

fn relax(
    frontier: &mut Frontier,
    queue: &mut RadixHeapMap<Reverse<u16>, u32>,
    node_id: u32,
    new_entry: Entry,
) {
    if let Some(walk_time) = frontier.nodes[node_id as usize].walk_only_time
        && new_entry.arrival_delta - new_entry.home_departure_delta >= walk_time
    {
        return;
    }
    if let Some(best) = frontier.head_mut(node_id) {
        if new_entry.arrival_delta >= best.arrival_delta {
            return;
        }
        if best.home_departure_delta == new_entry.home_departure_delta {
            // `best` was an entry in the current round, so relax it
            *best = new_entry;
        } else {
            // `best` was an entry with a later home departure time, so add a new frontier entry
            frontier.push(node_id, new_entry);
        }
    } else {
        frontier.push(node_id, new_entry);
    }

    queue.push(Reverse(new_entry.arrival_delta), node_id);
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
        let stop_idx = self.data.node_to_stop[node_id as usize];
        self.data.stops[stop_idx as usize].name.as_str()
    }

    fn compute_destination_stats(&self, frontier: &Frontier, node_id: u32) -> DestinationStats {
        let node_frontier = &frontier.nodes[node_id as usize];
        let walk_time = node_frontier.walk_only_time;

        if !node_frontier.has_head() {
            if let Some(walk_time) = walk_time {
                return DestinationStats {
                    mean_travel_time: walk_time,
                    reachable_fraction: u16::MAX,
                };
            } else {
                return DestinationStats {
                    mean_travel_time: 0,
                    reachable_fraction: 0,
                };
            }
        }

        let time_limit = walk_time
            .map(|t| t as u32)
            .unwrap_or(self.query.max_time + 1); // exclusive limit

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
        for entry in frontier.iter(node_id) {
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

        if let Some(walk) = walk_time {
            // Walk fills every t not claimed by a transit entry above.
            numerator += walk as u64 * (window_length - denominator) as u64;
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

        let stop_idx = self.data.node_to_stop[node as usize];
        if stop_idx == u32::MAX {
            return ControlFlow::Continue(());
        }
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
