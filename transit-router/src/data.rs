use std::io::Read;

#[derive(Debug, Clone)]
pub struct NodeData {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone)]
pub struct EdgeData {
    pub u: u32,
    pub v: u32,
    pub distance_meters: f32,
}

#[derive(Debug, Clone)]
pub struct StopData {
    pub lat: f64,
    pub lon: f64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct EventData {
    pub stop_index: u32,
    pub route_index: u32,
    pub trip_index: u32,
    pub next_stop_index: u32,
    pub travel_time: u32,
}

#[derive(Debug, Clone)]
pub struct FreqData {
    pub route_index: u32,
    pub stop_index: u32,
    pub start_time: u32,
    pub end_time: u32,
    pub headway_secs: u32,
    pub next_stop_index: u32,
    pub travel_time: u32,
}

#[derive(Debug, Clone)]
pub struct PatternData {
    pub day_mask: u8,
    pub min_time: u32,
    pub max_time: u32,
    pub events: Vec<Vec<EventData>>,
    pub frequency_routes: Vec<FreqData>,
}

pub struct PreparedData {
    pub nodes: Vec<NodeData>,
    pub edges: Vec<EdgeData>,
    pub stops: Vec<StopData>,
    pub stop_node_map: Vec<u32>, // stop_index -> node_index
    pub route_names: Vec<String>,
    pub patterns: Vec<PatternData>,
    pub num_nodes: usize,
    pub num_edges: usize,
    pub num_stops: usize,
    pub adj: Vec<Vec<(u32, f32)>>,
    pub node_is_stop: Vec<bool>,
    pub node_stop_indices: Vec<Vec<u32>>,
    /// shape_id -> [(lat, lon)]
    pub shapes: std::collections::HashMap<String, Vec<(f64, f64)>>,
    /// route_index -> [shape_ids] (all shapes for that route)
    pub route_shapes: Vec<Vec<String>>,
}

pub fn load(compressed: &[u8]) -> Result<PreparedData, String> {
    let mut decoder = flate2::read::GzDecoder::new(compressed);
    let mut buf = Vec::new();
    decoder
        .read_to_end(&mut buf)
        .map_err(|e| format!("Decompression failed: {}", e))?;

    let mut pos = 0;

    // Header
    if &buf[pos..pos + 4] != b"TRNS" {
        return Err("Invalid magic".to_string());
    }
    pos += 4;
    let version = read_u32(&buf, &mut pos);
    if version != 1 {
        return Err(format!("Unsupported version {}", version));
    }
    let num_nodes = read_u32(&buf, &mut pos) as usize;
    let num_edges = read_u32(&buf, &mut pos) as usize;
    let num_stops = read_u32(&buf, &mut pos) as usize;
    let num_stop_to_node = read_u32(&buf, &mut pos) as usize;
    let num_patterns = read_u32(&buf, &mut pos) as usize;
    let num_route_names = read_u32(&buf, &mut pos) as usize;
    let num_shapes = read_u32(&buf, &mut pos) as usize;

    // Nodes
    let mut nodes = Vec::with_capacity(num_nodes);
    for _ in 0..num_nodes {
        let lat = read_f64(&buf, &mut pos);
        let lon = read_f64(&buf, &mut pos);
        nodes.push(NodeData { lat, lon });
    }

    // Edges
    let mut edges = Vec::with_capacity(num_edges);
    for _ in 0..num_edges {
        let u = read_u32(&buf, &mut pos);
        let v = read_u32(&buf, &mut pos);
        let distance = read_f32(&buf, &mut pos);
        edges.push(EdgeData {
            u,
            v,
            distance_meters: distance,
        });
    }

    // Stops
    let mut stops = Vec::with_capacity(num_stops);
    for _ in 0..num_stops {
        let lat = read_f64(&buf, &mut pos);
        let lon = read_f64(&buf, &mut pos);
        let name_len = read_u32(&buf, &mut pos) as usize;
        let name = String::from_utf8_lossy(&buf[pos..pos + name_len]).to_string();
        pos += name_len;
        stops.push(StopData { lat, lon, name });
    }

    // Stop-to-node mapping
    let mut stop_node_map = vec![u32::MAX; num_stops];
    let mut node_is_stop = vec![false; num_nodes];
    let mut node_stop_indices: Vec<Vec<u32>> = vec![Vec::new(); num_nodes];
    for _ in 0..num_stop_to_node {
        let stop_idx = read_u32(&buf, &mut pos);
        let node_idx = read_u32(&buf, &mut pos);
        if (stop_idx as usize) < num_stops && (node_idx as usize) < num_nodes {
            stop_node_map[stop_idx as usize] = node_idx;
            node_is_stop[node_idx as usize] = true;
            node_stop_indices[node_idx as usize].push(stop_idx);
        }
    }

    // Route names
    let mut route_names = Vec::with_capacity(num_route_names);
    for _ in 0..num_route_names {
        let name_len = read_u32(&buf, &mut pos) as usize;
        let name = String::from_utf8_lossy(&buf[pos..pos + name_len]).to_string();
        pos += name_len;
        route_names.push(name);
    }

    // Patterns
    let mut patterns = Vec::with_capacity(num_patterns);
    for _ in 0..num_patterns {
        let _pattern_id = read_u32(&buf, &mut pos);
        let day_mask = buf[pos];
        pos += 1;
        let num_add = read_u32(&buf, &mut pos) as usize;
        pos += num_add * 4; // skip date exceptions for now
        let num_remove = read_u32(&buf, &mut pos) as usize;
        pos += num_remove * 4;
        let min_time = read_u32(&buf, &mut pos);
        let max_time = read_u32(&buf, &mut pos);
        let event_array_len = read_u32(&buf, &mut pos) as usize;
        let mut events = Vec::with_capacity(event_array_len);
        for _ in 0..event_array_len {
            let n = read_u16(&buf, &mut pos) as usize;
            let mut second_events = Vec::with_capacity(n);
            for _ in 0..n {
                let stop_index = read_u32(&buf, &mut pos);
                let route_index = read_u32(&buf, &mut pos);
                let trip_index = read_u32(&buf, &mut pos);
                let next_stop_index = read_u32(&buf, &mut pos);
                let travel_time = read_u32(&buf, &mut pos);
                second_events.push(EventData {
                    stop_index,
                    route_index,
                    trip_index,
                    next_stop_index,
                    travel_time,
                });
            }
            events.push(second_events);
        }
        let num_freq = read_u32(&buf, &mut pos) as usize;
        let mut freq_entries = Vec::with_capacity(num_freq);
        for _ in 0..num_freq {
            let route_index = read_u32(&buf, &mut pos);
            let stop_index = read_u32(&buf, &mut pos);
            let start_time = read_u32(&buf, &mut pos);
            let end_time = read_u32(&buf, &mut pos);
            let headway_secs = read_u32(&buf, &mut pos);
            let next_stop_index = read_u32(&buf, &mut pos);
            let travel_time = read_u32(&buf, &mut pos);
            freq_entries.push(FreqData {
                route_index,
                stop_index,
                start_time,
                end_time,
                headway_secs,
                next_stop_index,
                travel_time,
            });
        }
        patterns.push(PatternData {
            day_mask,
            min_time,
            max_time,
            events,
            frequency_routes: freq_entries,
        });
    }

    // Shapes
    let mut shapes = std::collections::HashMap::new();
    for _ in 0..num_shapes {
        let id_len = read_u32(&buf, &mut pos) as usize;
        let shape_id = String::from_utf8_lossy(&buf[pos..pos + id_len]).to_string();
        pos += id_len;
        let num_points = read_u32(&buf, &mut pos) as usize;
        let mut points = Vec::with_capacity(num_points);
        for _ in 0..num_points {
            let lat = read_f64(&buf, &mut pos);
            let lon = read_f64(&buf, &mut pos);
            points.push((lat, lon));
        }
        shapes.insert(shape_id, points);
    }

    // Route-to-shape mapping (may not be present in older binaries)
    let mut route_shapes: Vec<Vec<String>> = vec![Vec::new(); num_route_names];
    if pos < buf.len() {
        let num_route_shapes = read_u32(&buf, &mut pos) as usize;
        for i in 0..num_route_shapes {
            let num_shapes = read_u32(&buf, &mut pos) as usize;
            let mut shapes_for_route = Vec::with_capacity(num_shapes);
            for _ in 0..num_shapes {
                let id_len = read_u32(&buf, &mut pos) as usize;
                if id_len > 0 {
                    shapes_for_route.push(
                        String::from_utf8_lossy(&buf[pos..pos + id_len]).to_string(),
                    );
                }
                pos += id_len;
            }
            route_shapes[i] = shapes_for_route;
        }
    }

    // Build adjacency list
    let mut adj: Vec<Vec<(u32, f32)>> = vec![Vec::new(); num_nodes];
    for edge in &edges {
        adj[edge.u as usize].push((edge.v, edge.distance_meters));
        adj[edge.v as usize].push((edge.u, edge.distance_meters));
    }

    Ok(PreparedData {
        nodes,
        edges,
        stops,
        stop_node_map,
        route_names,
        patterns,
        num_nodes,
        num_edges,
        num_stops,
        adj,
        node_is_stop,
        node_stop_indices,
        shapes,
        route_shapes,
    })
}

fn read_u32(buf: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_le_bytes(buf[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    v
}

fn read_u16(buf: &[u8], pos: &mut usize) -> u16 {
    let v = u16::from_le_bytes(buf[*pos..*pos + 2].try_into().unwrap());
    *pos += 2;
    v
}

fn read_f32(buf: &[u8], pos: &mut usize) -> f32 {
    let v = f32::from_le_bytes(buf[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    v
}

fn read_f64(buf: &[u8], pos: &mut usize) -> f64 {
    let v = f64::from_le_bytes(buf[*pos..*pos + 8].try_into().unwrap());
    *pos += 8;
    v
}
