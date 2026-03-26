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

    eprintln!("Loaded: {} nodes, {} edges, {} stops, {} patterns, {} routes",
        prepared.num_nodes, prepared.num_edges, prepared.num_stops,
        prepared.patterns.len(), prepared.route_names.len());

    // Find the Thursday pattern (day_of_week: Thu = 3, bit 3)
    let thu_bit = 1u8 << 3; // Thursday
    let mut pattern_idx = None;
    for (i, p) in prepared.patterns.iter().enumerate() {
        if p.day_mask & thu_bit != 0 {
            eprintln!("Pattern {} has Thursday (day_mask={:07b}, events={}, min_time={}, max_time={})",
                i, p.day_mask, p.events.len(), p.min_time, p.max_time);
            if pattern_idx.is_none() {
                // Pick the pattern with the most events (likely the main weekday pattern)
                pattern_idx = Some(i);
            }
        }
    }

    // Pick the Thursday pattern with most events
    let thu_patterns: Vec<(usize, usize)> = prepared.patterns.iter().enumerate()
        .filter(|(_, p)| p.day_mask & thu_bit != 0)
        .map(|(i, p)| (i, p.events.len()))
        .collect();
    let best_pattern = thu_patterns.iter().max_by_key(|(_, n)| *n).unwrap().0;
    eprintln!("Using pattern {} (most events for Thursday)", best_pattern);

    // Snap origin and destination
    let origin_lat = 41.8961613696194;
    let origin_lon = -87.77847803599614;
    let dest_lat = 41.884409337007234;
    let dest_lon = -87.62865402720838;

    let origin_node = transit_router::router::snap_to_node(&prepared, origin_lat, origin_lon);
    let dest_node = transit_router::router::snap_to_node(&prepared, dest_lat, dest_lon);

    eprintln!("Origin node: {} at ({}, {})",
        origin_node, prepared.nodes[origin_node as usize].lat, prepared.nodes[origin_node as usize].lon);
    eprintln!("Dest node: {} at ({}, {})",
        dest_node, prepared.nodes[dest_node as usize].lat, prepared.nodes[dest_node as usize].lon);

    // Departure at 11:10 AM = 11*3600 + 10*60 = 40200 seconds
    let departure_time = 11 * 3600 + 10 * 60;
    eprintln!("Departure time: {} (11:10 AM)", departure_time);

    let transfer_slack = 60; // 1 minute default
    let result = transit_router::router::run_tdd(&prepared, origin_node, departure_time, best_pattern, transfer_slack);

    let dest_arrival = result[dest_node as usize][0];
    if dest_arrival == u32::MAX {
        eprintln!("ERROR: Destination is unreachable!");

        // Check reachability stats
        let reachable = result.iter().filter(|r| r[0] != u32::MAX).count();
        let transit_reached = result.iter().filter(|r| r[2] == 1).count();
        eprintln!("Total reachable: {}, via transit: {}", reachable, transit_reached);
        panic!("Destination unreachable");
    }

    let travel_time_sec = dest_arrival - departure_time;
    let travel_min = travel_time_sec / 60;
    let arrival_h = dest_arrival / 3600;
    let arrival_m = (dest_arrival % 3600) / 60;
    eprintln!("\n=== ROUTE RESULT ===");
    eprintln!("Travel time: {} min ({} sec)", travel_min, travel_time_sec);
    eprintln!("Arrival time: {:02}:{:02}", arrival_h, arrival_m);

    // Reconstruct path
    eprintln!("\n=== PATH RECONSTRUCTION ===");
    let mut path_nodes: Vec<(u32, u32, u32)> = Vec::new(); // (node, edge_type, route_idx)
    let mut current = dest_node;
    loop {
        let r = &result[current as usize];
        if r[0] == u32::MAX {
            break;
        }
        path_nodes.push((current, r[2], r[3]));
        let prev = r[1];
        if prev == u32::MAX || prev == current {
            break;
        }
        current = prev;
    }
    path_nodes.reverse();

    // Group consecutive segments by type and route
    let mut segments: Vec<PathSegment> = Vec::new();
    for &(node, edge_type, route_idx) in &path_nodes {
        let arrival = result[node as usize][0];
        let is_stop = prepared.node_is_stop[node as usize];
        let stop_name = if is_stop {
            prepared.node_stop_indices[node as usize]
                .first()
                .map(|&si| prepared.stops[si as usize].name.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let should_start_new = match segments.last() {
            None => true,
            Some(last) => last.edge_type != edge_type || last.route_idx != route_idx,
        };

        if should_start_new {
            segments.push(PathSegment {
                edge_type,
                route_idx,
                start_node: node,
                end_node: node,
                start_arrival: arrival,
                end_arrival: arrival,
                start_stop_name: stop_name.clone(),
                end_stop_name: stop_name,
                node_count: 1,
            });
        } else {
            let last = segments.last_mut().unwrap();
            last.end_node = node;
            last.end_arrival = arrival;
            last.end_stop_name = stop_name;
            last.node_count += 1;
        }
    }

    for (i, seg) in segments.iter().enumerate() {
        let seg_duration = seg.end_arrival - seg.start_arrival;
        let seg_min = seg_duration / 60;
        let seg_sec = seg_duration % 60;

        let start_time_h = seg.start_arrival / 3600;
        let start_time_m = (seg.start_arrival % 3600) / 60;
        let end_time_h = seg.end_arrival / 3600;
        let end_time_m = (seg.end_arrival % 3600) / 60;

        if seg.edge_type == 0 {
            eprintln!("  {}. WALK {} min {} sec ({:02}:{:02} -> {:02}:{:02}), {} nodes",
                i + 1, seg_min, seg_sec, start_time_h, start_time_m,
                end_time_h, end_time_m, seg.node_count);
            if !seg.end_stop_name.is_empty() {
                eprintln!("     to: {}", seg.end_stop_name);
            }
        } else {
            let route_name = if (seg.route_idx as usize) < prepared.route_names.len() {
                &prepared.route_names[seg.route_idx as usize]
            } else {
                "?"
            };
            eprintln!("  {}. TRANSIT route '{}' ({} min {} sec, {:02}:{:02} -> {:02}:{:02})",
                i + 1, route_name, seg_min, seg_sec,
                start_time_h, start_time_m, end_time_h, end_time_m);
            if !seg.start_stop_name.is_empty() {
                eprintln!("     board at: {}", seg.start_stop_name);
            }
            if !seg.end_stop_name.is_empty() {
                eprintln!("     alight at: {}", seg.end_stop_name);
            }
        }
    }

    eprintln!("\n=== COMPARISON ===");
    eprintln!("Our arrival: {:02}:{:02} ({} min travel)", arrival_h, arrival_m, travel_min);
    eprintln!("Google Maps arrival: 12:06 (56 min travel)");
    let diff = (travel_min as i32) - 56;
    eprintln!("Difference: {} min", diff);
}

struct PathSegment {
    edge_type: u32,
    route_idx: u32,
    start_node: u32,
    end_node: u32,
    start_arrival: u32,
    end_arrival: u32,
    start_stop_name: String,
    end_stop_name: String,
    node_count: u32,
}
