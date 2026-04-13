/// CLI routing harness: run a single query, reconstruct path, print segments with shape coords and colors.
///
/// Usage:
///   cargo run --bin route -- <city.bin> <src_lat> <src_lon> <dst_lat> <dst_lon> [YYYYMMDD] [departure_hhmm] [max_min] [slack_s]
///
/// Example (Hong Kong):
///   cargo run --bin route -- ../transit-viz/public/data/hong_kong.bin 22.303486 114.180321 22.2875 114.1745 20260413 1100 45 60
use std::path::PathBuf;
use transit_router::{data, reconstruct_path, router};

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
    // Detect gzip magic bytes and decompress if needed
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

    let hhmm = departure_hhmm;
    println!("Mode: single");
    println!("Date: {date}");
    println!("Departure: {:02}:{:02}", hhmm / 100, hhmm % 100);
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
    let arrival = sssp.results[dst_node as usize].arrival_delta;

    if arrival == u16::MAX {
        println!("Destination unreachable within {max_min} min");
        return;
    }

    let total_time = arrival as u32;
    println!("Travel time: {} min", total_time / 60);
    println!();

    let path = reconstruct_path(&prepared, &sssp, dst_node);
    if path.is_empty() {
        println!("(no path reconstructed)");
        return;
    }

    // Parse segments — mirrors parsePathSegments in router.ts, including boardNode tracking.
    // reconstruct_path emits:  [node, edge_type, route_idx, ...]
    // For transit: only the *alighting* stop is emitted. The boarding node comes from the
    // end of the preceding walk segment (stored in prev_end_node).
    println!("Route:");
    let mut i = 0;
    let mut prev_end_node: Option<u32> = None;

    while i < path.len() {
        let start_idx = i;
        let edge_type = path[i + 1];
        let route_idx = path[i + 2];

        // Extend segment while same edge_type and route_idx
        while i + 3 < path.len() && path[i + 3 + 1] == edge_type && path[i + 3 + 2] == route_idx {
            i += 3;
        }
        let end_idx = i;
        let start_node = path[start_idx];
        let end_node = path[end_idx];

        let start_arr = sssp.results[start_node as usize].arrival_delta;
        let end_arr = sssp.results[end_node as usize].arrival_delta;
        let duration = end_arr.saturating_sub(start_arr) as u32;

        if edge_type == 0 {
            // Walk segment
            println!("  Walk {} min", (duration + 30) / 60);
            prev_end_node = Some(end_node);
        } else {
            // Transit segment
            let route_name = if (route_idx as usize) < prepared.route_names.len() {
                prepared.route_names[route_idx as usize].clone()
            } else {
                format!("route#{route_idx}")
            };

            // Board node comes from the end of the prior walk segment (mirrors TypeScript boardNode)
            let board_node = prev_end_node.unwrap_or(start_node);

            // Collect stop nodes: board_node + all nodes in this transit segment
            let mut seg_nodes: Vec<u32> = Vec::new();
            if board_node != path[start_idx] {
                seg_nodes.push(board_node);
            }
            for j in (start_idx..=end_idx).step_by(3) {
                seg_nodes.push(path[j]);
            }

            let board_stop_name = node_stop_name(&prepared, board_node);
            let end_stop_name = node_stop_name(&prepared, end_node);

            // Wait time (arrival at board stop → boarding time)
            let boarding_delta = sssp.results[end_node as usize].boarding_delta;
            let wait_secs = if boarding_delta != u16::MAX {
                let arr_at_board = sssp.results[board_node as usize].arrival_delta as u32;
                (boarding_delta as u32).saturating_sub(arr_at_board)
            } else {
                0
            };

            // Ride duration
            let ride_duration = if boarding_delta != u16::MAX {
                (end_arr as u32).saturating_sub(boarding_delta as u32)
            } else {
                duration
            };

            // Color
            let color = if (route_idx as usize) < prepared.route_colors.len() {
                match prepared.route_colors[route_idx as usize] {
                    Some(c) => format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b),
                    None => "(no color)".to_string(),
                }
            } else {
                "(out of range)".to_string()
            };

            println!(
                "  {route_name} · {board_stop_name} → {end_stop_name}  {} min  color={color}",
                (ride_duration + 30) / 60
            );
            if wait_secs > 0 {
                println!("    Wait: {:.1} min", wait_secs as f64 / 60.0);
            }

            // Shape lookup for each consecutive stop pair in the segment
            println!("    Shape legs ({} nodes in segment):", seg_nodes.len());
            for j in 0..seg_nodes.len().saturating_sub(1) {
                let from_node = seg_nodes[j];
                let to_node = seg_nodes[j + 1];
                let from_stop = prepared.node_stop_indices.get(from_node).first().copied();
                let to_stop = prepared.node_stop_indices.get(to_node).first().copied();

                match (from_stop, to_stop) {
                    (Some(fs), Some(ts)) => {
                        let key = (route_idx, fs, ts);
                        let shape_pts = match prepared.leg_shape_keys.binary_search(&key) {
                            Ok(idx) => {
                                let start = prepared.leg_shapes.offsets[idx] as usize;
                                let end = prepared.leg_shapes.offsets[idx + 1] as usize;
                                let compressed = &prepared.leg_shapes.data[start..end];
                                if compressed.is_empty() {
                                    0
                                } else {
                                    match pco::standalone::simple_decompress::<u32>(compressed) {
                                        Ok(c) => c.len() / 2,
                                        Err(e) => {
                                            println!("      node {from_node}→{to_node}: stop {fs}→{ts}: decompress error: {e}");
                                            0
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                println!("      node {from_node}→{to_node}: stop {fs}→{ts}: key ({route_idx},{fs},{ts}) NOT FOUND in leg_shape_keys");
                                continue;
                            }
                        };
                        println!("      node {from_node}→{to_node}: stop {fs}→{ts}: {shape_pts} shape points");
                    }
                    _ => {
                        println!("      node {from_node}→{to_node}: from_stop={from_stop:?} to_stop={to_stop:?}: STOP NOT FOUND (node is not a stop)");
                    }
                }
            }

            prev_end_node = Some(end_node);
        }

        i += 3;
    }
}

fn node_stop_name(data: &data::PreparedData, node: u32) -> String {
    if node as usize >= data.node_is_stop.len() || !data.node_is_stop[node as usize] {
        return format!("node#{node}");
    }
    if let Some(&stop_idx) = data.node_stop_indices.get(node).first() {
        data.stops[stop_idx as usize].name.clone()
    } else {
        format!("node#{node}")
    }
}
