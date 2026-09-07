//! Deterministic fingerprint of routing results for one query, used to
//! verify that a data-format or engine change preserves results exactly.
//! Prints three hashes: the isochrone arrays + sampled Pareto entries, the
//! sampled paths' (departure, arrival, total) triples, and their segments.
//! Usage: fingerprint <city.bin> <lat> <lon> <YYYYMMDD> <start_hhmm> <minutes> <max_min>
use std::hash::{Hash, Hasher};
use std::ops::ControlFlow;
use std::sync::Arc;
use transit_router::{IsochroneParams, NodeId, Router, SinceMidnight, TimeWindow};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let raw = std::fs::read(&a[1]).unwrap();
    let bytes = transit_router::load_maybe_gzipped(&raw).unwrap();
    let data = transit_router::data::load(&bytes).unwrap();
    let router = Router::from_prepared(Arc::new(data));
    let src = router
        .snap(a[2].parse().unwrap(), a[3].parse().unwrap())
        .unwrap();
    let date = transit_router::data::yyyymmdd_to_naive_date_opt(a[4].parse().unwrap()).unwrap();
    let hhmm: u32 = a[5].parse().unwrap();
    let start = (hhmm / 100) * 3600 + (hhmm % 100) * 60;
    let minutes: u32 = a[6].parse().unwrap();
    let max_min: i64 = a[7].parse().unwrap();
    let params = IsochroneParams {
        source: src,
        date,
        window: TimeWindow {
            start: SinceMidnight::from_seconds(start),
            end: SinceMidnight::from_seconds(start + minutes * 60),
        },
        max_time: chrono::Duration::minutes(max_min),
        transfer_slack: chrono::Duration::seconds(60),
        max_parallelism: Some(1),
    };
    let iso = router
        .isochrone(params, |_, _| ControlFlow::Continue(()))
        .unwrap();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut hp = std::collections::hash_map::DefaultHasher::new();
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    iso.mean_travel_time().hash(&mut h);
    iso.reachable_fraction().hash(&mut h);
    let n = iso.num_nodes();
    let (mut entries, mut paths, mut segs) = (0usize, 0usize, 0usize);
    for node in (0..n).step_by(97) {
        let e = iso.entries(NodeId(node as u32));
        entries += e.len();
        for x in &e {
            x.departure.as_seconds().hash(&mut h);
            x.arrival.as_seconds().hash(&mut h);
        }
    }
    for node in (0..n).step_by(2001) {
        for p in iso.paths(NodeId(node as u32)) {
            paths += 1;
            segs += p.segments.len();
            (p.home_departure, p.arrival_time, p.total_time).hash(&mut hp);
            for s in &p.segments {
                (
                    s.start_time,
                    s.end_time,
                    s.route_index,
                    &s.node_sequence,
                    s.wait_time,
                )
                    .hash(&mut hs);
            }
        }
    }
    let reached = iso.reachable_fraction().iter().filter(|&&f| f > 0).count();
    println!(
        "src={} patterns={} reached={} entries={} paths={} segs={} iso={:016x} pathtimes={:016x} segments={:016x}",
        src.get(),
        router.data().patterns.len(),
        reached,
        entries,
        paths,
        segs,
        h.finish(),
        hp.finish(),
        hs.finish()
    );
}
