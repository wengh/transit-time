use std::collections::HashMap;
use transit_router::data::*;
use transit_router::router::*;

/// Helper: build events sorted by (stop_index, time_offset) with next_event_index precomputed,
/// mirroring the v3 binary format.
/// Returns (events_by_stop, sentinel_routes) where sentinel_routes maps event indices to route indices.
fn build_events_by_stop(
    events_with_routes: Vec<(u32, EventData, u32)>, // (trip_id, event, route_index for sentinels)
    num_stops: u32,
) -> (JaggedArray<EventData>, HashMap<u32, u32>) {
    // Extract just the (trip_id, event) pairs for linking
    let mut flat_events: Vec<(u32, EventData)> = events_with_routes.iter()
        .map(|(trip, event, _)| (*trip, event.clone()))
        .collect();

    // Sort by trip then time to compute next_event_index
    flat_events.sort_unstable_by_key(|&(trip, ref e)| (trip, e.time_offset));

    // Assign temporary indices and compute next pointers
    let n = flat_events.len();
    let mut next_in_trip = vec![u32::MAX; n];
    for i in 0..n.saturating_sub(1) {
        if flat_events[i].0 == flat_events[i + 1].0 {
            next_in_trip[i] = (i + 1) as u32;
        }
    }

    // Sort by (stop_index, time_offset) and remap next_event_index
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_unstable_by_key(|&i| {
        let e = &flat_events[i as usize].1;
        (e.stop_index, e.time_offset)
    });
    let mut inv = vec![0u32; n];
    for (new_pos, &old_pos) in order.iter().enumerate() {
        inv[old_pos as usize] = new_pos as u32;
    }

    let data: Vec<EventData> = order
        .iter()
        .map(|&i| {
            let e = &flat_events[i as usize].1;
            let nei = next_in_trip[i as usize];
            EventData {
                time_offset: e.time_offset,
                stop_index: e.stop_index,
                travel_time: e.travel_time,
                next_event_index: if nei == u32::MAX { u32::MAX } else { inv[nei as usize] },
            }
        })
        .collect();

    // Build sentinel_routes: map event indices to route indices for sentinels
    let mut sentinel_routes = HashMap::new();
    for (new_pos, &old_pos) in order.iter().enumerate() {
        let route_idx = events_with_routes[old_pos as usize].2;
        if route_idx != 0 {
            sentinel_routes.insert(new_pos as u32, route_idx);
        }
    }

    // Compute offsets
    let mut offsets = vec![0u32; num_stops as usize + 1];
    for e in &data {
        if e.stop_index < num_stops {
            offsets[e.stop_index as usize + 1] += 1;
        }
    }
    for i in 1..offsets.len() {
        offsets[i] += offsets[i - 1];
    }

    (JaggedArray { offsets, data }, sentinel_routes)
}

fn build_test_data(add_extra_green: bool) -> PreparedData {
    let nodes = vec![
        NodeData { lat: 0.0, lon: 0.0 },
        NodeData {
            lat: 0.001,
            lon: 0.0,
        },
        NodeData {
            lat: 0.002,
            lon: 0.0,
        },
        NodeData {
            lat: 0.0,
            lon: 0.001,
        },
        NodeData {
            lat: 0.003,
            lon: 0.0,
        },
    ];

    let edges = vec![
        EdgeData {
            u: 0,
            v: 1,
            distance_meters: 252.0,
        },
        EdgeData {
            u: 0,
            v: 3,
            distance_meters: 420.0,
        },
        EdgeData {
            u: 3,
            v: 2,
            distance_meters: 420.0,
        },
        EdgeData {
            u: 1,
            v: 2,
            distance_meters: 588.0,
        },
        EdgeData {
            u: 2,
            v: 4,
            distance_meters: 588.0,
        },
    ];

    let stops = vec![
        StopData {
            lat: 0.0,
            lon: 0.0,
            name: "PinkStop".into(),
        },
        StopData {
            lat: 0.0,
            lon: 0.0,
            name: "GreenStop".into(),
        },
        StopData {
            lat: 0.0,
            lon: 0.0,
            name: "Midway".into(),
        },
        StopData {
            lat: 0.0,
            lon: 0.0,
            name: "Dest".into(),
        },
    ];
    let stop_node_map = vec![3, 1, 2, 4];
    let num_stops = 4u32;

    let route_names = vec!["Pink Line".into(), "Green Line".into()];
    let route_colors = vec![None, None];

    let min_time = 28800u32;

    // (trip_id, EventData, route_index_if_sentinel) triples
    let mut events = vec![
        // Pink: Stop 0 -> Stop 2
        (0u32, EventData {
            time_offset: 300, stop_index: 0,
            travel_time: 120, next_event_index: u32::MAX,
        }, 0),
        (0, EventData {
            time_offset: 420, stop_index: 2,
            travel_time: 0, next_event_index: u32::MAX,
        }, 0), // sentinel, route from Pink
        // Green: Stop 1 -> Stop 2 -> Stop 3
        (1, EventData {
            time_offset: 300, stop_index: 1,
            travel_time: 120, next_event_index: u32::MAX,
        }, 0),
        (1, EventData {
            time_offset: 420, stop_index: 2,
            travel_time: 120, next_event_index: u32::MAX,
        }, 0),
        (1, EventData {
            time_offset: 540, stop_index: 3,
            travel_time: 0, next_event_index: u32::MAX,
        }, 1), // sentinel, route index 1 (Green)
    ];

    if add_extra_green {
        events.push((2, EventData {
            time_offset: 480, stop_index: 2,
            travel_time: 60, next_event_index: u32::MAX,
        }, 0));
        events.push((2, EventData {
            time_offset: 540, stop_index: 3,
            travel_time: 0, next_event_index: u32::MAX,
        }, 1)); // sentinel, route index 1 (Green)
    }

    let (events_by_stop, sentinel_routes) = build_events_by_stop(events, num_stops);

    let pattern = PatternData {
        day_mask: 0xFF,
        start_date: 0,
        end_date: 0,
        date_exceptions_add: vec![],
        date_exceptions_remove: vec![],
        min_time,
        max_time: min_time + 1000,
        frequency_routes: vec![],
        stop_index: PatternStopIndex {
            freq_by_stop: JaggedArray::build(vec![], |_| 0, num_stops),
            events_by_stop,
        },
        sentinel_routes,
    };

    let num_nodes = 5;
    let num_edges = 5;

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

    const CELL_SIZE_LAT: f64 = 0.0045;
    const CELL_SIZE_LON: f64 = 0.006;
    let mut node_grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        let cell = (
            (node.lat / CELL_SIZE_LAT).floor() as i32,
            (node.lon / CELL_SIZE_LON).floor() as i32,
        );
        node_grid.entry(cell).or_default().push(i as u32);
    }

    PreparedData {
        nodes,
        edges,
        stops,
        stop_node_map,
        route_names,
        route_colors,
        patterns: vec![pattern],
        num_nodes,
        num_edges,
        num_stops: 4,
        adj,
        node_is_stop,
        node_stop_indices,
        shapes: HashMap::new(),
        route_shapes: vec![Vec::new(); 2],
        node_grid,
    }
}

#[test]
fn test_trip_following_prefers_direct_green() {
    let data = build_test_data(false);
    let departure_time = 28800u32;
    let result = run_tdd_multi(&data, 0, departure_time, &[0], 60, 3600);
    assert_ne!(result[4].arrival_time, u32::MAX);
    assert_eq!(result[4].route_index, 1);
}

#[test]
fn test_trip_following_better_leave_home() {
    let data = build_test_data(true);
    let departure_time = 28800u32;
    let result = run_tdd_multi(&data, 0, departure_time, &[0], 60, 3600);
    assert_eq!(result[4].arrival_time, 29340);
    assert_eq!(result[4].leave_home, 28920);
}

#[test]
fn test_path_reconstruction_through_blocked_stop() {
    let data = build_test_data(false);
    let departure_time = 28800u32;
    let result = run_tdd_multi(&data, 0, departure_time, &[0], 60, 3600);

    let mut path = Vec::new();
    let mut current = 4u32;
    loop {
        let r = &result[current as usize];
        path.push((current, r.edge_type, r.route_index));
        if r.prev_node == u32::MAX || r.prev_node == current {
            break;
        }
        current = r.prev_node;
    }
    path.reverse();

    let has_pink = path.iter().any(|&(_, _, route)| route == 0);
    assert!(!has_pink);
}
