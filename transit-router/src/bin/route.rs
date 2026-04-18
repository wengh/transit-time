//! CLI routing harness: run a single TDD query and print the resulting journey.
//!
//! Usage:
//!   cargo run --bin route -- <city.bin> <src_lat> <src_lon> <dst_lat> <dst_lon> [YYYYMMDD] [departure_hhmm] [max_min] [slack_s]
//!
//! Uses the same [`sssp_path::optimal_path`] + [`path_display`] helpers the
//! WASM frontend goes through, so CLI output matches the UI.

use std::path::PathBuf;
use transit_router::profile::SegmentKind;
use transit_router::{data, path_display, router, sssp_path};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        eprintln!(
            "Usage: {} <city.bin> <src_lat> <src_lon> <dst_lat> <dst_lon> [YYYYMMDD] [departure_hhmm] [max_min] [slack_s]",
            args[0]
        );
        std::process::exit(1);
    }

    let bin_path = PathBuf::from(&args[1]);
    let src_lat: f64 = args[2].parse().expect("src_lat");
    let src_lon: f64 = args[3].parse().expect("src_lon");
    let dst_lat: f64 = args[4].parse().expect("dst_lat");
    let dst_lon: f64 = args[5].parse().expect("dst_lon");

    let date: u32 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(20260413);
    let departure_hhmm: u32 = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(1100);
    let max_min: u32 = args.get(8).and_then(|s| s.parse().ok()).unwrap_or(45);
    let slack_s: u32 = args.get(9).and_then(|s| s.parse().ok()).unwrap_or(60);

    let departure_time = (departure_hhmm / 100) * 3600 + (departure_hhmm % 100) * 60;
    let max_time = max_min * 60;

    println!("Loading {:?} ...", bin_path);
    let raw =
        std::fs::read(&bin_path).unwrap_or_else(|e| panic!("Cannot read {:?}: {}", bin_path, e));
    let decompressed;
    let buf = if raw.starts_with(&[0x1f, 0x8b]) {
        let out = std::process::Command::new("gzip")
            .args(["-d", "-c", bin_path.to_str().unwrap()])
            .output()
            .expect("gzip decompress failed — is gzip installed?");
        if !out.status.success() {
            panic!("gzip failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        decompressed = out.stdout;
        &decompressed[..]
    } else {
        &raw[..]
    };
    let prepared = data::load(buf).expect("Failed to load data");

    let src_node = router::snap_to_node(&prepared, src_lat, src_lon)
        .unwrap_or_else(|| panic!("Cannot snap source ({}, {}) to any node", src_lat, src_lon));
    let dst_node = router::snap_to_node(&prepared, dst_lat, dst_lon).unwrap_or_else(|| {
        panic!(
            "Cannot snap destination ({}, {}) to any node",
            dst_lat, dst_lon
        )
    });

    println!("Source: {src_lat}, {src_lon}  → node {src_node}");
    println!("Dest:   {dst_lat}, {dst_lon}  → node {dst_node}");
    println!();
    println!("Mode: single");
    println!("Date: {date}");
    println!(
        "Departure: {:02}:{:02}",
        departure_hhmm / 100,
        departure_hhmm % 100
    );
    println!("Max time: {max_min} min");
    println!("Transfer slack: {slack_s}s");
    println!();

    let pattern_indices = router::patterns_for_date(&prepared, date);
    println!("{} patterns active on {date}", pattern_indices.len());
    println!(
        "{} stops, {} routes, {} leg shapes",
        prepared.stops.len(),
        prepared.route_names.len(),
        prepared.leg_shape_keys.len()
    );

    let (results, boarding_events) = router::run_tdd_multi(
        &prepared,
        src_node,
        departure_time,
        &pattern_indices,
        slack_s,
        max_time,
    );
    let sssp = transit_router::SsspResult {
        results,
        boarding_events,
        departure_time,
    };

    let Some(path) = sssp_path::optimal_path(&prepared, &sssp, dst_node) else {
        println!("Destination unreachable within {max_min} min");
        return;
    };

    println!("Travel time: {} min", (path.total_time + 30) / 60);
    println!();

    let display = path_display::display(&path);
    println!("Route:");
    for lines in &display.segment_lines {
        for line in lines {
            println!("  {line}");
        }
    }
    println!();
    println!("{}", display.total_time_line);

    if let Some(color) = path_display::dominant_route_color(&prepared, &path) {
        println!("Dominant route color: {color}");
    }

    // Per-segment node/shape diagnostic, handy for debugging leg_shape lookups.
    println!();
    for (i, seg) in path.segments.iter().enumerate() {
        let kind = match seg.kind {
            SegmentKind::Walk => "walk",
            SegmentKind::Transit => "transit",
        };
        println!(
            "  segment {} ({kind}): {} nodes, {} → {}",
            i + 1,
            seg.node_sequence.len(),
            seg.start_stop_name,
            seg.end_stop_name
        );
    }
}
