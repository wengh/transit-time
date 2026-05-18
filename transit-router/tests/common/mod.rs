//! Shared helpers for `transit-router` integration tests. Loads a small
//! real-city fixture once per test process, then provides deterministic
//! source-selection primitives (event-count weights, temperature sampling,
//! walk-radius node picks) for building diverse but reproducible queries.

use std::io::Read;
use std::ops::ControlFlow;
use std::sync::{Arc, OnceLock};

use chrono::{Duration as ChronoDuration, NaiveDate};
use flate2::read::GzDecoder;
use rand::{RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;

use transit_router::data::{self, PreparedData};
use transit_router::router::{patterns_for_date, snap_to_node};
use transit_router::{Isochrone, IsochroneParams, NodeId, Router, SinceMidnight, TimeWindow};

/// Path to the fixture `.bin` file. Selected by `ROUTER_TEST_CITY`
/// (default: `chicago`).
fn fixture_path() -> &'static str {
    static ONCE: OnceLock<String> = OnceLock::new();
    ONCE.get_or_init(|| {
        let city = std::env::var("ROUTER_TEST_CITY").unwrap_or_else(|_| "chicago".to_string());
        format!("../transit-viz/public/data/{city}.bin")
    })
    .as_str()
}

/// Per-process RNG seed. Setting `ROUTER_TEST_SEED` pins it for reproducing
/// failures; leaving it unset generates a fresh random seed each run.
fn run_seed() -> u64 {
    static ONCE: OnceLock<u64> = OnceLock::new();
    *ONCE.get_or_init(|| {
        std::env::var("ROUTER_TEST_SEED")
            .map(|s| s.parse().unwrap())
            .unwrap_or_else(|_| rand::random())
    })
}

// =============================================================================
// Fixture & date discovery
// =============================================================================

pub fn load_fixture() -> &'static PreparedData {
    static ONCE: OnceLock<PreparedData> = OnceLock::new();
    ONCE.get_or_init(|| {
        let path = fixture_path();
        let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let bytes: Vec<u8> = if raw.starts_with(&[0x1f, 0x8b]) {
            let mut decoder = GzDecoder::new(&raw[..]);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).expect("gunzip fixture");
            out
        } else {
            raw
        };
        data::load(&bytes).expect("parse fixture")
    })
}

/// Date the test queries run against. Defaults to today in local time.
/// Override via `ROUTER_TEST_DATE=YYYYMMDD` to reproduce a specific failure.
pub fn test_date() -> NaiveDate {
    static ONCE: OnceLock<NaiveDate> = OnceLock::new();
    *ONCE.get_or_init(|| {
        if let Ok(s) = std::env::var("ROUTER_TEST_DATE") {
            let v: u32 = s.parse().unwrap_or_else(|e| {
                panic!("ROUTER_TEST_DATE={s:?} is not a YYYYMMDD integer: {e}")
            });
            NaiveDate::from_ymd_opt((v / 10_000) as i32, (v / 100) % 100, v % 100)
                .unwrap_or_else(|| panic!("ROUTER_TEST_DATE={s:?} is not a valid calendar date"))
        } else {
            chrono::Local::now().date_naive()
        }
    })
}

// =============================================================================
// Query construction
// =============================================================================

/// Baseline [`IsochroneParams`] with a temperature-biased source. The RNG
/// seed is `run_seed() ⊕ hash(test_id)` — `run_seed()` returns a fresh
/// random value per-process by default (logged so failures are
/// reproducible) or the value of `ROUTER_TEST_SEED` if set.
pub fn baseline_query(data: &PreparedData, test_id: &str) -> IsochroneParams {
    let date = test_date();
    let window_start = 9 * 3600; // 09:00
    let window_end = 10 * 3600; //  10:00
    let max_time = 45 * 60;
    let slack = 60;

    let seed = run_seed();
    let city = std::env::var("ROUTER_TEST_CITY").unwrap_or_else(|_| "chicago".to_string());
    eprintln!(
        "[router_tests] repro: ROUTER_TEST_CITY={city} ROUTER_TEST_DATE={} ROUTER_TEST_SEED={seed}",
        date.format("%Y%m%d"),
    );
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed ^ fnv1a(test_id));

    // 1. Stop-event weights for the chosen window. Memoized so the
    //    multi-iter test loops don't rebuild this O(num_events) vector
    //    once per source.
    static WEIGHTS: OnceLock<Vec<u32>> = OnceLock::new();
    let weights = WEIGHTS.get_or_init(|| {
        let w = stop_event_weights(data, date, window_start, window_end);
        assert!(
            w.iter().any(|&v| v > 0),
            "no stops have events in window — fixture/date mismatch"
        );
        w
    });

    // 2. Temperature-sample an anchor stop from stops with at least 10% of the
    //    busiest stop's event count.
    let max_weight = *weights.iter().max().unwrap();
    let threshold = (max_weight / 10).max(1);
    let top_p_weights: Vec<u32> = weights
        .iter()
        .map(|&w| if w >= threshold { w } else { 0 })
        .collect();
    let anchor_stop = temperature_sample(&top_p_weights, 0.9, &mut rng);
    let stop = &data.stops[anchor_stop];

    // 3. Snap stop's lat/lon to a node, then random-walk-within-15-min from it.
    let anchor_node =
        snap_to_node(data, stop.lat, stop.lon).expect("anchor stop has no nearby node");
    let source_node = random_node_within_walk(data, anchor_node, 15 * 60, &mut rng);
    let src = &data.nodes[source_node as usize];
    let total_weight: u32 = top_p_weights.iter().sum();
    eprintln!(
        "[router_tests] anchor={:?} ({:.5},{:.5}); weight={} threshold={} total={} max={}; source={source_node} ({:.5},{:.5})",
        stop.name,
        stop.lat,
        stop.lon,
        weights[anchor_stop],
        threshold,
        total_weight,
        max_weight,
        src.lat,
        src.lon,
    );

    IsochroneParams {
        source: NodeId(source_node),
        date,
        window: TimeWindow {
            start: SinceMidnight::from_seconds(window_start),
            end: SinceMidnight::from_seconds(window_end),
        },
        max_time: ChronoDuration::seconds(max_time as i64),
        transfer_slack: ChronoDuration::seconds(slack as i64),
        max_parallelism: None,
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

// =============================================================================
// Query-construction primitives (general purpose)
// =============================================================================

/// Per-stop count of events whose absolute time-of-day lies in
/// `[window_start, window_end]`, restricted to patterns active on `date`.
/// Frequency-based trips are not counted (the schedule data we want for
/// busy-stop weighting comes from the discrete events).
pub fn stop_event_weights(
    data: &PreparedData,
    date: NaiveDate,
    window_start: u32,
    window_end: u32,
) -> Vec<u32> {
    let mut weights = vec![0u32; data.stops.len()];
    for &p_idx in patterns_for_date(data, date).iter() {
        let pat = &data.patterns[p_idx];
        let events = &pat.stop_index.events_by_stop;
        for e in &events.data {
            if e.time_offset >= window_start && e.time_offset <= window_end {
                weights[e.stop_index as usize] += 1;
            }
        }
        for freq in &pat.frequency_routes {
            // Count if the frequency service overlaps the window.
            if freq.start_time < window_end && freq.end_time > window_start {
                weights[freq.stop_index as usize] += 1;
            }
        }
    }
    weights
}

/// Sample an index with probability `∝ weights[i]^(1/temperature)`. Zeros
/// excluded. Panics if no nonzero weight. Generic primitive — useful for
/// busy-stop sampling, rich-destination sampling, etc.
pub fn temperature_sample<R: RngCore>(weights: &[u32], temperature: f64, rng: &mut R) -> usize {
    let inv_t = 1.0 / temperature;
    // Compute log-weights for numerical stability with large counts; sample via
    // the Gumbel-max trick: argmax_i (log w_i / T + Gumbel_i).
    let mut best_idx: Option<usize> = None;
    let mut best_score = f64::NEG_INFINITY;
    for (i, &w) in weights.iter().enumerate() {
        if w == 0 {
            continue;
        }
        let log_w = (w as f64).ln();
        let u = next_unit_f64(rng);
        // Gumbel(0,1) = -ln(-ln(U))
        let g = -(-u.ln()).ln();
        let score = log_w * inv_t + g;
        if score > best_score {
            best_score = score;
            best_idx = Some(i);
        }
    }
    best_idx.expect("temperature_sample: all weights zero")
}

fn next_unit_f64<R: RngCore>(rng: &mut R) -> f64 {
    // Open-interval uniform in (0, 1); avoids ln(0) in Gumbel-max.
    loop {
        let bits = rng.next_u64();
        let v = (bits >> 11) as f64 / (1u64 << 53) as f64;
        if v > 0.0 {
            return v;
        }
    }
}

/// Plain Dijkstra over `data.adj` (the public walk graph), capped at
/// `max_seconds`. Returns per-node walk time in seconds; `u32::MAX` for nodes
/// not reachable within the cap.
pub fn walk_times_from(data: &PreparedData, source: u32, max_seconds: u32) -> Vec<u32> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let n = data.num_nodes;
    let mut dist = vec![u32::MAX; n];
    dist[source as usize] = 0;
    let mut heap = BinaryHeap::new();
    heap.push(Reverse((0u32, source)));

    while let Some(Reverse((d, u))) = heap.pop() {
        if d > dist[u as usize] {
            continue;
        }
        for &(v, w) in &data.adj[u] {
            let nd = d.saturating_add(w as u32);
            if nd > max_seconds {
                continue;
            }
            if nd < dist[v as usize] {
                dist[v as usize] = nd;
                heap.push(Reverse((nd, v)));
            }
        }
    }
    dist
}

/// Uniformly sample a node within `max_seconds` walk of `source`. Falls back
/// to `source` if its walk neighborhood is empty (rare, isolated nodes).
pub fn random_node_within_walk<R: RngCore>(
    data: &PreparedData,
    source: u32,
    max_seconds: u32,
    rng: &mut R,
) -> u32 {
    let times = walk_times_from(data, source, max_seconds);
    let candidates: Vec<u32> = (0..times.len() as u32)
        .filter(|&i| times[i as usize] != u32::MAX)
        .collect();
    if candidates.is_empty() {
        source
    } else {
        let idx = (rng.next_u64() % candidates.len() as u64) as usize;
        candidates[idx]
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Public-API helpers used by all property tests.
// ────────────────────────────────────────────────────────────────────────────

/// `&'static Router` over the same fixture as [`load_fixture`]. Re-decodes
/// the binary so the two fixtures hold independent `PreparedData` (engine
/// `&'static PreparedData` vs Router's `Arc<PreparedData>`). The bytes are
/// identical, so the two graphs are observationally indistinguishable.
pub fn router_fixture() -> &'static Router {
    static ROUTER: OnceLock<Router> = OnceLock::new();
    ROUTER.get_or_init(|| {
        let path = fixture_path();
        let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let bytes: Vec<u8> = if raw.starts_with(&[0x1f, 0x8b]) {
            let mut decoder = GzDecoder::new(&raw[..]);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).expect("gunzip fixture");
            out
        } else {
            raw
        };
        let data = data::load(&bytes).expect("parse fixture");
        Router::from_prepared(Arc::new(data))
    })
}

/// Run a query through the public [`Router::isochrone`] API.
pub fn run_iso(router: &Router, params: &IsochroneParams) -> Isochrone {
    router
        .isochrone(params.clone(), |_, _| ControlFlow::Continue(()))
        .expect("router::isochrone")
}

/// Run a query forcing a specific `max_parallelism`. Used by the parity test
/// to collapse the window to one chunk (`max_parallelism = 1`) and compare
/// against the default split path.
pub fn run_iso_with_max_parallelism(
    router: &Router,
    params: &IsochroneParams,
    max: usize,
) -> Isochrone {
    let mut p = params.clone();
    p.max_parallelism = Some(max);
    router
        .isochrone(p, |_, _| ControlFlow::Continue(()))
        .expect("router::isochrone")
}

/// Per-destination Pareto frontier as `(home_departure_secs, arrival_secs)`
/// pairs — flattens [`SinceMidnight`] back to raw u32 so tests can compare
/// directly with `(u32, u32)` sets.
pub fn iso_entries(iso: &Isochrone, dest: u32) -> Vec<(u32, u32)> {
    iso.entries(NodeId(dest))
        .into_iter()
        .map(|e| (e.departure.as_seconds(), e.arrival.as_seconds()))
        .collect()
}

/// Stratified sample of up to `n` destinations with at least one transit
/// path, bucketed by mean travel time to spread the sample across the
/// reachable histogram. Deterministic given `seed`.
pub fn sample_reachable_stratified_iso(iso: &Isochrone, n: usize, seed: u64) -> Vec<u32> {
    let mean = iso.mean_travel_time();
    let frac = iso.reachable_fraction();
    let reachable: Vec<u32> = (0..frac.len() as u32)
        .filter(|&i| frac[i as usize] > 0)
        .filter(|&i| !iso.entries(NodeId(i)).is_empty())
        .collect();
    if reachable.is_empty() {
        return Vec::new();
    }
    let want = n.min(reachable.len());

    let max_time = iso.params().max_time.num_seconds().max(0) as u16;
    let mut by_bucket: std::collections::BTreeMap<u16, Vec<u32>> =
        std::collections::BTreeMap::new();
    for &v in &reachable {
        let mtt = mean[v as usize];
        let bucket = ((mtt as u32 * n as u32) / (max_time.max(1) as u32 + 1)) as u16;
        by_bucket.entry(bucket).or_default().push(v);
    }

    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
    let mut buckets: Vec<Vec<u32>> = by_bucket.into_values().collect();
    for bucket in &mut buckets {
        for i in (1..bucket.len()).rev() {
            let j = (rng.next_u64() % (i as u64 + 1)) as usize;
            bucket.swap(i, j);
        }
    }

    let mut out: Vec<u32> = Vec::with_capacity(want);
    'fill: loop {
        let mut progressed = false;
        for bucket in &mut buckets {
            if let Some(v) = bucket.pop() {
                out.push(v);
                progressed = true;
                if out.len() == want {
                    break 'fill;
                }
            }
        }
        if !progressed {
            break;
        }
    }
    out
}
