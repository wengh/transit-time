use std::io::Read as _;
use std::path::Path;
use transit_router::profile::SegmentKind;
use transit_router::{SsspResult, data, router, sssp_path};

/// Load a city's .bin file, returning None if it doesn't exist (skip test).
/// Decompresses gzip automatically (files are stored gzip-compressed for the browser).
fn load_city(name: &str) -> Option<data::PreparedData> {
    let bin_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(format!("transit-viz/public/data/{name}.bin"));
    if !bin_path.exists() {
        eprintln!("Skipping: {bin_path:?} not found");
        return None;
    }
    let raw = std::fs::read(&bin_path).expect("read .bin");
    let bytes = if raw.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::read::GzDecoder::new(raw.as_slice());
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).expect("gzip decompress");
        out
    } else {
        raw
    };
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
    date: u32, // YYYYMMDD
    departure_secs: u32,
    transfer_slack: u32,
) -> Vec<Segment> {
    let src_node = router::snap_to_node(data, src.0, src.1).unwrap();
    let dst_node = router::snap_to_node(data, dst.0, dst.1).unwrap();
    let patterns = router::patterns_for_date(data, date);
    let max_time = 7200;
    let (results, boarding_events) = router::run_tdd_multi(
        data,
        src_node,
        departure_secs,
        &patterns,
        transfer_slack,
        max_time,
    );
    let sssp = SsspResult {
        results,
        boarding_events,
        departure_time: departure_secs,
    };

    let Some(path) = sssp_path::optimal_path(data, &sssp, dst_node) else {
        return vec![];
    };
    path.segments
        .iter()
        .map(|s| Segment {
            route_name: s.route_name.clone().unwrap_or_default(),
            is_transit: s.kind == SegmentKind::Transit,
        })
        .collect()
}

fn hhmm(h: u32, m: u32) -> u32 {
    h * 3600 + m * 60
}

// ── Montreal ─────────────────────────────────────────────────────────

#[test]
fn montreal_debug_patterns() {
    let Some(data) = load_city("montreal") else {
        return;
    };
    let patterns = router::patterns_for_date(&data, 20260405);
    eprintln!("Patterns for 20260405 (Sunday): {} active", patterns.len());

    let src_node = router::snap_to_node(&data, 45.500374, -73.568459).unwrap();
    let departure = hhmm(11, 0);
    eprintln!(
        "src_node={} at ({}, {})",
        src_node, data.nodes[src_node as usize].lat, data.nodes[src_node as usize].lon
    );

    let (results, _) = router::run_tdd_multi(&data, src_node, departure, &patterns, 60, 7200);

    let reachable = results
        .iter()
        .filter(|r| r.arrival_delta != u16::MAX)
        .count();
    let via_transit = results.iter().filter(|r| r.route_index != u32::MAX).count();
    eprintln!("Reachable: {}, via transit: {}", reachable, via_transit);

    // Show some transit-reached nodes
    for (i, r) in results.iter().enumerate() {
        if r.route_index != u32::MAX && r.arrival_delta != u16::MAX {
            let route = if (r.route_index as usize) < data.route_names.len() {
                &data.route_names[r.route_index as usize]
            } else {
                "?"
            };
            eprintln!(
                "  transit node {} arrival={} route='{}'",
                i,
                departure + r.arrival_delta as u32,
                route
            );
            // Just show the first 5
            if via_transit > 0 {
                break;
            }
        }
    }

    let dst_node = router::snap_to_node(&data, 45.492700, -73.631000).unwrap();
    eprintln!(
        "dst_node={} at ({}, {})",
        dst_node, data.nodes[dst_node as usize].lat, data.nodes[dst_node as usize].lon
    );
    let dst_r = &results[dst_node as usize];
    eprintln!(
        "dst result: arrival={} edge_type={} route={}",
        departure + dst_r.arrival_delta as u32,
        if dst_r.route_index == u32::MAX { 0 } else { 1 },
        dst_r.route_index
    );

    // Find nearest transit-reached nodes to the destination
    let dst_lat = data.nodes[dst_node as usize].lat;
    let dst_lon = data.nodes[dst_node as usize].lon;
    let mut near_transit: Vec<(f64, usize, u32)> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| r.route_index != u32::MAX && r.arrival_delta != u16::MAX)
        .map(|(i, r)| {
            let dlat = data.nodes[i].lat - dst_lat;
            let dlon = data.nodes[i].lon - dst_lon;
            let dist = (dlat * dlat + dlon * dlon).sqrt();
            (dist, i, departure + r.arrival_delta as u32)
        })
        .collect();
    near_transit.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    eprintln!("Nearest transit nodes to dst:");
    for (dist, i, arr) in near_transit.iter().take(5) {
        let route = if (results[*i].route_index as usize) < data.route_names.len() {
            &data.route_names[results[*i].route_index as usize]
        } else {
            "?"
        };
        eprintln!(
            "  node {} dist={:.5} arr={} route='{}'  ({}, {})",
            i, dist, arr, route, data.nodes[*i].lat, data.nodes[*i].lon
        );
    }

    // Print min_time/max_time for active patterns
    eprintln!("\nActive pattern details:");
    for &pi in &patterns {
        let p = &data.patterns[pi];
        let nstops = p.stop_index.events_by_stop.data.len();
        eprintln!("  pattern {} day_mask={:07b} min_time={}({:.1}h) max_time={}({:.1}h) events={} route_count={}",
            pi, p.day_mask,
            p.min_time, p.min_time as f64 / 3600.0,
            p.max_time, p.max_time as f64 / 3600.0,
            nstops, p.stop_index.events_by_stop.offsets.len());
    }

    // Print bounding box of transit-reached nodes
    let transit_nodes: Vec<(f64, f64)> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| r.route_index != u32::MAX && r.arrival_delta != u16::MAX)
        .map(|(i, _)| (data.nodes[i].lat, data.nodes[i].lon))
        .collect();
    if !transit_nodes.is_empty() {
        let (min_lat, max_lat, min_lon, max_lon) = transit_nodes.iter().fold(
            (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
            |(mila, mala, milo, malo), (lat, lon)| {
                (
                    mila.min(*lat),
                    mala.max(*lat),
                    milo.min(*lon),
                    malo.max(*lon),
                )
            },
        );
        eprintln!(
            "Transit bbox: lat [{:.4}, {:.4}] lon [{:.4}, {:.4}]",
            min_lat, max_lat, min_lon, max_lon
        );
    }

    // What routes are being used for transit-reachable nodes?
    let mut route_counts: std::collections::HashMap<String, u32> = Default::default();
    for r in results.iter() {
        if r.route_index != u32::MAX && r.arrival_delta != u16::MAX {
            let name = if (r.route_index as usize) < data.route_names.len() {
                data.route_names[r.route_index as usize].clone()
            } else {
                "?".into()
            };
            *route_counts.entry(name).or_default() += 1;
        }
    }
    let mut rc: Vec<_> = route_counts.into_iter().collect();
    rc.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("\nRoutes used for transit-reachable nodes (top 10):");
    for (name, count) in rc.iter().take(10) {
        eprintln!("  '{}': {} nodes", name, count);
    }

    // How many total patterns vs. how many active?
    eprintln!("\nTotal patterns in data: {}", data.patterns.len());

    // Check if any patterns have route_names containing metro keywords
    let metro_routes: Vec<_> = data
        .route_names
        .iter()
        .filter(|r| {
            r.to_lowercase().contains("orange")
                || r.to_lowercase().contains("vert")
                || r.to_lowercase().contains("green")
                || r.to_lowercase().contains("metro")
                || r.to_lowercase().contains("ligne")
                || r.to_lowercase().contains("bleu")
        })
        .take(10)
        .collect();
    eprintln!("Metro-like route names: {:?}", metro_routes);

    // Check why patterns are inactive - look at their date ranges and day masks
    let date = 20260405u32;
    let sun_bit = 1u8 << 6; // from patterns_for_date logic
    let mut inactive_reasons: std::collections::HashMap<&str, u32> = Default::default();
    for (i, p) in data.patterns.iter().enumerate() {
        if patterns.contains(&i) {
            continue;
        }
        if p.stop_index.events_by_stop.is_empty() {
            *inactive_reasons.entry("empty").or_default() += 1;
        } else if p.date_exceptions_remove.contains(&date) {
            *inactive_reasons.entry("removed").or_default() += 1;
        } else if p.day_mask & sun_bit == 0 {
            *inactive_reasons.entry("wrong_day").or_default() += 1;
        } else if p.start_date != 0 && date < p.start_date {
            *inactive_reasons.entry("before_start").or_default() += 1;
        } else if p.end_date != 0 && date > p.end_date {
            *inactive_reasons.entry("after_end").or_default() += 1;
        } else {
            *inactive_reasons.entry("other").or_default() += 1;
        }
    }
    eprintln!("\nInactive pattern reasons: {:?}", inactive_reasons);

    // Show date ranges of patterns inactive due to date bounds
    let mut after_end_dates: Vec<(u32, u32, String)> = data
        .patterns
        .iter()
        .enumerate()
        .filter(|(i, p)| !patterns.contains(i) && p.end_date != 0 && date > p.end_date)
        .map(|(_, p)| {
            let rname = String::new();
            (p.start_date, p.end_date, rname)
        })
        .collect();
    after_end_dates.sort_by_key(|x| x.1);
    after_end_dates.dedup_by_key(|x| x.1);
    eprintln!("Expired patterns (after_end), latest end_date first:");
    for (sd, ed, r) in after_end_dates.iter().rev().take(10) {
        eprintln!("  start={} end={} route='{}'", sd, ed, r);
    }

    // Find transit stops within 800m (0.008 deg) of source - what routes do they have?
    let src_lat = data.nodes[src_node as usize].lat;
    let src_lon = data.nodes[src_node as usize].lon;
    eprintln!("\nTransit stops within ~800m of source:");
    for (si, stop) in data.stops.iter().enumerate() {
        let sn = data.stop_node_map[si];
        if sn == u32::MAX {
            continue;
        }
        let stop_node = sn as usize;
        let dlat = data.nodes[stop_node].lat - src_lat;
        let dlon = data.nodes[stop_node].lon - src_lon;
        let dist = (dlat * dlat + dlon * dlon).sqrt();
        if dist < 0.009 {
            // Find what routes serve this stop
            let mut routes: Vec<String> = Vec::new();
            for pi in &patterns {
                let p = &data.patterns[*pi];
                let stop_events = &p.stop_index.events_by_stop[si as u32];
                if !stop_events.is_empty() {
                    // Find route names for events at this stop
                    for e in stop_events.iter().take(1) {
                        if let Some(&ri) = p
                            .sentinel_routes
                            .get(&((p.stop_index.events_by_stop.offsets[si] as usize + 0) as u32))
                        {
                            routes.push(
                                data.route_names
                                    .get(ri as usize)
                                    .cloned()
                                    .unwrap_or("?".into()),
                            );
                        }
                    }
                    routes.push(format!("pat{}", pi));
                }
            }
            eprintln!(
                "  stop {} '{}' dist={:.4} ({:.5},{:.5}) routes={:?}",
                si, stop.name, dist, data.nodes[stop_node].lat, data.nodes[stop_node].lon, routes
            );
        }
    }

    // Search for STM metro station stops by name (guard against unmapped stops)
    let metro_keywords = [
        "McGill",
        "Bonaventure",
        "Atwater",
        "Guy-Concordia",
        "Snowdon",
        "Berri",
        "Lionel",
    ];
    eprintln!("\nSTM metro station stops in data:");
    let mut found_any = false;
    for keyword in metro_keywords {
        for (si, stop) in data.stops.iter().enumerate() {
            if stop.name.contains(keyword) {
                found_any = true;
                let sn = data.stop_node_map[si];
                let node_info = if sn == u32::MAX {
                    "(no walk node)".to_string()
                } else {
                    format!(
                        "node={} ({:.4},{:.4})",
                        sn, data.nodes[sn as usize].lat, data.nodes[sn as usize].lon
                    )
                };
                let has_active = patterns
                    .iter()
                    .any(|&pi| !data.patterns[pi].stop_index.events_by_stop[si as u32].is_empty());
                eprintln!(
                    "  stop {} '{}' {} active={}",
                    si, stop.name, node_info, has_active
                );
            }
        }
    }
    if !found_any {
        eprintln!("  (none found — metro data may be absent from .bin)");
    }

    // Find which patterns serve Snowdon (stop with Snowdon in name) and when they expire
    eprintln!("\nPatterns for Snowdon-area stops:");
    for (si, stop) in data.stops.iter().enumerate() {
        if stop.name.contains("Snowdon") {
            for (pi, p) in data.patterns.iter().enumerate() {
                if !p.stop_index.events_by_stop[si as u32].is_empty() {
                    eprintln!("  stop {} '{}' → pattern {} day_mask={:07b} start={} end={} active_on_20260405={}",
                        si, stop.name, pi, p.day_mask, p.start_date, p.end_date,
                        patterns.contains(&pi));
                }
            }
        }
    }

    assert!(via_transit > 0, "no nodes reachable by transit from source");
}

/// Regression: source near McGill at 11:00 Sunday was routing walk-only (44 min)
/// instead of using the STM metro. Route to Snowdon station area (~4 km, orange line).
#[test]
fn montreal_mcgill_to_snowdon_uses_transit() {
    let Some(data) = load_city("montreal") else {
        return;
    };
    let segs = route(
        &data,
        (45.500374, -73.568459), // near McGill / Sherbrooke
        (45.492700, -73.631000), // near Snowdon metro station
        20260405,
        hhmm(11, 0),
        60, // Sunday 11:00, 60s slack
    );
    assert!(!segs.is_empty(), "should find a route");
    let has_transit = segs.iter().any(|s| s.is_transit);
    assert!(
        has_transit,
        "expected transit segment, got walk-only: {segs:?}"
    );
}

// ── Toronto ──────────────────────────────────────────────────────────

#[test]
fn toronto_union_to_bloor_uses_subway() {
    let Some(data) = load_city("toronto") else {
        return;
    };
    let segs = route(
        &data,
        (43.645673, -79.380542), // Union Station area
        (43.670678, -79.386178), // Bloor-Yonge area
        20260406,
        hhmm(8, 0),
        60, // Monday 08:00, 60s slack
    );
    assert!(!segs.is_empty(), "should find a route");
    let has_subway = segs.iter().any(|s| {
        s.is_transit
            && (s.route_name.contains("Line 1")
                || s.route_name.contains("Yonge")
                || s.route_name == "1"
                || s.route_name == "97")
    });
    assert!(has_subway, "expected Line 1 subway, got: {segs:?}");
}

#[test]
fn toronto_union_to_bloor_sunday() {
    let Some(data) = load_city("toronto") else {
        return;
    };
    let segs = route(
        &data,
        (43.645153, -79.380605),
        (43.664838, -79.384622),
        20260405,
        hhmm(8, 0),
        60, // Sunday 08:00
    );
    assert!(!segs.is_empty(), "should find a route on Sunday");
    let has_subway = segs.iter().any(|s| {
        s.is_transit
            && (s.route_name.contains("Line 1")
                || s.route_name.contains("Yonge")
                || s.route_name == "1"
                || s.route_name == "97")
    });
    assert!(
        has_subway,
        "expected Line 1 subway on Sunday, got: {segs:?}"
    );
}

// ── Mexico City ─────────────────────────────────────────────────────

#[test]
fn mexico_city_zocalo_to_chapultepec_uses_metro() {
    let Some(data) = load_city("mexico_city") else {
        return;
    };
    let segs = route(
        &data,
        (19.4326, -99.1332), // Zócalo area
        (19.4217, -99.1815), // Chapultepec area
        20260410,
        hhmm(9, 0),
        60, // Friday 09:00, 60s slack
    );
    assert!(!segs.is_empty(), "should find a route");
    let has_transit = segs.iter().any(|s| s.is_transit);
    assert!(
        has_transit,
        "expected transit segment (metro), got walk-only: {segs:?}"
    );
}
