use std::time::Duration;
extern crate console_error_panic_hook;

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

/// Zero-cost no-op Instant for wasm32 where std::time::Instant panics.
#[cfg(target_arch = "wasm32")]
struct Instant;
#[cfg(target_arch = "wasm32")]
impl Instant {
    fn now() -> Self {
        Instant
    }
    fn elapsed(&self) -> Duration {
        Duration::ZERO
    }
}

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
    pub walk_time: u16,
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
    /// Index of the next FreqData in the same trip, or u32::MAX if last leg.
    pub next_freq_index: u32,
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
        let n = len as usize;
        // Count items per bucket
        let mut counts = vec![0u32; n + 1];
        for item in &items {
            let bucket = key_fn(item) as usize;
            assert!(
                bucket < n,
                "key_fn returned out-of-bounds bucket: {} >= {}",
                bucket,
                n
            );
            counts[bucket] += 1;
        }
        // Convert counts to start offsets in-place, then append total as the sentinel.
        let mut acc = 0u32;
        for c in &mut counts {
            let prev = *c;
            *c = acc;
            acc += prev;
        }
        let offsets = counts;
        // Scatter items into a MaybeUninit buffer, then transmute once all slots are filled.
        let mut cursors = offsets[..n].to_vec();
        let mut data: Vec<std::mem::MaybeUninit<T>> = (0..acc as usize)
            .map(|_| std::mem::MaybeUninit::uninit())
            .collect();
        for item in items {
            let bucket = key_fn(&item) as usize;
            data[cursors[bucket] as usize].write(item);
            cursors[bucket] += 1;
        }
        // Safety: every slot 0..acc has been written exactly once above.
        let data = unsafe {
            let mut md = std::mem::ManuallyDrop::new(data);
            Vec::from_raw_parts(md.as_mut_ptr() as *mut T, md.len(), md.capacity())
        };

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
    /// Maps flat event index to route_index for sentinel events (travel_time == 0 and next_event_index == u32::MAX).
    pub sentinel_routes: std::collections::HashMap<u32, u32>,
}

pub struct PreparedData {
    pub nodes: Vec<NodeData>,
    pub stops: Vec<StopData>,
    pub route_names: Vec<String>,
    pub route_colors: Vec<Option<Color>>,
    pub patterns: Vec<PatternData>,
    pub num_nodes: usize,
    pub num_edges: usize,
    /// Binary-format invariant (v11): the first `num_stops` nodes are the
    /// transit-stop-bearing nodes, and `stop_idx == node_idx` for every stop.
    /// So `stop_to_node(s) = s` and `node_to_stop(n) = (n < num_stops).then_some(n)`.
    pub num_stops: usize,
    pub adj: JaggedArray<(u32, u16)>,
    /// Per-leg point-count prefix sum (length = num_legs + 1). Slice
    /// `leg_shapes_lat[offsets[i]..offsets[i+1]]` to get leg `i`'s lats.
    pub leg_shape_offsets: Vec<u32>,
    /// Concatenated i32 lat offsets for every leg, at 0.1 m against `coord_min_lat`.
    pub leg_shapes_lat: Vec<i32>,
    /// Concatenated i32 lon offsets, paired with `leg_shapes_lat`.
    pub leg_shapes_lon: Vec<i32>,
    /// Sorted keys for leg_shapes: (route_index, from_stop, to_stop)
    pub leg_shape_keys: Vec<(u32, u32, u32)>,
    /// Origin/scale for reconstructing shape (and node) coordinates from fixed-point offsets.
    pub coord_min_lat: f64,
    pub coord_min_lon: f64,
    pub coord_lat_scale: f64,
    pub coord_lon_scale: f64,
    /// Spatial grid index: (lat_cell, lon_cell) -> [node_indices]
    pub node_grid: std::collections::HashMap<(i32, i32), Vec<u32>>,
}

impl PreparedData {
    /// Stop index (== node index) if `node_idx` carries a transit stop, else `None`.
    #[inline]
    pub fn node_to_stop(&self, node_idx: u32) -> Option<u32> {
        ((node_idx as usize) < self.num_stops).then_some(node_idx)
    }

    /// Node index for a given stop index. Identity under the v11 layout.
    #[inline]
    pub fn stop_to_node(&self, stop_idx: u32) -> u32 {
        debug_assert!((stop_idx as usize) < self.num_stops);
        stop_idx
    }
}

pub fn load(buf: &[u8]) -> Result<PreparedData, String> {
    load_with_stats(buf).map(|(data, _)| data)
}

pub fn load_with_stats(buf: &[u8]) -> Result<(PreparedData, LoadStats), String> {
    console_error_panic_hook::set_once();
    let mut binary_sections: Vec<(&str, usize)> = Vec::new();
    let mut timings: Vec<(&str, Duration)> = Vec::new();

    let mut pos = 0;

    // Header
    if &buf[pos..pos + 4] != b"TRNS" {
        return Err("Invalid magic".to_string());
    }
    pos += 4;
    let version = read_u32(&buf, &mut pos);
    if version != 11 {
        return Err(format!("Unsupported version {}", version));
    }
    let num_nodes = read_u32(&buf, &mut pos) as usize;
    let num_edges = read_u32(&buf, &mut pos) as usize;
    let num_stops = read_u32(&buf, &mut pos) as usize;
    let num_patterns = read_u32(&buf, &mut pos) as usize;
    let num_route_names = read_u32(&buf, &mut pos) as usize;
    let num_shapes = read_u32(&buf, &mut pos) as usize;
    let header_end = pos;
    binary_sections.push(("header", header_end));

    // Nodes (v5): 32-bit fixed-point 0.1 m resolution, SFC-sorted.
    // Header: min_lat, min_lon (f64), lat_scale, lon_scale (f64 = units per degree).
    let t0 = Instant::now();
    let pos_before = pos;
    let min_lat = read_f64(&buf, &mut pos);
    let min_lon = read_f64(&buf, &mut pos);
    let lat_scale = read_f64(&buf, &mut pos);
    let lon_scale = read_f64(&buf, &mut pos);
    let lat_u32 = read_pco_u32(&buf, &mut pos)?;
    let lon_u32 = read_pco_u32(&buf, &mut pos)?;
    if lat_u32.len() != num_nodes || lon_u32.len() != num_nodes {
        return Err(format!(
            "Node count mismatch: header says {}, got lat={} lon={}",
            num_nodes,
            lat_u32.len(),
            lon_u32.len()
        ));
    }
    let nodes: Vec<NodeData> = lat_u32
        .into_iter()
        .zip(lon_u32)
        .map(|(ly, lx)| NodeData {
            lat: min_lat + ly as f64 / lat_scale,
            lon: min_lon + lx as f64 / lon_scale,
        })
        .collect();
    binary_sections.push(("nodes", pos - pos_before));
    timings.push(("parse nodes", t0.elapsed()));

    // Edges: u, delta=u-v, walk_time (u32 seconds, at 1.4 m/s, min 1).
    // Canonical u > v, sorted by (u, delta).
    let t0 = Instant::now();
    let pos_before = pos;
    let edge_u = read_pco_u32(&buf, &mut pos)?;
    let edge_delta = read_pco_u32(&buf, &mut pos)?;
    let edge_walk_time = read_pco_u32(&buf, &mut pos)?;
    if edge_u.len() != num_edges
        || edge_delta.len() != num_edges
        || edge_walk_time.len() != num_edges
    {
        return Err(format!(
            "Edge count mismatch: header says {}, got u={} delta={} walk_time={}",
            num_edges,
            edge_u.len(),
            edge_delta.len(),
            edge_walk_time.len()
        ));
    }
    let edges: Vec<EdgeData> = (0..num_edges)
        .map(|i| {
            let u = edge_u[i];
            let v = u - edge_delta[i];
            EdgeData {
                u,
                v,
                walk_time: edge_walk_time[i] as u16,
            }
        })
        .collect();
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

    // (v11) Stop↔node mapping is implicit: stops live at node indices
    // [0, num_stops), with stop_idx == node_idx.

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
            let next_freq_index = read_u32(&buf, &mut pos);
            freq_entries.push(FreqData {
                route_index,
                stop_index,
                start_time,
                end_time,
                headway_secs,
                next_stop_index,
                travel_time,
                next_freq_index,
            });
            freq_indices.push(i as u32);
        }

        let freq_by_stop = JaggedArray::build(
            freq_indices,
            |&i| freq_entries[i as usize].stop_index,
            num_stops as u32,
        );

        // Build sentinel_routes for this pattern.
        //
        // The prep format stores 0 for non-sentinel slots, which collides with
        // the real route_index=0 (first route in the feed). Toronto trips that
        // end on route 0 used to fall out of this map and panic later in
        // `profile.rs` via direct HashMap indexing. Use the intrinsic sentinel
        // predicate (`next_event_index == u32::MAX`) to decide membership instead
        // of treating 0 as the absence marker.
        let mut pattern_sentinel_routes = std::collections::HashMap::new();
        for (i, route_idx) in sentinel_route_indices.iter().enumerate() {
            if next_event_indices[i] == u32::MAX {
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
            stop_index: PatternStopIndex {
                freq_by_stop,
                events_by_stop,
            },
            sentinel_routes: pattern_sentinel_routes,
        });
    }
    binary_sections.push(("patterns", pos - pos_before));
    timings.push(("parse+index patterns", t0_patterns.elapsed()));

    // Leg shapes (v9): six global PCO columns. Decompress once at load time
    // into flat Vecs so per-hover lookups are a zero-allocation slice.
    let t0 = Instant::now();
    let pos_before = pos;
    let routes = read_pco_u32(&buf, &mut pos)?;
    let from_stops = read_pco_u32(&buf, &mut pos)?;
    let to_stops = read_pco_u32(&buf, &mut pos)?;
    let point_counts = read_pco_u32(&buf, &mut pos)?;
    let leg_shapes_lat: Vec<i32> = read_pco_i32(&buf, &mut pos)?;
    let leg_shapes_lon: Vec<i32> = read_pco_i32(&buf, &mut pos)?;
    if routes.len() != num_shapes
        || from_stops.len() != num_shapes
        || to_stops.len() != num_shapes
        || point_counts.len() != num_shapes
    {
        return Err(format!(
            "Leg shape column length mismatch: header says {}, got routes={} from={} to={} counts={}",
            num_shapes,
            routes.len(),
            from_stops.len(),
            to_stops.len(),
            point_counts.len()
        ));
    }
    let mut leg_shape_offsets: Vec<u32> = Vec::with_capacity(num_shapes + 1);
    leg_shape_offsets.push(0);
    let mut acc: u32 = 0;
    for &c in &point_counts {
        acc = acc.checked_add(c).ok_or("leg shape offset overflow")?;
        leg_shape_offsets.push(acc);
    }
    if leg_shapes_lat.len() != acc as usize || leg_shapes_lon.len() != acc as usize {
        return Err(format!(
            "Leg shape point total mismatch: counts sum {}, lats {}, lons {}",
            acc,
            leg_shapes_lat.len(),
            leg_shapes_lon.len()
        ));
    }
    let mut leg_shape_keys: Vec<(u32, u32, u32)> = Vec::with_capacity(num_shapes);
    for i in 0..num_shapes {
        leg_shape_keys.push((routes[i], from_stops[i], to_stops[i]));
    }
    binary_sections.push(("leg_shapes", pos - pos_before));
    timings.push(("parse leg_shapes", t0.elapsed()));

    // Build adjacency list as JaggedArray<(u32, u16)>
    let t0 = Instant::now();
    let adj = {
        // Count degree of each node
        let mut counts = vec![0u32; num_nodes];
        for edge in &edges {
            counts[edge.u as usize] += 1;
            counts[edge.v as usize] += 1;
        }
        // Build prefix-sum offsets
        let mut offsets = Vec::with_capacity(num_nodes + 1);
        offsets.push(0u32);
        for &c in &counts {
            offsets.push(offsets.last().unwrap() + c);
        }
        // Fill data
        let total = *offsets.last().unwrap() as usize;
        let mut data: Vec<(u32, u16)> = vec![(0, 0); total];
        let mut pos_fill = offsets[..num_nodes].to_vec();
        for edge in &edges {
            let u = edge.u as usize;
            let v = edge.v as usize;
            data[pos_fill[u] as usize] = (edge.v, edge.walk_time);
            pos_fill[u] += 1;
            data[pos_fill[v] as usize] = (edge.u, edge.walk_time);
            pos_fill[v] += 1;
        }
        JaggedArray { offsets, data }
    };
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

    // edges: Vec<EdgeData> where EdgeData = {u32, u32, u16} = 12 bytes each (padded)
    memory_sections.push(("edges", edges.capacity() * std::mem::size_of::<EdgeData>()));

    // stops: approximate (16 bytes struct + string heap)
    let stops_mem: usize = stops
        .iter()
        .map(|s| std::mem::size_of::<StopData>() + s.name.capacity())
        .sum();
    memory_sections.push(("stops", stops_mem));

    // route_names
    let rn_mem: usize = route_names
        .iter()
        .map(|s| std::mem::size_of::<String>() + s.capacity())
        .sum();
    memory_sections.push(("route_names", rn_mem));

    // route_colors
    memory_sections.push((
        "route_colors",
        route_colors.capacity() * std::mem::size_of::<Option<Color>>(),
    ));

    // patterns: events_by_stop data + offsets + freq data + freq offsets + freq_entries
    let mut pat_events_mem = 0usize;
    let mut pat_freq_mem = 0usize;
    let mut pat_other_mem = 0usize;
    for p in &patterns {
        pat_events_mem += p.stop_index.events_by_stop.data.capacity()
            * std::mem::size_of::<EventData>()
            + p.stop_index.events_by_stop.offsets.capacity() * 4;
        pat_freq_mem += p.stop_index.freq_by_stop.data.capacity() * 4
            + p.stop_index.freq_by_stop.offsets.capacity() * 4
            + p.frequency_routes.capacity() * std::mem::size_of::<FreqData>();
        pat_other_mem +=
            p.date_exceptions_add.capacity() * 4 + p.date_exceptions_remove.capacity() * 4;
    }
    memory_sections.push(("patterns/events", pat_events_mem));
    memory_sections.push(("patterns/freq", pat_freq_mem));
    memory_sections.push(("patterns/other", pat_other_mem));

    // adj list: JaggedArray<(u32, u16)> — offsets + flat data
    let adj_mem: usize =
        adj.offsets.capacity() * 4 + adj.data.capacity() * std::mem::size_of::<(u32, u16)>();
    memory_sections.push(("adj list", adj_mem));

    // leg_shapes: flat i32 lat/lon vectors + offsets prefix-sum + sorted keys
    let leg_shapes_mem: usize = leg_shapes_lat.capacity() * 4
        + leg_shapes_lon.capacity() * 4
        + leg_shape_offsets.capacity() * 4
        + leg_shape_keys.capacity() * std::mem::size_of::<(u32, u32, u32)>();
    memory_sections.push(("leg_shapes", leg_shapes_mem));

    // node_grid HashMap
    let ng_mem: usize = node_grid
        .iter()
        .map(|(_, v)| {
            16 + 64 + v.capacity() * 4 // key + hashmap overhead + data
        })
        .sum();
    memory_sections.push(("node_grid", ng_mem));

    // decompressed buf (transient)
    memory_sections.push(("input buf", buf.len()));

    let counts = vec![
        ("nodes", num_nodes),
        ("edges", num_edges),
        ("stops", num_stops),
        ("patterns", num_patterns),
        ("route_names", num_route_names),
        ("leg_shapes", num_shapes),
        ("total events (raw)", total_events),
        ("sentinel events", total_sentinels),
        ("total freq entries", total_freq),
        ("grid cells", node_grid.len()),
    ];

    let stats = LoadStats {
        decompressed_size: buf.len(),
        binary_sections,
        memory_sections,
        timings,
        counts,
    };

    let data = PreparedData {
        nodes,
        stops,
        route_names,
        route_colors,
        patterns,
        num_nodes,
        num_edges,
        num_stops,
        adj,
        leg_shape_offsets,
        leg_shapes_lat,
        leg_shapes_lon,
        leg_shape_keys,
        coord_min_lat: min_lat,
        coord_min_lon: min_lon,
        coord_lat_scale: lat_scale,
        coord_lon_scale: lon_scale,
        node_grid,
    };

    Ok((data, stats))
}

pub struct LoadStats {
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
        println!(
            "{:<25} {:>12}",
            "TOTAL decompressed",
            fmt_bytes(self.decompressed_size)
        );
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
    if b >= 1_048_576 {
        format!("{:.2} MB", b as f64 / 1_048_576.0)
    } else if b >= 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{} B", b)
    }
}

fn fmt_dur(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        format!("{:.1} ms", ms)
    }
}

fn read_pco_u32(buf: &[u8], pos: &mut usize) -> Result<Vec<u32>, String> {
    let pco_len = read_u32(buf, pos) as usize;
    if pco_len == 0 {
        return Ok(Vec::new());
    }
    let result: Vec<u32> = pco::standalone::simple_decompress(&buf[*pos..*pos + pco_len])
        .map_err(|e| format!("pco decompress failed: {}", e))?;
    *pos += pco_len;
    Ok(result)
}

fn read_pco_i32(buf: &[u8], pos: &mut usize) -> Result<Vec<i32>, String> {
    let pco_len = read_u32(buf, pos) as usize;
    if pco_len == 0 {
        return Ok(Vec::new());
    }
    let result: Vec<i32> = pco::standalone::simple_decompress(&buf[*pos..*pos + pco_len])
        .map_err(|e| format!("pco decompress failed: {}", e))?;
    *pos += pco_len;
    Ok(result)
}

fn read_u32(buf: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_le_bytes(buf[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    v
}

fn read_f64(buf: &[u8], pos: &mut usize) -> f64 {
    let v = f64::from_le_bytes(buf[*pos..*pos + 8].try_into().unwrap());
    *pos += 8;
    v
}
