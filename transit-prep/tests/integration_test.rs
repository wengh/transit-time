use std::path::Path;

/// Test that we can parse the cached GTFS data.
#[test]
fn test_parse_cached_gtfs() {
    let cache_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("cache");
    let gtfs_path = cache_dir.join("Chapel_Hill.gtfs.zip");

    if !gtfs_path.exists() {
        eprintln!("Skipping test: {:?} not found", gtfs_path);
        return;
    }

    let data = transit_prep::gtfs::parse_gtfs(&gtfs_path).expect("Failed to parse GTFS");

    assert!(!data.stops.is_empty(), "Should have stops");
    assert!(!data.routes.is_empty(), "Should have routes");
    assert!(!data.trips.is_empty(), "Should have trips");
    assert!(!data.stop_times.is_empty(), "Should have stop_times");
    assert!(!data.services.is_empty(), "Should have services");

    eprintln!(
        "GTFS: {} stops, {} routes, {} trips, {} stop_times",
        data.stops.len(),
        data.routes.len(),
        data.trips.len(),
        data.stop_times.len()
    );

    // Verify stop data
    for stop in &data.stops {
        assert!(!stop.id.is_empty(), "Stop should have ID");
        assert!(stop.lat != 0.0, "Stop should have lat");
        assert!(stop.lon != 0.0, "Stop should have lon");
    }

    // Build service patterns
    let patterns = transit_prep::gtfs::build_service_patterns(&data);
    assert!(!patterns.is_empty(), "Should have patterns");

    for pattern in &patterns {
        eprintln!(
            "Pattern {}: day_mask={:07b}, events={}, min_time={}, max_time={}, freq={}",
            pattern.pattern_id,
            pattern.day_mask,
            pattern.events.len(),
            pattern.min_time,
            pattern.max_time,
            pattern.frequency_routes.len(),
        );
    }

    eprintln!("GTFS parse test passed!");
}

/// Test OSM graph building from cached data.
#[test]
fn test_build_cached_osm_graph() {
    let cache_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("cache");

    // Find the cached OSM file
    let osm_path = std::fs::read_dir(&cache_dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .find(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("osm_") && n.ends_with(".xml"))
                })
                .map(|e| e.path())
        });

    let osm_path = match osm_path {
        Some(p) => p,
        None => {
            eprintln!("Skipping test: no cached OSM data found");
            return;
        }
    };

    // bbox not used for XML parsing, but required by signature
    let bbox = (-79.10, 35.87, -79.00, 35.97);
    let graph = transit_prep::graph::build_graph(&osm_path, bbox).expect("Failed to build graph");

    assert!(!graph.nodes.is_empty(), "Should have nodes");
    assert!(!graph.edges.is_empty(), "Should have edges");

    eprintln!(
        "OSM graph: {} nodes, {} edges",
        graph.nodes.len(),
        graph.edges.len()
    );

    // Verify all edges reference valid nodes
    let max_idx = graph.nodes.len() as u32;
    for edge in &graph.edges {
        assert!(edge.u < max_idx, "Edge u index out of bounds");
        assert!(edge.v < max_idx, "Edge v index out of bounds");
        assert!(edge.distance_meters > 0.0, "Edge distance should be positive");
    }

    eprintln!("OSM graph test passed!");
}

/// Test binary serialization round-trip.
#[test]
fn test_binary_roundtrip() {
    let cache_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("cache");
    let bin_path = cache_dir.join("chapel_hill.bin");

    if !bin_path.exists() {
        eprintln!("Skipping test: {:?} not found", bin_path);
        return;
    }

    let data = std::fs::read(&bin_path).expect("Failed to read binary");
    let deserialized =
        transit_prep::binary::read_binary(&data).expect("Failed to deserialize binary");

    assert!(!deserialized.nodes.is_empty(), "Should have nodes");
    assert!(!deserialized.edges.is_empty(), "Should have edges");
    assert!(!deserialized.stops.is_empty(), "Should have stops");
    assert!(!deserialized.patterns.is_empty(), "Should have patterns");

    eprintln!(
        "Binary roundtrip: {} nodes, {} edges, {} stops, {} patterns",
        deserialized.nodes.len(),
        deserialized.edges.len(),
        deserialized.stops.len(),
        deserialized.patterns.len()
    );

    eprintln!("Binary roundtrip test passed!");
}
