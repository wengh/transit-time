use std::path::Path;
use transit_router::{data, router, reconstruct_path, SsspResult};

/// Load a city's .bin file, returning None if it doesn't exist (skip test).
fn load_city(name: &str) -> Option<data::PreparedData> {
    let bin_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(format!("transit-viz/public/data/{name}.bin"));
    if !bin_path.exists() {
        eprintln!("Skipping: {bin_path:?} not found");
        return None;
    }
    let bytes = std::fs::read(&bin_path).expect("read .bin");
    Some(data::load(&bytes).expect("parse .bin"))
}

/// Parsed segment from a reconstructed path.
#[derive(Debug)]
struct Segment {
    route_name: String,
    is_transit: bool,
}

/// Route a trip and return the list of segments.
fn route(
    data: &data::PreparedData,
    src: (f64, f64),
    dst: (f64, f64),
    day: u8,           // 0=Mon..6=Sun
    departure_secs: u32,
    transfer_slack: u32,
) -> Vec<Segment> {
    let src_node = router::snap_to_node(data, src.0, src.1);
    let dst_node = router::snap_to_node(data, dst.0, dst.1);
    let patterns = router::patterns_for_day(data, day);
    let max_time = 7200;
    let results = router::run_tdd_multi(
        data, src_node, departure_secs, &patterns, transfer_slack, max_time,
    );
    let sssp = SsspResult { results, departure_time: departure_secs };

    let path = reconstruct_path(data, &sssp, dst_node);
    if path.is_empty() {
        return vec![];
    }

    // Group consecutive entries by (edge_type, route_index)
    let mut segments = Vec::new();
    let mut i = 0;
    while i < path.len() {
        let edge_type = path[i + 1];
        let route_idx = path[i + 2];
        // Skip ahead while same group
        while i + 3 < path.len() && path[i + 4] == edge_type && path[i + 5] == route_idx {
            i += 3;
        }
        let route_name = if edge_type == 1 && (route_idx as usize) < data.route_names.len() {
            data.route_names[route_idx as usize].clone()
        } else {
            String::new()
        };
        segments.push(Segment {
            route_name,
            is_transit: edge_type == 1,
        });
        i += 3;
    }
    segments
}

fn has_route(segments: &[Segment], name: &str) -> bool {
    segments.iter().any(|s| s.route_name.contains(name))
}

fn hhmm(h: u32, m: u32) -> u32 {
    h * 3600 + m * 60
}

// ── Toronto ──────────────────────────────────────────────────────────

#[test]
fn toronto_union_to_bloor_uses_subway() {
    let Some(data) = load_city("toronto") else { return };
    let segs = route(
        &data,
        (43.645673, -79.380542), // Union Station area
        (43.670678, -79.386178), // Bloor-Yonge area
        0, hhmm(8, 0), 60,      // Monday 08:00, 60s slack
    );
    assert!(!segs.is_empty(), "should find a route");
    let has_subway = segs.iter().any(|s| {
        s.is_transit && (s.route_name.contains("Line 1") || s.route_name.contains("Yonge"))
    });
    assert!(has_subway, "expected Line 1 subway, got: {segs:?}");
}

#[test]
fn toronto_union_to_bloor_sunday() {
    let Some(data) = load_city("toronto") else { return };
    let segs = route(
        &data,
        (43.645153, -79.380605),
        (43.664838, -79.384622),
        6, hhmm(8, 0), 60,      // Sunday 08:00
    );
    assert!(!segs.is_empty(), "should find a route on Sunday");
    let has_subway = segs.iter().any(|s| {
        s.is_transit && (s.route_name.contains("Line 1") || s.route_name.contains("Yonge"))
    });
    assert!(has_subway, "expected Line 1 subway on Sunday, got: {segs:?}");
}
