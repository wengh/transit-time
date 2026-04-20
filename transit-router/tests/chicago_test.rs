use std::path::Path;

/// Test routing in Chicago from the user-specified origin to destination.
/// Origin: 41.8961613696194, -87.77847803599614
/// Dest:   41.884409337007234, -87.62865402720838
/// Departure: 11:10 AM Thursday
/// Google Maps says arrival at 12:06 PM
#[test]
fn test_chicago_route() {
    let bin_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("cache/chicago.bin");

    if !bin_path.exists() {
        eprintln!("Skipping test: {:?} not found", bin_path);
        return;
    }

    let data = std::fs::read(&bin_path).expect("Failed to read binary");
    let prepared = transit_router::data::load(&data).expect("Failed to load data");

    eprintln!(
        "Loaded: {} nodes, {} edges, {} stops, {} patterns, {} routes",
        prepared.num_nodes,
        prepared.num_edges,
        prepared.num_stops,
        prepared.patterns.len(),
        prepared.route_names.len()
    );

    // Find the Thursday pattern (day_of_week: Thu = 3, bit 3)
    let thu_bit = 1u8 << 3; // Thursday
    let mut pattern_idx = None;
    for (i, p) in prepared.patterns.iter().enumerate() {
        if p.day_mask & thu_bit != 0 {
            eprintln!(
                "Pattern {} has Thursday (day_mask={:07b}, events={}, min_time={}, max_time={})",
                i,
                p.day_mask,
                p.stop_index.events_by_stop.data.len(),
                p.min_time,
                p.max_time
            );
            if pattern_idx.is_none() {
                // Pick the pattern with the most events (likely the main weekday pattern)
                pattern_idx = Some(i);
            }
        }
    }

    // Use patterns_for_date to find all patterns active on a Thursday
    let thu_patterns = transit_router::router::patterns_for_date(&prepared, 20260402); // Thursday
    eprintln!("All Thursday patterns: {:?}", thu_patterns);

    // Snap origin and destination
    let origin_lat = 41.8961613696194;
    let origin_lon = -87.77847803599614;
    let dest_lat = 41.884409337007234;
    let dest_lon = -87.62865402720838;

    let origin_node =
        transit_router::router::snap_to_node(&prepared, origin_lat, origin_lon).unwrap();
    let dest_node = transit_router::router::snap_to_node(&prepared, dest_lat, dest_lon).unwrap();

    eprintln!(
        "Origin node: {} at ({}, {})",
        origin_node,
        prepared.nodes[origin_node as usize].lat,
        prepared.nodes[origin_node as usize].lon
    );
    eprintln!(
        "Dest node: {} at ({}, {})",
        dest_node, prepared.nodes[dest_node as usize].lat, prepared.nodes[dest_node as usize].lon
    );

    // Departure at 11:10 AM = 11*3600 + 10*60 = 40200 seconds
    let departure_time = 11 * 3600 + 10 * 60;
    eprintln!("Departure time: {} (11:10 AM)", departure_time);

    let transfer_slack = 60; // 1 minute default
    let max_time = 7200; // 2 hours
    let (result, boarding_events) = transit_router::router::run_tdd_multi(
        &prepared,
        origin_node,
        departure_time,
        &thu_patterns,
        transfer_slack,
        max_time,
    );

    let dest_arrival_delta = result[dest_node as usize].arrival_delta;
    if dest_arrival_delta == u16::MAX {
        eprintln!("ERROR: Destination is unreachable!");

        // Check reachability stats
        let reachable = result
            .iter()
            .filter(|r| r.arrival_delta != u16::MAX)
            .count();
        let transit_reached = result.iter().filter(|r| r.route_index != u32::MAX).count();
        eprintln!(
            "Total reachable: {}, via transit: {}",
            reachable, transit_reached
        );
        panic!("Destination unreachable");
    }

    let dest_arrival = departure_time + dest_arrival_delta as u32;
    let travel_time_sec = dest_arrival_delta as u32;
    let travel_min = travel_time_sec / 60;
    let arrival_h = dest_arrival / 3600;
    let arrival_m = (dest_arrival % 3600) / 60;
    eprintln!("\n=== ROUTE RESULT ===");
    eprintln!("Travel time: {} min ({} sec)", travel_min, travel_time_sec);
    eprintln!("Arrival time: {:02}:{:02}", arrival_h, arrival_m);

    // Reconstruct path using library function
    eprintln!("\n=== PATH RECONSTRUCTION ===");
    let sssp = transit_router::SsspResult {
        results: result,
        boarding_events,
        departure_time,
    };
    let path = transit_router::sssp_path::optimal_path(&prepared, &sssp, dest_node).expect("path");
    for (i, seg) in path.segments.iter().enumerate() {
        let dur = seg.end_time.saturating_sub(seg.start_time);
        eprintln!(
            "  {}. {:?} {} sec: {} → {}",
            i + 1,
            seg.kind,
            dur,
            seg.start_stop_name,
            seg.end_stop_name
        );
    }

    eprintln!("\n=== COMPARISON ===");
    eprintln!(
        "Our arrival: {:02}:{:02} ({} min travel)",
        arrival_h, arrival_m, travel_min
    );
    eprintln!("Google Maps arrival: 12:06 (56 min travel)");
    let diff = (travel_min as i32) - 56;
    eprintln!("Difference: {} min", diff);
}
