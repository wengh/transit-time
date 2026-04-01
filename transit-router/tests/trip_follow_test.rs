use transit_router::data::*;
use transit_router::router::*;
use std::collections::HashMap;

fn build_test_data(add_extra_green: bool) -> PreparedData {
    let nodes = vec![
        NodeData { lat: 0.0, lon: 0.0 },
        NodeData { lat: 0.001, lon: 0.0 },
        NodeData { lat: 0.002, lon: 0.0 },
        NodeData { lat: 0.0, lon: 0.001 },
        NodeData { lat: 0.003, lon: 0.0 },
    ];

    let edges = vec![
        EdgeData { u: 0, v: 1, distance_meters: 252.0 },
        EdgeData { u: 0, v: 3, distance_meters: 420.0 },
        EdgeData { u: 3, v: 2, distance_meters: 420.0 },
        EdgeData { u: 1, v: 2, distance_meters: 588.0 },
        EdgeData { u: 2, v: 4, distance_meters: 588.0 },
    ];

    let stops = vec![
        StopData { lat: 0.0, lon: 0.0, name: "PinkStop".into() },
        StopData { lat: 0.0, lon: 0.0, name: "GreenStop".into() },
        StopData { lat: 0.0, lon: 0.0, name: "Midway".into() },
        StopData { lat: 0.0, lon: 0.0, name: "Dest".into() },
    ];
    let stop_node_map = vec![3, 1, 2, 4];
    let num_stops = 4;

    let route_names = vec!["Pink Line".into(), "Green Line".into()];

    let min_time = 28800u32;
    let mut flat_events = Vec::new();

    // Pink Stop 0 -> Stop 2
    flat_events.push(EventData {
        time_offset: 300,
        stop_index: 0,
        route_index: 0,
        trip_index: 0,
        travel_time: 120,
        next_event_index: u32::MAX,
    });
    // Pink arrival Sentinel (Stop 2)
    flat_events.push(EventData {
        time_offset: 420,
        stop_index: 2,
        route_index: 0,
        trip_index: 0,
        travel_time: 0,
        next_event_index: u32::MAX,
    });
    
    // Green
    flat_events.push(EventData {
        time_offset: 300,
        stop_index: 1,
        route_index: 1,
        trip_index: 1,
        travel_time: 120,
        next_event_index: u32::MAX,
    });

    flat_events.push(EventData {
        time_offset: 420,
        stop_index: 2,
        route_index: 1,
        trip_index: 1,
        travel_time: 120,
        next_event_index: u32::MAX,
    });

    // Green arrival Sentinel (Stop 3 / Dest)
    flat_events.push(EventData {
        time_offset: 540,
        stop_index: 3,
        route_index: 1,
        trip_index: 1,
        travel_time: 0,
        next_event_index: u32::MAX,
    });

    if add_extra_green {
        flat_events.push(EventData {
            time_offset: 480,
            stop_index: 2,
            route_index: 1,
            trip_index: 2,
            travel_time: 60,
            next_event_index: u32::MAX,
        });
        flat_events.push(EventData {
            time_offset: 540,
            stop_index: 3,
            route_index: 1,
            trip_index: 2,
            travel_time: 0,
            next_event_index: u32::MAX,
        });
    }

    flat_events.sort_unstable_by_key(|e| e.time_offset);
    let mut events_by_stop = JaggedArray::build(flat_events, |e| e.stop_index, num_stops as u32);

    let mut links: Vec<(u32, u32, u32)> = events_by_stop
        .data
        .iter()
        .enumerate()
        .map(|(i, e)| (e.trip_index, e.time_offset, i as u32))
        .collect();
    links.sort_unstable();

    for w in links.windows(2) {
        if w[0].0 == w[1].0 {
            events_by_stop.data[w[0].2 as usize].next_event_index = w[1].2;
        }
    }

    let pattern = PatternData {
        day_mask: 0xFF,
        min_time,
        max_time: min_time + 1000,
        frequency_routes: vec![],
        stop_index: PatternStopIndex {
            freq_by_stop: JaggedArray::build(vec![], |_| 0, num_stops as u32),
            events_by_stop,
        },
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
