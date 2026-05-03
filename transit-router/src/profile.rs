//! Profile routing: Pareto frontier of (arrival, home_departure) per node
//! over a departure-time window. One pass replaces N-sample Dijkstra.
//!
//! # Public interface
//!
//! [`ProfileRouter`] is the contract. [`SplitProfileRouting`] is the only
//! implementation: it splits the departure window into chunks, runs each
//! chunk through the internal [`ProfileRouting`] engine (in parallel when
//! rayon is available), and stitches the per-chunk frontiers into a single
//! [`Isochrone`]. Callers hold `impl ProfileRouter` or `SplitProfileRouting`
//! directly; internal representation is free to change.

use std::{
    cmp::Reverse,
    ops::ControlFlow,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
};

use radix_heap::RadixHeapMap;
use rayon::prelude::*;

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use crate::{data::PreparedData, maybe_par_collect};
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
    /// Number of worker threads used for the profile query's window splitting.
    pub num_threads: u32,
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
    /// `progress` is called from the calling thread with `(done, total)`; return
    /// `ControlFlow::Break(())` from it to cancel the computation. The compute
    /// itself returns `ControlFlow::Break(())` when it observed cancellation
    /// and `ControlFlow::Continue(self)` on a complete run.
    fn compute(
        data: &PreparedData,
        query: &ProfileQuery,
        progress: impl FnMut(usize, usize) -> ControlFlow<()>,
    ) -> ControlFlow<(), Self>;

    /// Per-node isochrone for map rendering.
    fn isochrone(&self) -> &Isochrone;

    /// All Pareto-optimal paths to `destination`, sorted ascending by
    /// `home_departure`. Stop and route names resolved from `data`.
    fn optimal_paths(&self, data: &PreparedData, destination: u32) -> Vec<Path>;
}

// ============================================================================
// Implementation
// ============================================================================

const MIN_SPLIT_CHUNK_SECONDS: u32 = 15 * 60; // 15 minutes

const CHUNKS_PER_THREAD: usize = 1; // Having more chunks than CPU thread count might help load balance against skew (e.g. less transit at night) but also causes more overhead.

const LAST_ENTRY: u32 = u32::MAX;
/// Sentinel for Index::walk_only_time to indicate that a node is not reachable within `query.max_time` by walk.
const WALK_UNREACHABLE: u16 = u16::MAX;
/// Empty-head marker stored in `NodeFrontier::head.entry.home_departure_delta`.
const EMPTY_HEAD_SENTINEL: u16 = u16::MAX;
/// Max time delta that represents a real value
const MAX_DELTA: u16 = u16::MAX - 1;

/// Sentinel `ArenaEntry` for an empty inline head. Signalled by
/// `home_departure_delta == EMPTY_HEAD_SENTINEL`. The sibling pointer is
/// `LAST_ENTRY` so that walking the chain of an empty head is a no-op (used by
/// `iter` paths that don't pre-check).
const EMPTY_ARENA_ENTRY: ArenaEntry = ArenaEntry {
    entry: Entry {
        home_departure_delta: EMPTY_HEAD_SENTINEL,
        arrival_delta: 0,
    },
    sibling_entry_idx: LAST_ENTRY,
};

/// A single entry for a node, representing a Pareto-optimal
/// (home_departure, arrival) pair.
///
/// The predecessor edge is not stored — it is recovered at reconstruction time
/// by `recover_edge`, using the per-pattern reverse arrival index in `Index`.
#[derive(Debug, Copy, Clone)]
struct Entry {
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
/// to in the arena. "No head" is encoded by the sentinel
/// `home_departure_delta == EMPTY_HEAD_SENTINEL` (= `u16::MAX`), not by an
/// `Option` discriminant — that's what gets us to 8 bytes.
///
/// Layout: `ArenaEntry` (8B = Entry 4B + u32), align 4. Verified by the const
/// assert below.
#[derive(Debug)]
struct NodeFrontier {
    /// Most-recent (smallest-arrival) Pareto entry plus its sibling pointer
    /// into the arena tail. Empty iff
    /// `head.entry.home_departure_delta == EMPTY_HEAD_SENTINEL`.
    head: ArenaEntry,
}

impl NodeFrontier {
    #[inline(always)]
    fn has_head(&self) -> bool {
        self.head.entry.home_departure_delta != EMPTY_HEAD_SENTINEL
    }
}

const _: () = assert!(std::mem::size_of::<NodeFrontier>() == 8);
const _: () = assert!(std::mem::size_of::<Entry>() == 4);

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
    #[inline(always)]
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

    #[inline(always)]
    fn head(&self, node_id: u32) -> Option<&Entry> {
        let nf = &self.nodes[node_id as usize];
        if nf.has_head() {
            Some(&nf.head.entry)
        } else {
            None
        }
    }

    #[inline(always)]
    fn head_next(&self, node_id: u32) -> Option<&Entry> {
        let nf = &self.nodes[node_id as usize];
        if nf.head.sibling_entry_idx != LAST_ENTRY {
            Some(&self.arena[nf.head.sibling_entry_idx as usize].entry)
        } else {
            None
        }
    }

    #[inline(always)]
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

#[derive(Debug, Copy, Clone)]
struct DestinationTotals {
    numerator: u64,
    denominator: u32,
}

struct Index {
    /// Inverted index: per global stop_idx, the active patterns that actually
    /// serve that stop (have ≥1 scheduled event or ≥1 frequency entry there).
    /// Replaces the per-pop `O(active_patterns)` scan in `expand_transit_legs`
    /// with `O(patterns_serving_this_stop)`. Populated once per query in `new`.
    patterns_at_stop: Vec<Vec<u32>>,
    /// Walk-only travel time from the source to each node. `WALK_UNREACHABLE`
    /// means not reachable within `query.max_time`.
    walk_only_time: Vec<u16>,
    /// Reverse arrival data per pattern. `pattern_reverse[i] == None` for
    /// inactive patterns. Used only by `recover_edge` during path reconstruction.
    pattern_reverse: Vec<Option<PatternReverse>>,
}

/// Backward chains for a single active pattern, mirroring the forward
/// `next_event_index`/`next_freq_index` pointers. Built once in `Index::new`
/// alongside `patterns_at_stop`.
struct PatternReverse {
    /// Same length as `pat.stop_index.events_by_stop.data`. For event index
    /// `i`, holds the index of the event whose `next_event_index == i`, or
    /// `u32::MAX` if `i` is the first event of its trip (no predecessor).
    event_prev: Vec<u32>,
    /// Same length as `pat.frequency_routes`. For freq index `i`, holds the
    /// index of the freq whose `next_freq_index == i`, or `u32::MAX` if `i`
    /// is the first leg of its trip.
    freq_prev: Vec<u32>,
}

impl PatternReverse {
    fn build(pat: &crate::data::PatternData) -> Self {
        let events = &pat.stop_index.events_by_stop.data;
        let mut event_prev = vec![u32::MAX; events.len()];
        for (i, e) in events.iter().enumerate() {
            if e.next_event_index != u32::MAX {
                event_prev[e.next_event_index as usize] = i as u32;
            }
        }
        let freqs = &pat.frequency_routes;
        let mut freq_prev = vec![u32::MAX; freqs.len()];
        for (i, f) in freqs.iter().enumerate() {
            if f.next_freq_index != u32::MAX {
                freq_prev[f.next_freq_index as usize] = i as u32;
            }
        }
        Self {
            event_prev,
            freq_prev,
        }
    }
}

/// Profile router that transparently splits long departure windows into
/// independent [`ProfileRouting`] subqueries, runs those subqueries in
/// parallel when rayon is available, and merges their outputs behind the same
/// [`ProfileRouter`] contract.
pub struct SplitProfileRouting {
    chunks: Vec<ProfileRouting>,
    isochrone: Isochrone,
}

impl ProfileRouter for SplitProfileRouting {
    fn compute(
        data: &PreparedData,
        query: &ProfileQuery,
        mut progress: impl FnMut(usize, usize) -> ControlFlow<()>,
    ) -> ControlFlow<(), Self> {
        assert!(
            query.window_start <= query.window_end,
            "Time window must have non-negative duration"
        );
        assert!(
            query.max_time < WALK_UNREACHABLE as u32,
            "max_time must fit below the walk-unreachable sentinel"
        );
        assert!(query.max_time >= 1, "max_time must be at least 1 second");

        let chunk_queries = split_profile_query(query);
        let t_index = Instant::now();
        let index = Arc::new(Index::new(data, query));
        let index_ms = t_index.elapsed().as_secs_f64() * 1e3;
        let chunks =
            compute_profile_chunks(data, &chunk_queries, Arc::clone(&index), &mut progress)?;
        let num_threads = get_thread_count().min(chunks.len()).max(1) as u32;
        let t_isochrone = Instant::now();
        let isochrone = compute_isochrone_chunks(data, query, &chunks, num_threads);
        let isochrone_ms = t_isochrone.elapsed().as_secs_f64() * 1e3;
        eprintln!(
            "[profile/split] index_build={:.1}ms compute_isochrone={:.1}ms chunks={}",
            index_ms,
            isochrone_ms,
            chunks.len(),
        );
        ControlFlow::Continue(Self { chunks, isochrone })
    }

    fn isochrone(&self) -> &Isochrone {
        &self.isochrone
    }

    fn optimal_paths(&self, data: &PreparedData, destination: u32) -> Vec<Path> {
        let mut paths: Vec<Path> = Vec::new();
        let mut walk_path: Option<Path> = None;

        let chunk_results =
            maybe_par_collect(&self.chunks, |chunk| chunk.optimal_paths(data, destination));

        for chunk_result in chunk_results {
            for path in chunk_result {
                if path.segments.iter().all(|s| s.kind == SegmentKind::Walk) {
                    if walk_path.is_none() {
                        walk_path = Some(path);
                    }
                    continue;
                }
                if let Some(last) = paths.last()
                    && last.arrival_time == path.arrival_time
                {
                    // Two paths may have the same arrival time
                    // when crossing chunk boundary.
                    // In this case the one from later chunk is
                    // always optimal since
                    // it has a later home departure time.
                    paths.pop();
                }
                paths.push(path);
            }
        }

        if let Some(walk_path) = walk_path {
            paths.push(walk_path);
        }
        paths.sort_by_key(|p| (p.home_departure, p.arrival_time));
        paths
    }
}

fn split_profile_query(query: &ProfileQuery) -> Vec<ProfileQuery> {
    let max_chunk_points = MAX_DELTA as u32 - query.max_time + 1;
    let window_points = query.window_end - query.window_start + 1;
    let min_required_chunks = window_points.div_ceil(max_chunk_points) as usize;
    let max_chunks_by_min_size = (window_points / MIN_SPLIT_CHUNK_SECONDS).max(1) as usize;
    let max_allowed_chunks = max_chunks_by_min_size.max(min_required_chunks);
    let desired_num_chunks = get_thread_count() * CHUNKS_PER_THREAD;
    let chunk_count = desired_num_chunks
        .max(1)
        .clamp(min_required_chunks, max_allowed_chunks)
        .min(window_points as usize);

    let mut chunk_start = query.window_start;
    let mut chunks = Vec::with_capacity(chunk_count);

    for idx in 0..chunk_count {
        let remaining_points = query.window_end - chunk_start + 1;
        let remaining_chunks = chunk_count - idx;
        let chunk_points = remaining_points.div_ceil(remaining_chunks as u32);
        let chunk_end = chunk_start + chunk_points - 1;
        chunks.push(ProfileQuery {
            window_start: chunk_start,
            window_end: chunk_end,
            ..*query
        });

        if chunk_end == query.window_end {
            break;
        }
        chunk_start = chunk_end + 1;
    }

    chunks
}

fn get_thread_count() -> usize {
    if crate::rayon_available() {
        rayon::current_num_threads().max(1)
    } else {
        1
    }
}

/// Progress increments per chunk. Wrapping every chunk's local `(done, total)`
/// onto a shared `[0, PROGRESS_INCREMENTS]` axis lets the wrapper compose chunk progress
/// onto a single `[0, chunk_count * PROGRESS_INCREMENTS]` global stream without exposing
/// per-chunk denominators that would jump under the caller.
const PROGRESS_INCREMENTS: usize = 100;

fn compute_profile_chunks(
    data: &PreparedData,
    chunk_queries: &[ProfileQuery],
    index: Arc<Index>,
    progress: &mut impl FnMut(usize, usize) -> ControlFlow<()>,
) -> ControlFlow<(), Vec<ProfileRouting>> {
    let total = chunk_queries.len();
    if crate::rayon_available() && rayon::current_num_threads() > 1 {
        return compute_profile_chunks_parallel(data, chunk_queries, index, progress);
    }
    // Sequential fallback
    let mut chunks = Vec::with_capacity(total);
    for chunk_query in chunk_queries {
        let result =
            ProfileRouting::compute_with_index(data, chunk_query, Arc::clone(&index), progress)?;
        chunks.push(result);
    }
    ControlFlow::Continue(chunks)
}

fn compute_profile_chunks_parallel(
    data: &PreparedData,
    chunk_queries: &[ProfileQuery],
    index: Arc<Index>,
    progress: &mut impl FnMut(usize, usize) -> ControlFlow<()>,
) -> ControlFlow<(), Vec<ProfileRouting>> {
    let total_chunks = chunk_queries.len();
    let total_units = total_chunks * PROGRESS_INCREMENTS;
    let global_done = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let check_abort = || {
        if aborted.load(Ordering::Relaxed) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let (tx, rx) = mpsc::channel::<()>();

    // `in_place_scope` runs the body on the calling thread (and doesn't
    // require it to be `Send`), unlike `rayon::scope` which may move it.
    // That's exactly what we need: every `progress(...)` call below stays
    // on the thread that entered `compute`.
    let mut results: Vec<ControlFlow<(), ProfileRouting>> = Vec::with_capacity(total_chunks);
    rayon::in_place_scope(|s| {
        let global_done = &global_done;
        let results = &mut results;
        // Moves `tx` into the closure so `rx.recv()` returns `Err` once every
        // worker's clone has been dropped (i.e. all chunks finished).
        s.spawn(move |_| {
            chunk_queries
                .par_iter()
                .map(|query| {
                    check_abort()?;
                    let mut prev: usize = 0;
                    let bump = |new_local: usize, prev: &mut usize| {
                        if new_local > *prev {
                            let delta = new_local - *prev;
                            *prev = new_local;
                            global_done.fetch_add(delta, Ordering::Relaxed);
                            let _ = tx.send(());
                        }
                    };
                    let result = ProfileRouting::compute_with_index(
                        data,
                        query,
                        Arc::clone(&index),
                        &mut |local_done, local_total| {
                            check_abort()?;
                            let local = scale_progress(local_done, local_total);
                            bump(local, &mut prev);
                            ControlFlow::Continue(())
                        },
                    );
                    // Top up so the global counter still reaches total_units even
                    // for chunks where the inner progress closure never fired
                    // (e.g. zero initial transit entries) or stopped short.
                    bump(PROGRESS_INCREMENTS, &mut prev);
                    result
                })
                .collect_into_vec(results);
        });

        // Calling thread: pump wakeups, report progress, observe cancellation.
        while rx.recv().is_ok() {
            if aborted.load(Ordering::Relaxed) {
                continue; // drain remaining wakeups so the loop exits cleanly
            }
            let done = global_done.load(Ordering::Relaxed);
            if progress(done, total_units).is_break() {
                aborted.store(true, Ordering::Relaxed);
            }
        }
    });

    check_abort()?;
    let chunks = results
        .into_iter()
        // All values are Continue if we didn't abort, so unwrap is fine.
        .map(|r| r.continue_value().unwrap())
        .collect();
    ControlFlow::Continue(chunks)
}

#[inline(always)]
fn scale_progress(done: usize, total: usize) -> usize {
    if total == 0 {
        PROGRESS_INCREMENTS
    } else {
        (done * PROGRESS_INCREMENTS / total).min(PROGRESS_INCREMENTS)
    }
}

fn compute_isochrone_chunks(
    data: &PreparedData,
    query: &ProfileQuery,
    chunks: &[ProfileRouting],
    num_threads: u32,
) -> Isochrone {
    let contexts: Vec<_> = chunks
        .iter()
        .map(|chunk| ProfileQueryContext {
            data,
            query: &chunk.query,
            index: &chunk.patterns,
        })
        .collect();
    let total_window_len = query.window_end - query.window_start + 1;
    let stats = maybe_par_collect(0..data.num_nodes, |node_id| {
        let mut total = DestinationTotals {
            numerator: 0,
            denominator: 0,
        };
        for (chunk, context) in chunks.iter().zip(contexts.iter()) {
            let chunk_total = context.compute_destination_totals(&chunk.frontier, node_id as u32);
            total.numerator += chunk_total.numerator;
            total.denominator += chunk_total.denominator;
        }
        destination_totals_to_stats(total, total_window_len)
    });

    Isochrone {
        mean_travel_time: stats.iter().map(|s| s.mean_travel_time).collect(),
        reachable_fraction: stats.iter().map(|s| s.reachable_fraction).collect(),
        num_threads,
        query: *query,
    }
}

fn destination_totals_to_stats(totals: DestinationTotals, window_length: u32) -> DestinationStats {
    let fraction_q = (totals.denominator as u64 * u16::MAX as u64 / window_length as u64) as u16;
    let mean = if totals.denominator > 0 {
        (totals.numerator / totals.denominator as u64).min(u16::MAX as u64) as u16
    } else {
        0
    };
    DestinationStats {
        mean_travel_time: mean,
        reachable_fraction: fraction_q,
    }
}

pub struct ProfileRouting {
    frontier: Frontier,
    query: ProfileQuery,
    patterns: Arc<Index>,
}

impl ProfileRouting {
    fn compute_with_index(
        data: &PreparedData,
        query: &ProfileQuery,
        index: Arc<Index>,
        progress: &mut impl FnMut(usize, usize) -> ControlFlow<()>,
    ) -> ControlFlow<(), Self> {
        assert!(
            query.window_start <= query.window_end,
            "Time window must have non-negative duration"
        );
        assert!(
            query.window_end - query.window_start + query.max_time <= MAX_DELTA as u32,
            "Time values must fit in u16 deltas"
        );
        assert!(
            query.max_time <= MAX_DELTA as u32,
            "max_time must fit in u16 deltas"
        );

        let t_total = Instant::now();

        let n = data.num_nodes;
        let context = ProfileQueryContext {
            data,
            query,
            index: &index,
        };

        let mut frontier = Frontier {
            nodes: (0..n)
                .map(|_| NodeFrontier {
                    head: EMPTY_ARENA_ENTRY,
                })
                .collect(),
            arena: Vec::new(),
        };

        let t_phase1 = Instant::now();

        // ── Phase 1: initial transit boardings from walk-reachable stops ─────
        // - Walk-only travel times are precomputed once in `Index`.
        // - Gather all transit legs available from walk-reachable stops:
        //   departure time in [window_start + walk_time, window_end + walk_time],
        //   i.e. home departure (= transit dep − walk_time) in [window_start, window_end].
        // - Sort the transit legs by home departure time.

        let mut initial_transit_entries: Vec<PendingEntry> = Vec::new();

        // Our workload has monotonic pop so we can use a radix heap instead of a binary heap for better performance.
        let mut queue: RadixHeapMap<Reverse<u16>, u32> = RadixHeapMap::new();
        for (node_id, &walk_time) in index.walk_only_time.iter().enumerate() {
            if walk_time == WALK_UNREACHABLE {
                continue;
            }
            let node_id = node_id as u32;
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
                            // Clamp home departure to be within the query window
                            home_departure_delta: window_length.min(leg.board_delta - walk_time),
                            arrival_delta: leg.arrival_delta,
                        },
                    });
                    ControlFlow::Continue(())
                },
            );
        }

        // Sort by descending home_departure (= transit_departure − walk_time)
        initial_transit_entries.sort_by_key(|x| Reverse(x.entry.home_departure_delta));

        let phase1_ms = t_phase1.elapsed().as_secs_f64() * 1e3;
        let initial_transit_count = initial_transit_entries.len();
        let t_phase2 = Instant::now();

        // ── Phase 2: main profile routing pass ───────────────────────────────

        // Count total chunks for progress reporting
        let total_entries = initial_transit_entries.len();
        let mut entries_done = 0;

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
                    &index,
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
                        home_departure_delta,
                        arrival_delta: new_arrival_delta,
                    };

                    relax(&mut frontier, &index, &mut queue, neighbor, new_entry);
                }

                // Skip unnecessary work if not a transit stop. Under v11,
                // stops occupy indices [0, num_stops); a single compare suffices.
                if (node_id as usize) >= data.num_stops {
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
                } else if let Some(walk_time) = index.walk_time(node_id) {
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
                            &index,
                            &mut queue,
                            leg.node_id,
                            Entry {
                                home_departure_delta,
                                arrival_delta: leg.arrival_delta,
                            },
                        );
                        ControlFlow::Continue(())
                    },
                );
            }
            entries_done += chunk.len();
            progress(entries_done, total_entries)?;
        }
        // Guarantee a terminal tick so wrappers see the chunk cross 100% even
        // when `total_entries == 0` (the loop above never fired) or when the
        // last chunk's `entries_done` already equalled the total before the
        // final progress call.
        progress(total_entries, total_entries)?;

        let phase2_ms = t_phase2.elapsed().as_secs_f64() * 1e3;
        let total_ms = t_total.elapsed().as_secs_f64() * 1e3;
        eprintln!(
            "[profile] phase1(initial)={:.1}ms phase2(transfer)={:.1}ms total={:.1}ms initial_transit_entries={}",
            phase1_ms, phase2_ms, total_ms, initial_transit_count,
        );

        ControlFlow::Continue(Self {
            frontier,
            query: *query,
            patterns: index,
        })
    }

    fn optimal_paths(&self, data: &PreparedData, destination: u32) -> Vec<Path> {
        let context = ProfileQueryContext {
            data,
            query: &self.query,
            index: &self.patterns,
        };
        let entries: Vec<Option<Entry>> = self
            .frontier
            .iter(destination)
            .map(|entry| Some(*entry))
            .chain(self.patterns.walk_time(destination).map(|_| None))
            .collect();
        maybe_par_collect(entries, |entry| {
            self.reconstruct_path(&context, destination, entry)
        })
    }

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
        let delta_to_time = |delta: u16| delta as u32 + self.query.window_start;
        let get_true_arrival_delta = |node: u32, entry: &Option<Entry>| {
            if let Some(entry) = entry {
                entry.arrival_delta
            } else {
                // Walk-only entry, arrival delta is just departure delta + walk time
                home_departure_delta + self.patterns.walk_time(node).unwrap()
            }
        };

        let mut segments: Vec<PathSegment> = Vec::new();

        let source = self.query.source_node;
        let mut curr_node = destination;
        let mut curr = entry;
        while curr_node != source {
            let recovery = context.recover_edge(&self.frontier, curr_node, curr);
            let prev_node = recovery.prev_node;
            let prev = recovery.prev_entry;
            let prev_arrival_delta = get_true_arrival_delta(prev_node, &prev);

            match recovery.kind {
                EdgeKind::Walk => {
                    if let Some(segment) = segments.last_mut()
                        && segment.kind == SegmentKind::Walk
                    {
                        // Merge into the in-progress walk segment.
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
                            // Walk node_sequence is built in reverse-traversal
                            // order; flipped back to forward order at end.
                            node_sequence: vec![curr_node, prev_node],
                        });
                    }
                }
                EdgeKind::Transit { leg } => {
                    let pat = &context.data.patterns[leg.pattern_idx as usize];
                    let mut node_sequence = Vec::new();
                    let end_stop = context
                        .data
                        .node_to_stop(curr_node)
                        .expect("curr_node is a stop");
                    let route_index = match leg.transit_ref {
                        TransitRef::Scheduled { event_idx } => {
                            let events = &pat.stop_index.events_by_stop.data;
                            let mut curr_event_idx = event_idx;
                            let mut reached_end_stop = false;
                            let route_index = loop {
                                let event = &events[curr_event_idx as usize];
                                if !reached_end_stop {
                                    node_sequence.push(context.data.stop_to_node(event.stop_index));
                                }
                                if event.stop_index == end_stop {
                                    reached_end_stop = true;
                                }
                                if event.next_event_index == u32::MAX {
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
                                node_sequence.push(context.data.stop_to_node(freq.stop_index));
                                if freq.next_stop_index == end_stop {
                                    // Last hop: alighting stop only appears as
                                    // `next_stop_index`, never as a subsequent
                                    // `stop_index`.
                                    node_sequence
                                        .push(context.data.stop_to_node(freq.next_stop_index));
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

        if let Some(first) = segments.first() {
            assert!(
                first.start_time == delta_to_time(home_departure_delta),
                "First segment start time {} does not match home departure time {}",
                first.start_time,
                delta_to_time(home_departure_delta)
            );
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

#[inline(always)]
fn relax(
    frontier: &mut Frontier,
    index: &Index,
    queue: &mut RadixHeapMap<Reverse<u16>, u32>,
    node_id: u32,
    new_entry: Entry,
) {
    let walk_time = index.walk_only_time[node_id as usize];
    if walk_time != WALK_UNREACHABLE
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
        assert!(
            query.max_time < WALK_UNREACHABLE as u32,
            "max_time must fit below the walk-unreachable sentinel"
        );
        assert!(query.max_time >= 1, "max_time must be at least 1 second");
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
        let mut pattern_reverse: Vec<Option<PatternReverse>> =
            (0..data.patterns.len()).map(|_| None).collect();
        for &pat_idx in &active_patterns {
            pattern_reverse[pat_idx] = Some(PatternReverse::build(&data.patterns[pat_idx]));
        }
        let walk_only_time = compute_walk_only_times(data, query);
        Self {
            patterns_at_stop,
            walk_only_time,
            pattern_reverse,
        }
    }

    #[inline(always)]
    fn walk_time(&self, node_id: u32) -> Option<u16> {
        let walk_time = self.walk_only_time[node_id as usize];
        (walk_time != WALK_UNREACHABLE).then_some(walk_time)
    }
}

fn compute_walk_only_times(data: &PreparedData, query: &ProfileQuery) -> Vec<u16> {
    let mut walk_only_time = vec![WALK_UNREACHABLE; data.num_nodes];
    let mut queue: RadixHeapMap<Reverse<u16>, u32> = RadixHeapMap::new();
    walk_only_time[query.source_node as usize] = 0;
    queue.push(Reverse(0u16), query.source_node);

    while let Some((Reverse(walk_time), node_id)) = queue.pop() {
        if walk_time != walk_only_time[node_id as usize] {
            continue;
        }

        for &(neighbor, edge_walk_time) in &data.adj[node_id] {
            let new_walk_time = walk_time.saturating_add(edge_walk_time);

            if new_walk_time == WALK_UNREACHABLE || new_walk_time as u32 > query.max_time {
                continue;
            }

            let existing = &mut walk_only_time[neighbor as usize];
            if new_walk_time >= *existing {
                continue;
            }

            *existing = new_walk_time;
            queue.push(Reverse(new_walk_time), neighbor);
        }
    }

    walk_only_time
}

impl<'a> ProfileQueryContext<'a> {
    fn get_stop_name(&self, node_id: u32) -> &'a str {
        let stop_idx = self
            .data
            .node_to_stop(node_id)
            .expect("get_stop_name called on non-stop node");
        self.data.stops[stop_idx as usize].name.as_str()
    }

    #[inline(always)]
    fn compute_destination_totals(&self, frontier: &Frontier, node_id: u32) -> DestinationTotals {
        // This function is called on a very hot loop
        let node_frontier = &frontier.nodes[node_id as usize];
        let walk_time = self.index.walk_time(node_id);

        if !node_frontier.has_head() {
            if let Some(walk_time) = walk_time {
                let window_length = self.query.window_end - self.query.window_start + 1;
                return DestinationTotals {
                    numerator: walk_time as u64 * window_length as u64,
                    denominator: window_length,
                };
            } else {
                return DestinationTotals {
                    numerator: 0,
                    denominator: 0,
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
        let mut numerator: u32 = 0; // u32 is safe since the full area of the plot is at most window_size (u16) * max_time (u16)
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
            numerator += reachable * travel;
            numerator += (reachable * (reachable - 1)) / 2;

            prev_departure = departure as i32;
        }

        if let Some(walk) = walk_time {
            // Walk fills every t not claimed by a transit entry above.
            numerator += walk as u32 * (window_length - denominator);
            denominator = window_length;
        }

        DestinationTotals {
            numerator: numerator as u64,
            denominator,
        }
    }

    /// For each transit vehicle departing `node` within `[min_departure, max_departure]`,
    /// follow the trip forward and emit one [`TransitLeg`] per downstream stop, with
    /// `arrival_delta` filled in. The boarding stop (`node`) is implicit in `transit_ref`
    /// for path reconstruction. Covers both scheduled and frequency-based routes.
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

        let Some(stop_idx) = self.data.node_to_stop(node) else {
            return ControlFlow::Continue(());
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
                        node_id: self.data.stop_to_node(next.stop_index),
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
                            node_id: self.data.stop_to_node(f.next_stop_index),
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

    /// Recover the predecessor edge for `(curr_node, curr)` during path
    /// reconstruction. With `prev` no longer stored on `Entry`, we search:
    ///   1. walk neighbour with an Entry at the same `home_departure_delta`
    ///      whose arrival + edge_walk matches (non-initial walk edge);
    ///   2. transit legs arriving at `curr_node` at `curr.arrival_delta`,
    ///      walking the trip's reverse chain to find a boarding stop with
    ///      either an Entry at `H` (transfer) or `walk_only_time` consistent
    ///      with initial-walk boarding;
    ///   3. (when `curr` is `None`) walk-only neighbour whose
    ///      `walk_only_time + edge_walk` matches.
    fn recover_edge(
        &self,
        frontier: &Frontier,
        curr_node: u32,
        curr: Option<Entry>,
    ) -> EdgeRecovery {
        let data = self.data;
        let index = self.index;

        // ── Case 3: walk-only tail ──────────────────────────────────────────
        let Some(curr) = curr else {
            let curr_walk_time = index
                .walk_time(curr_node)
                .expect("walk-only-tail recovery on a node without walk_only_time");
            let &(prev_node, _) = data.adj[curr_node]
                .iter()
                .find(|&&(neighbor, edge_walk_time)| {
                    let Some(walk_time) = index.walk_time(neighbor) else {
                        return false;
                    };
                    walk_time.saturating_add(edge_walk_time) == curr_walk_time
                })
                .unwrap_or_else(|| {
                    panic!(
                        "No walk predecessor for walk-only node {} (walk_time={})",
                        curr_node, curr_walk_time
                    )
                });
            return EdgeRecovery {
                prev_node,
                prev_entry: None,
                kind: EdgeKind::Walk,
            };
        };

        let h = curr.home_departure_delta;
        let a = curr.arrival_delta;

        // ── Case 1: non-initial walk edge ──────────────────────────────────
        for &(neighbor, edge_walk_time) in &data.adj[curr_node] {
            if let Some(prev_entry) = frontier
                .iter(neighbor)
                .find(|e| e.home_departure_delta == h)
                .copied()
                && prev_entry.arrival_delta.saturating_add(edge_walk_time) == a
            {
                return EdgeRecovery {
                    prev_node: neighbor,
                    prev_entry: Some(prev_entry),
                    kind: EdgeKind::Walk,
                };
            }
        }

        // ── Case 2: transit leg ────────────────────────────────────────────
        if let Some(rec) = self.recover_transit_leg(frontier, curr_node, h, a) {
            return rec;
        }

        panic!(
            "Failed to recover edge for node {} (H={}, A={})",
            curr_node, h, a
        );
    }

    fn recover_transit_leg(
        &self,
        frontier: &Frontier,
        curr_node: u32,
        h: u16,
        a: u16,
    ) -> Option<EdgeRecovery> {
        let data = self.data;
        let index = self.index;
        let query = self.query;
        let window_start = query.window_start;
        let transfer_slack = query.transfer_slack;
        let target_arrival_abs = window_start + a as u32;
        let curr_stop = data.node_to_stop(curr_node)?;

        // Decide whether `B` boarded at `board_time` is a valid predecessor.
        // Returns `Some(Some(entry))` for transfer (Case 2a), `Some(None)`
        // for initial-walk boarding (Case 2b), or `None` if neither holds.
        let check_boarding = |board_node: u32, board_time: u32| -> Option<Option<Entry>> {
            // Case 2(a): transfer — `B` already has an Entry at H, with enough
            // slack between its arrival and the boarding event.
            if let Some(b_entry) = frontier
                .iter(board_node)
                .find(|e| e.home_departure_delta == h)
                .copied()
            {
                let b_arrival_abs = window_start + b_entry.arrival_delta as u32;
                if b_arrival_abs + transfer_slack <= board_time {
                    return Some(Some(b_entry));
                }
            }
            // Case 2(b): initial-walk boarding — `B` reached purely by walking
            // from source. Feasibility (not exact reproduction): leaving home
            // at `window_start + H`, walking `walk_only(B)` seconds, and
            // waiting if needed all fits inside the leg's boarding time. The
            // displayed `home_departure` stays at `H`; any slack becomes wait.
            if let Some(b_walk) = index.walk_time(board_node) {
                if board_time < window_start {
                    return None;
                }
                let board_delta = board_time - window_start;
                if b_walk as u32 + h as u32 <= board_delta {
                    return Some(None);
                }
            }
            None
        };

        // ── Scheduled events ───────────────────────────────────────────────
        for &pat_idx in &index.patterns_at_stop[curr_stop as usize] {
            let pat = &data.patterns[pat_idx as usize];
            let pat_rev = index.pattern_reverse[pat_idx as usize]
                .as_ref()
                .expect("active pattern has reverse data");
            let events_data = &pat.stop_index.events_by_stop.data;
            let off_lo = pat.stop_index.events_by_stop.offsets[curr_stop as usize] as usize;
            let off_hi = pat.stop_index.events_by_stop.offsets[curr_stop as usize + 1] as usize;
            for arr_idx in off_lo..off_hi {
                let prev_idx = pat_rev.event_prev[arr_idx];
                if prev_idx == u32::MAX {
                    continue; // Trip starts here — not an arrival.
                }
                let prev_event = &events_data[prev_idx as usize];
                let arrival_time = prev_event.time_offset + prev_event.travel_time;
                if arrival_time != target_arrival_abs {
                    continue;
                }
                // Walk backward from the arrival's predecessor through the
                // trip chain. Each visited event is a candidate boarding.
                let mut k_idx = prev_idx;
                loop {
                    let k_event = &events_data[k_idx as usize];
                    let board_node = data.stop_to_node(k_event.stop_index);
                    let board_time = k_event.time_offset;
                    if let Some(prev_entry) = check_boarding(board_node, board_time) {
                        let leg = TransitLeg {
                            node_id: curr_node,
                            board_delta: (board_time - window_start) as u16,
                            arrival_delta: a,
                            pattern_idx: pat_idx as u16,
                            transit_ref: TransitRef::Scheduled { event_idx: k_idx },
                        };
                        return Some(EdgeRecovery {
                            prev_node: board_node,
                            prev_entry,
                            kind: EdgeKind::Transit { leg },
                        });
                    }
                    let earlier = pat_rev.event_prev[k_idx as usize];
                    if earlier == u32::MAX {
                        break;
                    }
                    k_idx = earlier;
                }
            }
        }

        // ── Frequency-based legs ───────────────────────────────────────────
        for &pat_idx in &index.patterns_at_stop[curr_stop as usize] {
            let pat = &data.patterns[pat_idx as usize];
            let pat_rev = index.pattern_reverse[pat_idx as usize]
                .as_ref()
                .expect("active pattern has reverse data");
            let freqs = &pat.frequency_routes;
            for (i, fi) in freqs.iter().enumerate() {
                if fi.next_stop_index != curr_stop || fi.travel_time == 0 {
                    continue;
                }
                let mut chain_idx = i as u32;
                let mut cumulative_after: u32 = fi.travel_time;
                loop {
                    let curr_freq = &freqs[chain_idx as usize];
                    if curr_freq.travel_time == 0 {
                        break;
                    }
                    if (target_arrival_abs as u64) < cumulative_after as u64 {
                        break;
                    }
                    let board_time = target_arrival_abs - cumulative_after;
                    let valid = board_time >= curr_freq.start_time
                        && board_time < curr_freq.end_time
                        && (board_time - curr_freq.start_time) % curr_freq.headway_secs == 0;
                    if valid {
                        let board_node = data.stop_to_node(curr_freq.stop_index);
                        if let Some(prev_entry) = check_boarding(board_node, board_time) {
                            let leg = TransitLeg {
                                node_id: curr_node,
                                board_delta: (board_time - window_start) as u16,
                                arrival_delta: a,
                                pattern_idx: pat_idx as u16,
                                transit_ref: TransitRef::Frequency {
                                    freq_idx: chain_idx,
                                },
                            };
                            return Some(EdgeRecovery {
                                prev_node: board_node,
                                prev_entry,
                                kind: EdgeKind::Transit { leg },
                            });
                        }
                    }
                    let earlier = pat_rev.freq_prev[chain_idx as usize];
                    if earlier == u32::MAX {
                        break;
                    }
                    let prev_freq = &freqs[earlier as usize];
                    if prev_freq.travel_time == 0 {
                        break;
                    }
                    cumulative_after = cumulative_after.saturating_add(prev_freq.travel_time);
                    chain_idx = earlier;
                }
            }
        }

        None
    }
}

#[derive(Debug)]
struct EdgeRecovery {
    prev_node: u32,
    prev_entry: Option<Entry>,
    kind: EdgeKind,
}

#[derive(Debug)]
enum EdgeKind {
    Walk,
    Transit { leg: TransitLeg },
}
