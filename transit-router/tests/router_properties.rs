//! Behavior-level property tests for the profile router. Each test is a
//! metamorphic relation or self-consistency check expressed entirely against
//! the public [`transit_router::Router`] / [`transit_router::Isochrone`]
//! surface — `entries`, `mean_travel_time`, `reachable_fraction`, `paths`.
//!
//! `split_matches_single_pass` is the trickiest: it asserts the parallel
//! split-and-merge path produces byte-identical output to the unsplit one.
//! Both runs go through `Router::isochrone`; the unsplit run sets
//! `IsochroneParams::max_parallelism = Some(1)` to collapse the window into
//! a single chunk.
//!
//! Each test runs `ITERS` times against a fresh source picked by
//! `baseline_query` — see `tests/common/mod.rs` for the temperature-biased
//! anchor + walk-radius algorithm. An `eprintln!` at the top of every
//! iteration prints the source node, so a failing test's captured stderr
//! tells you which seed reproduces the failure (`ROUTER_TEST_SEED=...`).

mod common;

use std::collections::HashSet;

use chrono::Duration as ChronoDuration;
use transit_router::{IsochroneParams, NodeId, SegmentKind};

use common::{
    baseline_query, iso_entries, load_fixture, router_fixture, run_iso,
    run_iso_with_max_parallelism, sample_reachable_stratified_iso, walk_times_from,
};

/// Iterations per property test. Each iteration runs against a different
/// random source. Override with `ROUTER_TEST_ITERS=<n>` for local sweeps.
fn iters() -> u32 {
    std::env::var("ROUTER_TEST_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

const SAMPLE_SIZE: usize = 50;
const SAMPLE_SEED: u64 = 0x_5A_5A_5A_5A_5A_5A_5A_5A;

fn iter_id(name: &str, iter: u32) -> String {
    format!("{name}#{iter}")
}

#[test]
fn determinism_isochrone_and_entries() {
    let data = load_fixture();
    let router = router_fixture();
    for iter in 0..iters() {
        let p = baseline_query(data, &iter_id("determinism", iter));
        eprintln!("[determinism iter {iter}] source={}", p.source.get());

        let i1 = run_iso(router, &p);
        let i2 = run_iso(router, &p);

        assert_eq!(i1.mean_travel_time(), i2.mean_travel_time());
        assert_eq!(i1.reachable_fraction(), i2.reachable_fraction());

        for v in 0..i1.reachable_fraction().len() as u32 {
            if i1.reachable_fraction()[v as usize] == 0 {
                continue;
            }
            let e1 = iso_entries(&i1, v);
            let e2 = iso_entries(&i2, v);
            assert_eq!(e1, e2, "iter {iter} node {v}: entries differ");
        }
    }
}

// Phase 2 processes initial entries in descending home_departure order
// (profile.rs ~line 855), so for any node the chain's home_departure values
// are strictly decreasing as new entries push to head. Combined with the
// relax invariant `a_new < best.a`, the chain is strict-Pareto: walking from
// head outward, both home_departure and arrival strictly increase.
//
// Therefore: for an in-budget journey D found by T-search, every intermediate
// has travel ≤ travel(D) ≤ T (so the walk-relax cutoff doesn't prune it), and
// the entry survives in the strict-Pareto frontier. T+Δ-search reaches the
// same set of in-budget journeys plus more, so entries(T) ⊆ entries(T+Δ),
// and any extra has travel > T.
#[test]
fn max_time_monotonicity() {
    let data = load_fixture();
    let router = router_fixture();
    for iter in 0..iters() {
        let base = baseline_query(data, &iter_id("max_time_monotonicity", iter));
        eprintln!(
            "[max_time_monotonicity iter {iter}] source={}",
            base.source.get()
        );
        let small_secs: u32 = 30 * 60;
        let large_secs: u32 = 45 * 60;
        let p_small = IsochroneParams {
            max_time: ChronoDuration::seconds(small_secs as i64),
            ..base.clone()
        };
        let p_large = IsochroneParams {
            max_time: ChronoDuration::seconds(large_secs as i64),
            ..base
        };

        let i_small = run_iso(router, &p_small);
        let i_large = run_iso(router, &p_large);

        let n = data.num_nodes as u32;
        for v in 0..n {
            let small: HashSet<(u32, u32)> = iso_entries(&i_small, v).into_iter().collect();
            let large: HashSet<(u32, u32)> = iso_entries(&i_large, v).into_iter().collect();

            for entry in &small {
                assert!(
                    large.contains(entry),
                    "iter {iter} node {v}: entry {:?} present in T={}s but missing in T={}s",
                    entry,
                    small_secs,
                    large_secs,
                );
            }
            for &(h, a) in large.difference(&small) {
                let travel = a - h;
                assert!(
                    travel > small_secs,
                    "iter {iter} node {v}: extra entry ({h},{a}) has travel {travel} ≤ T_small={small_secs}",
                );
            }
        }
    }
}

#[test]
fn slack_monotonicity() {
    let data = load_fixture();
    let router = router_fixture();
    for iter in 0..iters() {
        let base = baseline_query(data, &iter_id("slack_monotonicity", iter));
        eprintln!(
            "[slack_monotonicity iter {iter}] source={}",
            base.source.get()
        );
        let slacks_secs = [0i64, 60];

        let runs: Vec<_> = slacks_secs
            .iter()
            .map(|&s| {
                let p = IsochroneParams {
                    transfer_slack: ChronoDuration::seconds(s),
                    ..base.clone()
                };
                run_iso(router, &p)
            })
            .collect();

        for w in runs.windows(2) {
            let small = w[0].reachable_fraction();
            let large = w[1].reachable_fraction();
            for v in 0..small.len() {
                assert!(
                    large[v] <= small[v],
                    "iter {iter} node {v}: fraction grew from slack=small ({}) to larger slack ({})",
                    small[v],
                    large[v],
                );
            }
        }
    }
}

// Cross-check the parallel split-and-merge path against the unsplit path.
// Same `Router::isochrone` entry point twice; the second run uses
// `max_parallelism: Some(1)` to collapse the window to a single chunk so the
// engine's merge logic is short-circuited.
#[test]
fn split_matches_single_pass() {
    let router = router_fixture();
    let data = load_fixture();
    for iter in 0..iters() {
        let p = baseline_query(data, &iter_id("split_matches_single", iter));
        eprintln!(
            "[split_matches_single iter {iter}] source={}",
            p.source.get()
        );

        let split = run_iso(router, &p);
        let single = run_iso_with_max_parallelism(router, &p, 1);

        assert_eq!(split.mean_travel_time(), single.mean_travel_time());
        assert_eq!(split.reachable_fraction(), single.reachable_fraction());

        for v in 0..split.reachable_fraction().len() as u32 {
            let e_split = iso_entries(&split, v);
            let e_single = iso_entries(&single, v);
            assert_eq!(
                e_split, e_single,
                "iter {iter} node {v}: split vs single entries differ",
            );
        }
    }
}

#[test]
fn itinerary_self_consistency() {
    let data = load_fixture();
    let router = router_fixture();
    for iter in 0..iters() {
        let p = baseline_query(data, &iter_id("itinerary", iter));
        eprintln!("[itinerary iter {iter}] source={}", p.source.get());
        let iso = run_iso(router, &p);
        // Filter out the source itself: source==dest is a trivial zero-length
        // journey that `optimal_paths` legitimately returns as a Path with
        // empty `segments`, which would trip the structural assertions below.
        let source_idx = p.source.get();
        let sample: Vec<u32> = sample_reachable_stratified_iso(&iso, SAMPLE_SIZE, SAMPLE_SEED)
            .into_iter()
            .filter(|&v| v != source_idx)
            .collect();
        assert!(
            !sample.is_empty(),
            "iter {iter}: no non-trivial reachable destinations"
        );

        for &dest in &sample {
            let paths = iso.paths(NodeId(dest));
            assert!(
                !paths.is_empty(),
                "iter {iter} node {dest} marked reachable but no paths"
            );

            // Per-path checks.
            for (i, path) in paths.iter().enumerate() {
                assert_eq!(
                    path.arrival_time - path.home_departure,
                    path.total_time,
                    "iter {iter} node {dest} path {i}: total_time mismatch",
                );

                let segs = &path.segments;
                assert!(
                    !segs.is_empty(),
                    "iter {iter} node {dest} path {i}: empty segments"
                );

                for s in segs {
                    match s.kind {
                        SegmentKind::Walk => {
                            assert_eq!(s.wait_time, 0, "walk wait_time != 0");
                            assert!(s.route_index.is_none(), "walk has route_index");
                            assert!(s.node_sequence.len() >= 2, "walk node_sequence < 2");
                        }
                        SegmentKind::Transit => {
                            assert!(
                                s.route_index.is_some(),
                                "transit segment missing route_index",
                            );
                            assert!(s.node_sequence.len() >= 2, "transit node_sequence < 2");
                        }
                    }
                }

                for w in segs.windows(2) {
                    let (a, b) = (&w[0], &w[1]);
                    assert!(
                        b.start_time >= a.end_time,
                        "iter {iter} node {dest} path {i}: non-contiguous times {} < {}",
                        b.start_time,
                        a.end_time,
                    );
                    assert_eq!(
                        *a.node_sequence.last().unwrap(),
                        *b.node_sequence.first().unwrap(),
                        "iter {iter} node {dest} path {i}: node-sequence break at segment boundary",
                    );
                }

                // No degenerate node sequences (a node repeated immediately).
                for s in segs {
                    for w in s.node_sequence.windows(2) {
                        assert_ne!(
                            w[0], w[1],
                            "iter {iter} node {dest} path {i}: node_sequence has repeated node {}",
                            w[0],
                        );
                    }
                }
            }

            // Pareto invariant on the transit-only subset (the trailing walk-only
            // path may not satisfy domination since the router emits it
            // unconditionally as a baseline).
            let transit_paths: Vec<_> = paths
                .iter()
                .filter(|p| p.segments.iter().any(|s| s.kind == SegmentKind::Transit))
                .collect();
            for w in transit_paths.windows(2) {
                let (a, b) = (w[0], w[1]);
                assert!(
                    b.home_departure > a.home_departure && b.arrival_time > a.arrival_time,
                    "iter {iter} node {dest}: transit paths not Pareto-sorted",
                );
            }

            // Cross-check vs entries(). The set of (home_dep, arrival) projected
            // from transit paths must equal the entries() set.
            let path_set: HashSet<(u32, u32)> = transit_paths
                .iter()
                .map(|p| (p.home_departure, p.arrival_time))
                .collect();
            let entry_set: HashSet<(u32, u32)> = iso_entries(&iso, dest).into_iter().collect();
            assert_eq!(
                path_set, entry_set,
                "iter {iter} node {dest}: transit-paths frontier disagrees with entries()",
            );
        }
    }
}

#[test]
fn mean_and_fraction_by_exact_integration() {
    let data = load_fixture();
    let router = router_fixture();
    let iter = 0; // each iteration is kinda expensive so just do once
    let p = baseline_query(data, &iter_id("integration", iter));
    eprintln!("[integration iter {iter}] source={}", p.source.get());
    let iso = run_iso(router, &p);
    let sample = sample_reachable_stratified_iso(&iso, SAMPLE_SIZE, SAMPLE_SEED);
    assert!(!sample.is_empty(), "iter {iter}: empty sample");

    // Pick the node with the most entries; ties broken by smallest index.
    let dest = *sample
        .iter()
        .max_by_key(|&&v| (iso_entries(&iso, v).len(), std::cmp::Reverse(v)))
        .expect("non-empty sample");
    let entries = iso_entries(&iso, dest);
    assert!(
        !entries.is_empty(),
        "iter {iter}: richest node has no entries"
    );

    // Independent walk-only time at dest via plain Dijkstra over the public
    // walk graph (capped to max_time so we know whether walk is in the budget).
    let max_time_secs = p.max_time.num_seconds().max(0) as u32;
    let window_start = p.window.start.as_seconds();
    let window_end = p.window.end.as_seconds();
    let walk_times = walk_times_from(data, p.source.get(), max_time_secs);
    let walk_at_dest = walk_times[dest as usize];
    let walk_in_budget = (walk_at_dest != u32::MAX).then_some(walk_at_dest);

    // Iterate every integer second of the home-departure window. For each t,
    // best travel = min(transit_at_t, walk_at_dest), each clamped to max_time.
    let mut numerator: u64 = 0;
    let mut count: u64 = 0;
    for t in window_start..=window_end {
        let mut best = u32::MAX;
        if let Some(walk) = walk_in_budget {
            if walk <= max_time_secs {
                best = best.min(walk);
            }
        }
        // Smallest entry with home_departure ≥ t.
        let idx = entries.partition_point(|e: &(u32, u32)| e.0 < t);
        if idx < entries.len() {
            let (_h, a) = entries[idx];
            if a >= t {
                let trav = a - t;
                if trav <= max_time_secs {
                    best = best.min(trav);
                }
            }
        }
        if best != u32::MAX {
            numerator += best as u64;
            count += 1;
        }
    }

    let window_len = (window_end - window_start + 1) as u64;
    let expected_mean: u16 = if count > 0 {
        (numerator / count).min(u16::MAX as u64) as u16
    } else {
        0
    };
    let expected_frac: u16 = (count * u16::MAX as u64 / window_len) as u16;

    let actual_mean = iso.mean_travel_time()[dest as usize];
    let actual_frac = iso.reachable_fraction()[dest as usize];

    assert_eq!(
        expected_mean, actual_mean,
        "iter {iter} node {dest} mean_travel_time mismatch: expected {expected_mean}, got {actual_mean}",
    );
    assert_eq!(
        expected_frac, actual_frac,
        "iter {iter} node {dest} reachable_fraction mismatch: expected {expected_frac}, got {actual_frac}",
    );
}
