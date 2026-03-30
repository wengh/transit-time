use transit_router::data::*;
use transit_router::router::*;
use std::collections::HashMap;

/// Build a minimal PreparedData for testing.
///
/// Graph topology:
///
///   [0:Source] --walk 180s-- [1:GreenStop] --walk 420s-- [2:Midway] --walk 420s-- [4:Dest]
///                  \                                        |
///                   --walk 300s-- [3:PinkStop]              |
///                                                          |
///   Pink trip: stop0(node3) @29100 --120s--> stop2(node2)
///   Green trip: stop1(node1) @29100 --120s--> stop2(node2) --120s--> stop3(node4)
///   Pink→Green transfer at node2 (Midway): takes 60s slack, then Green continues
///
/// Expected:
///   - Pink path to Dest: walk 300s to PinkStop, board Pink @29100, arrive Midway @29220,
///     transfer to Green @29280 (60s slack), Green departs ~29280, arrives Dest @29400
///     leave_home = 29100 - 300 = 28800
///   - Direct Green to Dest: walk 180s to GreenStop, board Green @29100, ride through
///     Midway @29220, continue to Dest @29340
///     leave_home = 29100 - 180 = 28920
///
///   Direct Green arrives EARLIER (29340 < 29400) AND has better leave_home (28920 > 28800).
///   Without trip-following, Green gets blocked at Midway (Pink arrives first via walk shortcut).
///   With trip-following, Green rides through Midway to Dest directly.
fn build_test_data() -> PreparedData {
    // 5 nodes: source(0), green_stop(1), midway(2), pink_stop(3), dest(4)
    let nodes = vec![
        NodeData { lat: 0.0, lon: 0.0 },  // 0: source
        NodeData { lat: 0.001, lon: 0.0 }, // 1: green line stop
        NodeData { lat: 0.002, lon: 0.0 }, // 2: midway (shared stop)
        NodeData { lat: 0.0, lon: 0.001 }, // 3: pink line stop
        NodeData { lat: 0.003, lon: 0.0 }, // 4: destination stop
    ];

    // Walking edges (bidirectional):
    //   0-1: 180s walk (252m at 1.4 m/s)
    //   0-3: 300s walk (420m)
    //   0-2: 420s walk (588m) — direct walk to midway is slower than Pink
    //   2-4: 420s walk (588m)
    let edges = vec![
        EdgeData { u: 0, v: 1, distance_meters: 252.0 },
        EdgeData { u: 0, v: 3, distance_meters: 420.0 },
        EdgeData { u: 0, v: 2, distance_meters: 588.0 },
        EdgeData { u: 2, v: 4, distance_meters: 588.0 },
    ];

    // 4 stops:
    //   stop0 -> node3 (pink stop)
    //   stop1 -> node1 (green stop)
    //   stop2 -> node2 (midway, served by both)
    //   stop3 -> node4 (dest, green only)
    let stops = vec![
        StopData { lat: 0.0, lon: 0.001, name: "Pink Stop".into() },
        StopData { lat: 0.001, lon: 0.0, name: "Green Stop".into() },
        StopData { lat: 0.002, lon: 0.0, name: "Midway".into() },
        StopData { lat: 0.003, lon: 0.0, name: "Dest".into() },
    ];
    let stop_node_map = vec![3, 1, 2, 4]; // stop_idx -> node_idx

    let route_names = vec!["Pink Line".into(), "Green Line".into()];

    // Build events.
    // min_time = 28800 (8:00 AM), so events[i] = second 28800+i
    //
    // Pink trip (trip_index=0, route_index=0):
    //   stop0(PinkStop) @29100 -> stop2(Midway), travel=120s
    //
    // Green trip (trip_index=1, route_index=1):
    //   stop1(GreenStop) @29100 -> stop2(Midway), travel=120s
    //   stop2(Midway) @29220 -> stop3(Dest), travel=120s
    let min_time = 28800u32;
    let mut events: Vec<Vec<EventData>> = vec![Vec::new(); 500];

    // At second 29100 (index 300): Pink departs PinkStop, Green departs GreenStop
    events[300].push(EventData {
        stop_index: 0,     // PinkStop
        route_index: 0,    // Pink Line
        trip_index: 0,
        next_stop_index: 2, // Midway
        travel_time: 120,
    });
    events[300].push(EventData {
        stop_index: 1,     // GreenStop
        route_index: 1,    // Green Line
        trip_index: 1,
        next_stop_index: 2, // Midway
        travel_time: 120,
    });

    // At second 29220 (index 420): Green continues from Midway to Dest
    events[420].push(EventData {
        stop_index: 2,     // Midway
        route_index: 1,    // Green Line
        trip_index: 1,
        next_stop_index: 3, // Dest
        travel_time: 120,
    });

    let pattern = PatternData {
        day_mask: 0xFF, // every day
        min_time,
        max_time: min_time + 500,
        events,
        frequency_routes: vec![],
    };

    let num_nodes = 5;
    let num_edges = 4;

    let mut adj: Vec<Vec<(u32, f32)>> = vec![Vec::new(); num_nodes];
    for edge in &edges {
        adj[edge.u as usize].push((edge.v, edge.distance_meters));
        adj[edge.v as usize].push((edge.u, edge.distance_meters));
    }

    let mut node_is_stop = vec![false; num_nodes];
    let mut node_stop_indices: Vec<Vec<u32>> = vec![Vec::new(); num_nodes];
    for (si, &ni) in stop_node_map.iter().enumerate() {
        node_is_stop[ni as usize] = true;
        node_stop_indices[ni as usize].push(si as u32);
    }

    PreparedData {
        nodes,
        edges,
        stops,
        stop_node_map,
        route_names,
        patterns: vec![pattern],
        num_nodes,
        num_edges,
        num_stops: 4,
        adj,
        node_is_stop,
        node_stop_indices,
        shapes: HashMap::new(),
        route_shapes: vec![Vec::new(); 2],
    }
}

#[test]
fn test_trip_following_prefers_direct_green() {
    let data = build_test_data();
    let departure_time = 28800u32; // 8:00 AM
    let source_node = 0u32;
    let transfer_slack = 60u32;
    let max_time = 3600u32;

    let result = run_tdd_multi(&data, source_node, departure_time, &[0], transfer_slack, max_time);

    let dest_node = 4u32; // Dest stop node

    // Dest should be reachable
    assert_ne!(result[dest_node as usize].arrival_time, u32::MAX,
        "Destination should be reachable");

    let dest_r = &result[dest_node as usize];
    let midway_r = &result[2];

    eprintln!("Midway (node 2): arrival={}, leave_home={}, route={}, edge_type={}",
        midway_r.arrival_time, midway_r.leave_home, midway_r.route_index, midway_r.edge_type);
    eprintln!("Dest (node 4): arrival={}, leave_home={}, route={}, edge_type={}",
        dest_r.arrival_time, dest_r.leave_home, dest_r.route_index, dest_r.edge_type);

    // Direct Green should reach Dest:
    //   Walk 180s (arrive GreenStop at 28980), board Green @29100,
    //   ride through Midway @29220, arrive Dest @29340
    //   leave_home = 29100 - 180 = 28920
    //
    // Pink→Green would be:
    //   Walk 300s (arrive PinkStop at 29100), board Pink @29100,
    //   arrive Midway @29220, transfer slack 60s → board Green @29280,
    //   but Green already departed Midway at 29220, so must wait for next Green...
    //   which doesn't exist in our test data. So Pink→Green can't reach Dest via transit.
    //   (It can reach Dest by walking from Midway: 29220 + 420 = 29640)
    //
    // Direct Green arrives at Dest at 29340, which is earlier than walk (29640).
    // The key: Green must ride THROUGH Midway (node 2) even though Pink reached Midway
    // earlier (Pink reaches Midway at 29220 via transit, same as Green).
    // Actually both reach Midway at 29220 but via different routes.

    // The direct Green should arrive at Dest via transit at 29340
    assert_eq!(dest_r.arrival_time, 29340,
        "Direct Green should arrive at 29340 (28800+180walk+120green+120green)... wait, \
         let me recalculate. Board Green at 29100, travel 120 to Midway (29220), \
         travel 120 to Dest (29340). Expected 29340, got {}",
        dest_r.arrival_time);

    // Should have come via Green Line (route_index=1)
    assert_eq!(dest_r.route_index, 1,
        "Dest should be reached via Green Line (route 1), got route {}",
        dest_r.route_index);

    // leave_home should be 28920 (boarded Green at 29100, walked 180s)
    assert_eq!(dest_r.leave_home, 28920,
        "leave_home should be 28920, got {}", dest_r.leave_home);
}

#[test]
fn test_trip_following_better_leave_home() {
    // Same setup but both paths reach Dest at the same time.
    // Green direct: board @29100, midway @29220, dest @29340, leave_home=28920
    // Pink→Green: board Pink @29100, midway @29220, wait for Green @29220 (same route continuation??)
    //
    // Actually let's make it so both CAN reach dest:
    // Add a second Green trip at 29280 from Midway so Pink→Green transfer works.
    let mut data = build_test_data();

    // Add second Green trip departing Midway at 29280 → Dest in 60s (arrives 29340)
    // This means Pink→Green also arrives at 29340 but with worse leave_home
    let pat = &mut data.patterns[0];
    let idx = (29280 - pat.min_time) as usize; // index 480
    pat.events[idx].push(EventData {
        stop_index: 2,     // Midway
        route_index: 1,    // Green Line
        trip_index: 2,     // different trip
        next_stop_index: 3, // Dest
        travel_time: 60,   // faster train, same arrival
    });

    let departure_time = 28800u32;
    let result = run_tdd_multi(&data, 0, departure_time, &[0], 60, 3600);
    let dest_r = &result[4];

    eprintln!("Dest: arrival={}, leave_home={}, route={}, prev={}",
        dest_r.arrival_time, dest_r.leave_home, dest_r.route_index, dest_r.prev_node);

    // Both paths arrive at 29340.
    // Direct Green: leave_home = 29100 - 180 = 28920
    // Pink→Green: leave_home = 29100 - 300 = 28800
    // Should prefer Direct Green (leave_home 28920 > 28800)
    assert_eq!(dest_r.arrival_time, 29340);
    assert_eq!(dest_r.leave_home, 28920,
        "Should prefer direct Green path with leave_home=28920 (leave later), got {}",
        dest_r.leave_home);
}

#[test]
fn test_path_reconstruction_through_blocked_stop() {
    // Green rides through Midway (which is "blocked" by Pink arriving earlier).
    // Path reconstruction from Dest should trace back through the Green Line trip,
    // NOT through the Pink path at Midway.
    let data = build_test_data();
    let departure_time = 28800u32;
    let result = run_tdd_multi(&data, 0, departure_time, &[0], 60, 3600);

    let dest_node = 4u32;
    assert_ne!(result[dest_node as usize].arrival_time, u32::MAX);

    // Trace path from Dest back to source
    let mut path = Vec::new();
    let mut current = dest_node;
    loop {
        let r = &result[current as usize];
        path.push((current, r.edge_type, r.route_index));
        if r.prev_node == u32::MAX || r.prev_node == current {
            break;
        }
        current = r.prev_node;
    }
    path.reverse();

    eprintln!("Path:");
    for &(node, edge_type, route) in &path {
        let et = if edge_type == 0 { "walk" } else { "transit" };
        eprintln!("  node={} type={} route={}", node, et, route);
    }

    // Dest (node 4) should be reached via Green Line (route 1)
    let dest_r = &result[dest_node as usize];
    assert_eq!(dest_r.route_index, 1, "Dest should be via Green Line");

    // prev_node of Dest should be a node on the Green Line trip path,
    // NOT node 2 (Midway) if Midway is on a different path (Pink).
    // With trip-following: prev_node should be either node 1 (GreenStop, boarding)
    // or node 2 (Midway, if it was updated by Green too).
    // The key: if we trace from Dest to source, we should NOT go through
    // a Pink Line segment.
    let has_pink = path.iter().any(|&(_, _, route)| route == 0);
    assert!(!has_pink,
        "Path to Dest should NOT include Pink Line (route 0), found: {:?}", path);
}
