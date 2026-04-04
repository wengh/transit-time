use std::io::Read;

#[derive(Debug, Clone, Copy)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub fn to_hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

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
    pub time_offset: u32,
    pub stop_index: u32,
    pub route_index: u32,
    pub trip_index: u32,
    pub travel_time: u32,
    pub next_event_index: u32, // u32::MAX if it's the last event in the trip
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
pub struct JaggedArray<T> {
    pub offsets: Vec<u32>,
    pub data: Vec<T>,
}

impl<T> std::ops::Index<u32> for JaggedArray<T> {
    type Output = [T];

    fn index(&self, index: u32) -> &Self::Output {
        let start = self.offsets[index as usize] as usize;
        let end = self.offsets[index as usize + 1] as usize;
        &self.data[start..end]
    }
}

impl<T> JaggedArray<T> {
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn build(items: Vec<T>, key_fn: impl Fn(&T) -> u32, len: u32) -> Self {
        let mut buckets: Vec<Vec<T>> = std::iter::repeat_with(Vec::new)
            .take(len as usize)
            .collect();
        let total_items = items.len();

        for item in items {
            let bucket = key_fn(&item) as usize;
            if bucket < len as usize {
                buckets[bucket].push(item);
            }
        }

        let mut offsets = Vec::with_capacity(buckets.len() + 1);
        let mut data = Vec::with_capacity(total_items);

        for mut bucket in buckets {
            offsets.push(data.len() as u32);
            data.append(&mut bucket);
        }
        offsets.push(data.len() as u32);

        Self { offsets, data }
    }
}

pub struct PatternStopIndex {
    pub freq_by_stop: JaggedArray<u32>,
    pub events_by_stop: JaggedArray<EventData>,
}

pub struct PatternData {
    pub day_mask: u8,
    pub start_date: u32, // YYYYMMDD, 0 = unbounded
    pub end_date: u32,   // YYYYMMDD, 0 = unbounded
    pub date_exceptions_add: Vec<u32>,
    pub date_exceptions_remove: Vec<u32>,
    pub min_time: u32,
    pub max_time: u32,
    pub frequency_routes: Vec<FreqData>,
    pub stop_index: PatternStopIndex,
}

pub struct PreparedData {
    pub nodes: Vec<NodeData>,
    pub edges: Vec<EdgeData>,
    pub stops: Vec<StopData>,
    pub stop_node_map: Vec<u32>, // stop_index -> node_index
    pub route_names: Vec<String>,
    pub route_colors: Vec<Option<Color>>,
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
    /// Spatial grid index: (lat_cell, lon_cell) -> [node_indices]
    pub node_grid: std::collections::HashMap<(i32, i32), Vec<u32>>,
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
    if version != 2 {
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

    // Route colors
    let mut route_colors = Vec::with_capacity(num_route_names);
    for _ in 0..num_route_names {
        let has_color = buf[pos];
        pos += 1;
        if has_color != 0 {
            let r = buf[pos];
            pos += 1;
            let g = buf[pos];
            pos += 1;
            let b = buf[pos];
            pos += 1;
            route_colors.push(Some(Color { r, g, b }));
        } else {
            route_colors.push(None);
        }
    }

    // Patterns
    let mut patterns = Vec::with_capacity(num_patterns);
    for _ in 0..num_patterns {
        let _pattern_id = read_u32(&buf, &mut pos);
        let day_mask = buf[pos];
        pos += 1;
        let start_date = read_u32(&buf, &mut pos);
        let end_date = read_u32(&buf, &mut pos);
        let num_add = read_u32(&buf, &mut pos) as usize;
        let mut date_exceptions_add = Vec::with_capacity(num_add);
        for _ in 0..num_add {
            date_exceptions_add.push(read_u32(&buf, &mut pos));
        }
        let num_remove = read_u32(&buf, &mut pos) as usize;
        let mut date_exceptions_remove = Vec::with_capacity(num_remove);
        for _ in 0..num_remove {
            date_exceptions_remove.push(read_u32(&buf, &mut pos));
        }
        let min_time = read_u32(&buf, &mut pos);
        let max_time = read_u32(&buf, &mut pos);

        let num_flat_events = read_u32(&buf, &mut pos) as usize;

        struct RawEvent {
            time_offset: u32,
            stop_index: u32,
            route_index: u32,
            trip_index: u32,
            next_stop_index: u32,
            travel_time: u32,
        }

        let time_offsets = read_pco_u32(&buf, &mut pos)?;
        let stop_indices = read_pco_u32(&buf, &mut pos)?;
        let route_indices = read_pco_u32(&buf, &mut pos)?;
        let trip_indices = read_pco_u32(&buf, &mut pos)?;
        let next_stop_indices = read_pco_u32(&buf, &mut pos)?;
        let travel_times = read_pco_u32(&buf, &mut pos)?;

        let mut raw_events: Vec<RawEvent> = (0..num_flat_events)
            .map(|i| RawEvent {
                time_offset: time_offsets[i],
                stop_index: stop_indices[i],
                route_index: route_indices[i],
                trip_index: trip_indices[i],
                next_stop_index: next_stop_indices[i],
                travel_time: travel_times[i],
            })
            .collect();

        // Sort by trip to identify the end of each trip and attach sentinels
        raw_events.sort_unstable_by_key(|e| (e.trip_index, e.time_offset));

        let mut events_pre = Vec::with_capacity(num_flat_events + (num_flat_events / 10));

        for i in 0..num_flat_events {
            let r = &raw_events[i];
            events_pre.push(EventData {
                time_offset: r.time_offset,
                stop_index: r.stop_index,
                route_index: r.route_index,
                trip_index: r.trip_index,
                travel_time: r.travel_time,
                next_event_index: u32::MAX, // Set after bucketing
            });

            let is_last = i + 1 == num_flat_events || raw_events[i + 1].trip_index != r.trip_index;
            if is_last && r.travel_time > 0 {
                // Append sentinel arrival event for the final stop
                events_pre.push(EventData {
                    time_offset: r.time_offset + r.travel_time,
                    stop_index: r.next_stop_index,
                    route_index: r.route_index,
                    trip_index: r.trip_index,
                    travel_time: 0,
                    next_event_index: u32::MAX,
                });
            }
        }

        // Must sort by time_offset so events_by_stop buckets naturally provide binary search ordered arrays
        events_pre.sort_unstable_by_key(|e| e.time_offset);

        let mut events_by_stop = JaggedArray::build(events_pre, |e| e.stop_index, num_stops as u32);

        // Dynamically link next_event_index for O(1) trip following!
        // We know its precise globally bucketted flat index.
        let mut links: Vec<(u32, u32, u32)> = events_by_stop
            .data
            .iter()
            .enumerate()
            .map(|(i, e)| (e.trip_index, e.time_offset, i as u32))
            .collect();

        // Sort just the tuples by Trip then Time!
        links.sort_unstable();

        for w in links.windows(2) {
            if w[0].0 == w[1].0 {
                events_by_stop.data[w[0].2 as usize].next_event_index = w[1].2;
            }
        }

        let num_freq = read_u32(&buf, &mut pos) as usize;
        let mut freq_entries = Vec::with_capacity(num_freq);
        let mut freq_indices = Vec::with_capacity(num_freq);

        for i in 0..num_freq {
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
            freq_indices.push(i as u32);
        }

        let freq_by_stop = JaggedArray::build(
            freq_indices,
            |&i| freq_entries[i as usize].stop_index,
            num_stops as u32,
        );

        patterns.push(PatternData {
            day_mask,
            start_date,
            end_date,
            date_exceptions_add,
            date_exceptions_remove,
            min_time,
            max_time,
            frequency_routes: freq_entries,
            stop_index: PatternStopIndex {
                freq_by_stop,
                events_by_stop,
            },
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
                    shapes_for_route
                        .push(String::from_utf8_lossy(&buf[pos..pos + id_len]).to_string());
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

    // Build spatial grid index for snap_to_node
    const CELL_SIZE_LAT: f64 = 0.0045;
    const CELL_SIZE_LON: f64 = 0.006;
    let mut node_grid: std::collections::HashMap<(i32, i32), Vec<u32>> =
        std::collections::HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        let cell = (
            (node.lat / CELL_SIZE_LAT).floor() as i32,
            (node.lon / CELL_SIZE_LON).floor() as i32,
        );
        node_grid.entry(cell).or_default().push(i as u32);
    }

    Ok(PreparedData {
        nodes,
        edges,
        stops,
        stop_node_map,
        route_names,
        route_colors,
        patterns,
        num_nodes,
        num_edges,
        num_stops,
        adj,
        node_is_stop,
        node_stop_indices,
        shapes,
        route_shapes,
        node_grid,
    })
}

fn read_pco_u32(buf: &[u8], pos: &mut usize) -> Result<Vec<u32>, String> {
    let pco_len = read_u32(buf, pos) as usize;
    let result: Vec<u32> = pco::standalone::simple_decompress(&buf[*pos..*pos + pco_len])
        .map_err(|e| format!("pco decompress failed: {}", e))?;
    *pos += pco_len;
    Ok(result)
}

fn read_u32(buf: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_le_bytes(buf[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
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
