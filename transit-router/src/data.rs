use std::io::Read;
use std::time::{Duration, Instant};

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
    pub fn len(&self) -> u32 {
        (self.offsets.len() - 1) as u32
    }

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
    /// Maps flat event index to route_index for sentinel events (travel_time == 0)
    pub sentinel_routes: std::collections::HashMap<u32, u32>,
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
    /// Compressed shapes: JaggedArray of PCO-compressed data (lat/lon pairs as f32 bits)
    pub shapes: JaggedArray<u8>,
    /// route_index -> [shape_indices]
    pub route_shapes: Vec<Vec<u32>>,
    /// Spatial grid index: (lat_cell, lon_cell) -> [node_indices]
    pub node_grid: std::collections::HashMap<(i32, i32), Vec<u32>>,
}

pub fn load(compressed: &[u8]) -> Result<PreparedData, String> {
    #[cfg(target_arch = "wasm32")]
    web_sys::console::log_1(&"[load] Starting decompression".into());

    let mut decoder = flate2::read::GzDecoder::new(compressed);
    let mut buf = Vec::new();
    decoder
        .read_to_end(&mut buf)
        .map_err(|e| format!("Decompression failed: {}", e))?;

    #[cfg(target_arch = "wasm32")]
    web_sys::console::log_1(&format!("[load] Decompressed {} bytes", buf.len()).into());

    let mut pos = 0;

    // Header
    if &buf[pos..pos + 4] != b"TRNS" {
        return Err("Invalid magic".to_string());
    }
    pos += 4;
    let version = read_u32(&buf, &mut pos);
    if version != 3 {
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

        // v3: events are pre-sorted by (stop_index, time_offset) with sentinels
        // 4 PCO columns (time_offset, stop_index, travel_time, next_event_index) + stop_offsets + sentinel_routes
        let num_events = read_u32(&buf, &mut pos) as usize;

        let time_offsets = read_pco_u32(&buf, &mut pos)?;
        let stop_indices = read_pco_u32(&buf, &mut pos)?;
        let travel_times = read_pco_u32(&buf, &mut pos)?;
        let next_event_indices = read_pco_u32(&buf, &mut pos)?;
        let stop_offsets = read_pco_u32(&buf, &mut pos)?;
        let sentinel_route_indices = read_pco_u32(&buf, &mut pos)?;

        let data_vec: Vec<EventData> = (0..num_events)
            .map(|i| EventData {
                time_offset: time_offsets[i],
                stop_index: stop_indices[i],
                travel_time: travel_times[i],
                next_event_index: next_event_indices[i],
            })
            .collect();

        // Build sentinel_routes map: only events with travel_time == 0 and route_index > 0
        let mut pattern_sentinel_routes = std::collections::HashMap::new();
        for (i, route_idx) in sentinel_route_indices.iter().enumerate() {
            if *route_idx != 0 {
                pattern_sentinel_routes.insert(i as u32, *route_idx);
            }
        }

        let events_by_stop = JaggedArray {
            offsets: stop_offsets,
            data: data_vec,
        };

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
            sentinel_routes: pattern_sentinel_routes,
        });
    }

    // Shapes: stored as PCO-compressed data (lat/lon as f32 bits)
    let mut shapes_data: Vec<u8> = Vec::new();
    let mut shapes_offsets: Vec<u32> = vec![0];
    for shape_idx in 0..num_shapes {
        if pos + 4 > buf.len() {
            return Err(format!("Incomplete shape data at index {}", shape_idx));
        }
        let compressed_len = read_u32(&buf, &mut pos) as usize;
        if pos + compressed_len > buf.len() {
            return Err(format!(
                "Shape {} compressed data out of bounds: need {} bytes at pos {}, buf len {}",
                shape_idx, compressed_len, pos, buf.len()
            ));
        }
        shapes_data.extend_from_slice(&buf[pos..pos + compressed_len]);
        pos += compressed_len;
        shapes_offsets.push(shapes_data.len() as u32);
    }
    let shapes = JaggedArray {
        data: shapes_data,
        offsets: shapes_offsets,
    };

    // Route-to-shape mapping: indices instead of IDs (may not be present in older binaries)
    let mut route_shapes: Vec<Vec<u32>> = vec![Vec::new(); num_route_names];
    if pos + 4 <= buf.len() {
        let num_route_shapes = read_u32(&buf, &mut pos) as usize;
        for i in 0..num_route_shapes.min(num_route_names) {
            if pos + 4 > buf.len() {
                return Err(format!("Incomplete route_shapes data at route {}", i));
            }
            let num_shapes = read_u32(&buf, &mut pos) as usize;
            let mut shapes_for_route = Vec::with_capacity(num_shapes);
            for _ in 0..num_shapes {
                if pos + 4 > buf.len() {
                    return Err(format!("Incomplete shape index in route_shapes for route {}", i));
                }
                let shape_idx = read_u32(&buf, &mut pos);
                // Only keep valid shape indices (those that actually exist in the shapes array)
                if shape_idx != u32::MAX && (shape_idx as usize) < shapes.offsets.len() - 1 {
                    shapes_for_route.push(shape_idx);
                }
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

// === Stats instrumentation ===

pub struct LoadStats {
    pub compressed_size: usize,
    pub decompressed_size: usize,
    /// (section_name, binary_bytes)
    pub binary_sections: Vec<(&'static str, usize)>,
    /// (name, heap_bytes)
    pub memory_sections: Vec<(&'static str, usize)>,
    /// (phase_name, duration)
    pub timings: Vec<(&'static str, Duration)>,
    /// Counts for context
    pub counts: Vec<(&'static str, usize)>,
}

impl LoadStats {
    pub fn print(&self) {
        println!("=== Binary Section Sizes (decompressed) ===");
        println!("{:<25} {:>12} {:>8}", "Section", "Bytes", "% of total");
        for &(name, bytes) in &self.binary_sections {
            let pct = 100.0 * bytes as f64 / self.decompressed_size as f64;
            println!("{:<25} {:>12} {:>7.1}%", name, fmt_bytes(bytes), pct);
        }
        println!("{:<25} {:>12}", "TOTAL decompressed", fmt_bytes(self.decompressed_size));
        println!("{:<25} {:>12}", "TOTAL compressed (gzip)", fmt_bytes(self.compressed_size));
        println!();

        println!("=== In-Memory Sizes ===");
        let total_mem: usize = self.memory_sections.iter().map(|x| x.1).sum();
        println!("{:<25} {:>12} {:>8}", "Structure", "Bytes", "% of total");
        for &(name, bytes) in &self.memory_sections {
            let pct = 100.0 * bytes as f64 / total_mem as f64;
            println!("{:<25} {:>12} {:>7.1}%", name, fmt_bytes(bytes), pct);
        }
        println!("{:<25} {:>12}", "TOTAL in-memory", fmt_bytes(total_mem));
        println!();

        println!("=== Load Timings ===");
        let total_dur: Duration = self.timings.iter().map(|x| x.1).sum();
        println!("{:<25} {:>10} {:>8}", "Phase", "Time", "% of total");
        for &(name, dur) in &self.timings {
            let pct = 100.0 * dur.as_secs_f64() / total_dur.as_secs_f64();
            println!("{:<25} {:>10} {:>7.1}%", name, fmt_dur(dur), pct);
        }
        println!("{:<25} {:>10}", "TOTAL", fmt_dur(total_dur));
        println!();

        println!("=== Counts ===");
        for &(name, count) in &self.counts {
            println!("{:<25} {:>10}", name, count);
        }
    }
}

fn fmt_bytes(b: usize) -> String {
    if b >= 1_048_576 { format!("{:.2} MB", b as f64 / 1_048_576.0) }
    else if b >= 1024 { format!("{:.1} KB", b as f64 / 1024.0) }
    else { format!("{} B", b) }
}

fn fmt_dur(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms >= 1000.0 { format!("{:.2} s", ms / 1000.0) }
    else { format!("{:.1} ms", ms) }
}

pub fn load_with_stats(compressed: &[u8]) -> Result<(PreparedData, LoadStats), String> {
    let mut binary_sections: Vec<(&str, usize)> = Vec::new();
    let mut timings: Vec<(&str, Duration)> = Vec::new();

    let t0 = Instant::now();
    let mut decoder = flate2::read::GzDecoder::new(compressed);
    let mut buf = Vec::new();
    decoder
        .read_to_end(&mut buf)
        .map_err(|e| format!("Decompression failed: {}", e))?;
    timings.push(("decompress", t0.elapsed()));

    let mut pos = 0;

    // Header
    if &buf[pos..pos + 4] != b"TRNS" {
        return Err("Invalid magic".to_string());
    }
    pos += 4;
    let version = read_u32(&buf, &mut pos);
    if version != 3 {
        return Err(format!("Unsupported version {}", version));
    }
    let num_nodes = read_u32(&buf, &mut pos) as usize;
    let num_edges = read_u32(&buf, &mut pos) as usize;
    let num_stops = read_u32(&buf, &mut pos) as usize;
    let num_stop_to_node = read_u32(&buf, &mut pos) as usize;
    let num_patterns = read_u32(&buf, &mut pos) as usize;
    let num_route_names = read_u32(&buf, &mut pos) as usize;
    let num_shapes = read_u32(&buf, &mut pos) as usize;
    let header_end = pos;
    binary_sections.push(("header", header_end));

    // Nodes
    let t0 = Instant::now();
    let pos_before = pos;
    let mut nodes = Vec::with_capacity(num_nodes);
    for _ in 0..num_nodes {
        let lat = read_f64(&buf, &mut pos);
        let lon = read_f64(&buf, &mut pos);
        nodes.push(NodeData { lat, lon });
    }
    binary_sections.push(("nodes", pos - pos_before));
    timings.push(("parse nodes", t0.elapsed()));

    // Edges
    let t0 = Instant::now();
    let pos_before = pos;
    let mut edges = Vec::with_capacity(num_edges);
    for _ in 0..num_edges {
        let u = read_u32(&buf, &mut pos);
        let v = read_u32(&buf, &mut pos);
        let distance = read_f32(&buf, &mut pos);
        edges.push(EdgeData { u, v, distance_meters: distance });
    }
    binary_sections.push(("edges", pos - pos_before));
    timings.push(("parse edges", t0.elapsed()));

    // Stops
    let t0 = Instant::now();
    let pos_before = pos;
    let mut stops = Vec::with_capacity(num_stops);
    for _ in 0..num_stops {
        let lat = read_f64(&buf, &mut pos);
        let lon = read_f64(&buf, &mut pos);
        let name_len = read_u32(&buf, &mut pos) as usize;
        let name = String::from_utf8_lossy(&buf[pos..pos + name_len]).to_string();
        pos += name_len;
        stops.push(StopData { lat, lon, name });
    }
    binary_sections.push(("stops", pos - pos_before));
    timings.push(("parse stops", t0.elapsed()));

    // Stop-to-node mapping
    let t0 = Instant::now();
    let pos_before = pos;
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
    binary_sections.push(("stop_to_node", pos - pos_before));
    timings.push(("parse stop_to_node", t0.elapsed()));

    // Route names
    let t0 = Instant::now();
    let pos_before = pos;
    let mut route_names = Vec::with_capacity(num_route_names);
    for _ in 0..num_route_names {
        let name_len = read_u32(&buf, &mut pos) as usize;
        let name = String::from_utf8_lossy(&buf[pos..pos + name_len]).to_string();
        pos += name_len;
        route_names.push(name);
    }
    binary_sections.push(("route_names", pos - pos_before));
    timings.push(("parse route_names", t0.elapsed()));

    // Route colors
    let t0 = Instant::now();
    let pos_before = pos;
    let mut route_colors = Vec::with_capacity(num_route_names);
    for _ in 0..num_route_names {
        let has_color = buf[pos];
        pos += 1;
        if has_color != 0 {
            let r = buf[pos]; pos += 1;
            let g = buf[pos]; pos += 1;
            let b = buf[pos]; pos += 1;
            route_colors.push(Some(Color { r, g, b }));
        } else {
            route_colors.push(None);
        }
    }
    binary_sections.push(("route_colors", pos - pos_before));
    timings.push(("parse route_colors", t0.elapsed()));

    // Patterns
    let t0_patterns = Instant::now();
    let pos_before = pos;
    let mut total_events = 0usize;
    let total_sentinels = 0usize; // sentinels now included in total_events
    let mut total_freq = 0usize;
    let mut patterns = Vec::with_capacity(num_patterns);
    for _ in 0..num_patterns {
        let _pattern_id = read_u32(&buf, &mut pos);
        let day_mask = buf[pos]; pos += 1;
        let start_date = read_u32(&buf, &mut pos);
        let end_date = read_u32(&buf, &mut pos);
        let num_add = read_u32(&buf, &mut pos) as usize;
        let mut date_exceptions_add = Vec::with_capacity(num_add);
        for _ in 0..num_add { date_exceptions_add.push(read_u32(&buf, &mut pos)); }
        let num_remove = read_u32(&buf, &mut pos) as usize;
        let mut date_exceptions_remove = Vec::with_capacity(num_remove);
        for _ in 0..num_remove { date_exceptions_remove.push(read_u32(&buf, &mut pos)); }
        let min_time = read_u32(&buf, &mut pos);
        let max_time = read_u32(&buf, &mut pos);

        // v3: events pre-sorted with sentinels and next_event_index precomputed
        // 4 columns + sentinel_routes
        let num_events = read_u32(&buf, &mut pos) as usize;
        total_events += num_events;

        let time_offsets = read_pco_u32(&buf, &mut pos)?;
        let stop_indices = read_pco_u32(&buf, &mut pos)?;
        let travel_times = read_pco_u32(&buf, &mut pos)?;
        let next_event_indices = read_pco_u32(&buf, &mut pos)?;
        let stop_offsets = read_pco_u32(&buf, &mut pos)?;
        let sentinel_route_indices = read_pco_u32(&buf, &mut pos)?;

        let data_vec: Vec<EventData> = (0..num_events)
            .map(|i| EventData {
                time_offset: time_offsets[i],
                stop_index: stop_indices[i],
                travel_time: travel_times[i],
                next_event_index: next_event_indices[i],
            })
            .collect();

        let events_by_stop = JaggedArray {
            offsets: stop_offsets,
            data: data_vec,
        };

        let num_freq = read_u32(&buf, &mut pos) as usize;
        total_freq += num_freq;
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
                route_index, stop_index, start_time, end_time,
                headway_secs, next_stop_index, travel_time,
            });
            freq_indices.push(i as u32);
        }

        let freq_by_stop = JaggedArray::build(
            freq_indices, |&i| freq_entries[i as usize].stop_index, num_stops as u32,
        );

        // Build sentinel_routes for this pattern
        let mut pattern_sentinel_routes = std::collections::HashMap::new();
        for (i, route_idx) in sentinel_route_indices.iter().enumerate() {
            if *route_idx != 0 {
                pattern_sentinel_routes.insert(i as u32, *route_idx);
            }
        }

        patterns.push(PatternData {
            day_mask,
            start_date,
            end_date,
            date_exceptions_add,
            date_exceptions_remove,
            min_time,
            max_time,
            frequency_routes: freq_entries,
            stop_index: PatternStopIndex { freq_by_stop, events_by_stop },
            sentinel_routes: pattern_sentinel_routes,
        });
    }
    binary_sections.push(("patterns", pos - pos_before));
    timings.push(("parse+index patterns", t0_patterns.elapsed()));

    // Shapes: compressed PCO data
    let t0 = Instant::now();
    let pos_before = pos;
    let mut shapes_data: Vec<u8> = Vec::new();
    let mut shapes_offsets: Vec<u32> = vec![0];
    for shape_idx in 0..num_shapes {
        if pos + 4 > buf.len() {
            return Err(format!("Incomplete shape data at index {}", shape_idx));
        }
        let compressed_len = read_u32(&buf, &mut pos) as usize;
        if pos + compressed_len > buf.len() {
            return Err(format!(
                "Shape {} compressed data out of bounds: need {} bytes at pos {}, buf len {}",
                shape_idx, compressed_len, pos, buf.len()
            ));
        }
        shapes_data.extend_from_slice(&buf[pos..pos + compressed_len]);
        pos += compressed_len;
        shapes_offsets.push(shapes_data.len() as u32);
    }
    let shapes = JaggedArray {
        data: shapes_data,
        offsets: shapes_offsets,
    };
    binary_sections.push(("shapes", pos - pos_before));
    timings.push(("parse shapes", t0.elapsed()));

    // Route-to-shape mapping (indices instead of IDs)
    let t0 = Instant::now();
    let pos_before = pos;
    let mut route_shapes: Vec<Vec<u32>> = vec![Vec::new(); num_route_names];
    if pos + 4 <= buf.len() {
        let num_route_shapes = read_u32(&buf, &mut pos) as usize;
        for i in 0..num_route_shapes.min(num_route_names) {
            if pos + 4 > buf.len() {
                return Err(format!("Incomplete route_shapes data at route {}", i));
            }
            let num_shapes_for_route = read_u32(&buf, &mut pos) as usize;
            let mut shapes_for_route = Vec::with_capacity(num_shapes_for_route);
            for _ in 0..num_shapes_for_route {
                if pos + 4 > buf.len() {
                    return Err(format!("Incomplete shape index in route_shapes for route {}", i));
                }
                let shape_idx = read_u32(&buf, &mut pos);
                // Only keep valid shape indices (those that actually exist in the shapes array)
                if shape_idx != u32::MAX && (shape_idx as usize) < shapes.offsets.len() - 1 {
                    shapes_for_route.push(shape_idx);
                }
            }
            route_shapes[i] = shapes_for_route;
        }
    }
    binary_sections.push(("route_shapes", pos - pos_before));
    timings.push(("parse route_shapes", t0.elapsed()));

    // Build adjacency list
    let t0 = Instant::now();
    let mut adj: Vec<Vec<(u32, f32)>> = vec![Vec::new(); num_nodes];
    for edge in &edges {
        adj[edge.u as usize].push((edge.v, edge.distance_meters));
        adj[edge.v as usize].push((edge.u, edge.distance_meters));
    }
    timings.push(("build adj list", t0.elapsed()));

    // Build spatial grid
    let t0 = Instant::now();
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
    timings.push(("build node_grid", t0.elapsed()));

    // Compute memory sizes
    let mut memory_sections: Vec<(&str, usize)> = Vec::new();

    // nodes: Vec<NodeData> where NodeData = {f64, f64} = 16 bytes each
    memory_sections.push(("nodes", nodes.capacity() * std::mem::size_of::<NodeData>()));

    // edges: Vec<EdgeData> where EdgeData = {u32, u32, f32} = 12 bytes each
    memory_sections.push(("edges", edges.capacity() * std::mem::size_of::<EdgeData>()));

    // stops: approximate (16 bytes struct + string heap)
    let stops_mem: usize = stops.iter().map(|s| {
        std::mem::size_of::<StopData>() + s.name.capacity()
    }).sum();
    memory_sections.push(("stops", stops_mem));

    // stop_node_map
    memory_sections.push(("stop_node_map", stop_node_map.capacity() * 4));

    // node_is_stop
    memory_sections.push(("node_is_stop", node_is_stop.capacity()));

    // node_stop_indices: Vec<Vec<u32>> — outer vec + inner vecs
    let nsi_mem: usize = num_nodes * std::mem::size_of::<Vec<u32>>()
        + node_stop_indices.iter().map(|v| v.capacity() * 4).sum::<usize>();
    memory_sections.push(("node_stop_indices", nsi_mem));

    // route_names
    let rn_mem: usize = route_names.iter().map(|s| std::mem::size_of::<String>() + s.capacity()).sum();
    memory_sections.push(("route_names", rn_mem));

    // route_colors
    memory_sections.push(("route_colors", route_colors.capacity() * std::mem::size_of::<Option<Color>>()));

    // patterns: events_by_stop data + offsets + freq data + freq offsets + freq_entries
    let mut pat_events_mem = 0usize;
    let mut pat_freq_mem = 0usize;
    let mut pat_other_mem = 0usize;
    for p in &patterns {
        pat_events_mem += p.stop_index.events_by_stop.data.capacity() * std::mem::size_of::<EventData>()
            + p.stop_index.events_by_stop.offsets.capacity() * 4;
        pat_freq_mem += p.stop_index.freq_by_stop.data.capacity() * 4
            + p.stop_index.freq_by_stop.offsets.capacity() * 4
            + p.frequency_routes.capacity() * std::mem::size_of::<FreqData>();
        pat_other_mem += p.date_exceptions_add.capacity() * 4
            + p.date_exceptions_remove.capacity() * 4;
    }
    memory_sections.push(("patterns/events", pat_events_mem));
    memory_sections.push(("patterns/freq", pat_freq_mem));
    memory_sections.push(("patterns/other", pat_other_mem));

    // adj list: Vec<Vec<(u32, f32)>>
    let adj_mem: usize = num_nodes * std::mem::size_of::<Vec<(u32, f32)>>()
        + adj.iter().map(|v| v.capacity() * std::mem::size_of::<(u32, f32)>()).sum::<usize>();
    memory_sections.push(("adj list", adj_mem));

    // shapes JaggedArray: compressed PCO data
    let shapes_mem: usize = shapes.data.capacity() + shapes.offsets.capacity() * 4;
    memory_sections.push(("shapes", shapes_mem));

    // route_shapes: now Vec<Vec<u32>> (shape indices) - stored compressed in binary anyway
    // Skip detailed memory calculation as it's in compressed form
    memory_sections.push(("route_shapes", 0));

    // node_grid HashMap
    let ng_mem: usize = node_grid.iter().map(|(_, v)| {
        16 + 64 + v.capacity() * 4 // key + hashmap overhead + data
    }).sum();
    memory_sections.push(("node_grid", ng_mem));

    // decompressed buf (transient)
    memory_sections.push(("decompressed buf (transient)", buf.capacity()));

    let counts = vec![
        ("nodes", num_nodes),
        ("edges", num_edges),
        ("stops", num_stops),
        ("stop_to_node", num_stop_to_node),
        ("patterns", num_patterns),
        ("route_names", num_route_names),
        ("shapes", num_shapes),
        ("total events (raw)", total_events),
        ("sentinel events", total_sentinels),
        ("total freq entries", total_freq),
        ("grid cells", node_grid.len()),
    ];

    let stats = LoadStats {
        compressed_size: compressed.len(),
        decompressed_size: buf.len(),
        binary_sections,
        memory_sections,
        timings,
        counts,
    };

    let data = PreparedData {
        nodes, edges, stops, stop_node_map, route_names, route_colors,
        patterns, num_nodes, num_edges, num_stops, adj, node_is_stop,
        node_stop_indices, shapes, route_shapes, node_grid,
    };

    Ok((data, stats))
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
