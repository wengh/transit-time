use std::time::Duration;
extern crate console_error_panic_hook;

use chrono::NaiveDate;

/// Decode a u32 GTFS-style date (YYYYMMDD) into a [`NaiveDate`], returning
/// `None` if the value isn't a valid calendar date. Used at the JS / CLI
/// boundary where dates are passed as YYYYMMDD u32.
pub fn yyyymmdd_to_naive_date_opt(v: u32) -> Option<NaiveDate> {
    let y = (v / 10_000) as i32;
    let m = (v / 100) % 100;
    let d = v % 100;
    NaiveDate::from_ymd_opt(y, m, d)
}

fn days_to_naive_date(v: i32) -> Result<NaiveDate, String> {
    NaiveDate::from_num_days_from_ce_opt(v)
        .ok_or_else(|| format!("invalid days-since-CE value in prepared binary: {v}"))
}

/// `i32::MIN` encodes "unbounded" for pattern service-window bounds.
fn days_bound_to_naive_date(v: i32) -> Result<Option<NaiveDate>, String> {
    if v == i32::MIN {
        Ok(None)
    } else {
        days_to_naive_date(v).map(Some)
    }
}

/// Bounds-checked little-endian cursor over the prepared binary. Every read
/// returns `Err` on truncated input instead of panicking on a slice index, so
/// `load` can honour its `Result` contract for corrupt files.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn pos(&self) -> usize {
        self.pos
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.buf.len())
            .ok_or_else(|| {
                format!(
                    "truncated input: need {n} bytes at offset {}, but only {} remain",
                    self.pos,
                    self.buf.len() - self.pos
                )
            })?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }

    /// `len: u32` followed by `len` bytes of (lossily decoded) UTF-8.
    fn string(&mut self) -> Result<String, String> {
        let len = self.u32()? as usize;
        Ok(String::from_utf8_lossy(self.bytes(len)?).into_owned())
    }

    /// `pco_len: u32` followed by a PCO frame; a zero length is an empty column.
    fn pco<T: pco::data_types::Number>(&mut self) -> Result<Vec<T>, String> {
        let pco_len = self.u32()? as usize;
        if pco_len == 0 {
            return Ok(Vec::new());
        }
        pco::standalone::simple_decompress(self.bytes(pco_len)?)
            .map_err(|e| format!("pco decompress failed: {}", e))
    }
}

fn check_len(what: &str, got: usize, want: usize) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("{what}: expected {want} entries, got {got}"))
    }
}

/// `idx` must be a valid index into a table of `len` entries, or (when
/// `allow_sentinel`) the `u32::MAX` "none" marker.
fn check_index(what: &str, idx: u32, len: usize, allow_sentinel: bool) -> Result<(), String> {
    if (idx as usize) < len || (allow_sentinel && idx == u32::MAX) {
        Ok(())
    } else {
        Err(format!("{what}: index {idx} out of range (len {len})"))
    }
}

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
    /// Absolute seconds since the start of the GTFS service day.
    pub time_offset: u32,
    pub stop_index: u32,
    pub travel_time: u32,
    pub next_event_index: u32, // u32::MAX if it's the last event in the trip
}

impl EventData {
    /// True for the synthetic arrival-only event appended to each trip in
    /// `transit-prep`. Sentinels carry the final stop's arrival time and have
    /// no successor (`next_event_index == u32::MAX`, `travel_time == 0`); they
    /// must not be used as boarding candidates or traversed as legs. Every non-sentinel event has `travel_time > 0`.
    #[inline]
    pub fn is_trip_end(&self) -> bool {
        self.next_event_index == u32::MAX
    }
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

impl FreqData {
    /// True for the last leg of a frequency-based trip — i.e. no successor
    /// leg to chain into.
    #[inline]
    pub fn is_last_leg(&self) -> bool {
        self.next_freq_index == u32::MAX
    }
}

#[derive(Debug, Clone)]
pub struct JaggedArray<T> {
    pub offsets: Vec<u32>,
    pub data: Vec<T>,
}

impl<T> std::ops::Index<u32> for JaggedArray<T> {
    type Output = [T];

    #[inline(always)]
    fn index(&self, index: u32) -> &Self::Output {
        let start = self.offsets[index as usize] as usize;
        let end = self.offsets[index as usize + 1] as usize;
        &self.data[start..end]
    }
}

impl<T> JaggedArray<T> {
    /// Number of buckets (rows).
    pub fn len(&self) -> u32 {
        (self.offsets.len() - 1) as u32
    }

    /// `true` when there are no buckets. Note this is about rows, not items:
    /// use `data.is_empty()` to ask whether the array holds any items.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: Copy + Default> JaggedArray<T> {
    /// Bucket `items` by `key_fn` into `len` rows (counting sort; items keep
    /// their input order within a row).
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
        // Scatter into a default-filled buffer; every slot is overwritten
        // exactly once since the cursors start at the bucket offsets.
        let mut cursors = offsets[..n].to_vec();
        let mut data = vec![T::default(); acc as usize];
        for item in items {
            let bucket = key_fn(&item) as usize;
            data[cursors[bucket] as usize] = item;
            cursors[bucket] += 1;
        }

        Self { offsets, data }
    }
}

pub struct PatternStopIndex {
    pub freq_by_stop: JaggedArray<u32>,
    pub events_by_stop: JaggedArray<EventData>,
}

pub struct PatternData {
    pub day_mask: u8,
    /// Inclusive lower bound of the service window. `None` = unbounded.
    pub start_date: Option<NaiveDate>,
    /// Inclusive upper bound of the service window. `None` = unbounded.
    pub end_date: Option<NaiveDate>,
    pub date_exceptions_add: Vec<NaiveDate>,
    pub date_exceptions_remove: Vec<NaiveDate>,
    pub min_time: u32,
    pub max_time: u32,
    pub frequency_routes: Vec<FreqData>,
    pub stop_index: PatternStopIndex,
    /// Maps flat event index to route_index for trip-end sentinel events
    /// (see `EventData::is_trip_end`).
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

    let mut r = Reader::new(buf);

    // Header
    if r.bytes(4)? != b"TRNS" {
        return Err("Invalid magic".to_string());
    }
    let version = r.u32()?;
    if version != 12 {
        return Err(format!("Unsupported version {}", version));
    }
    let num_nodes = r.u32()? as usize;
    let num_edges = r.u32()? as usize;
    let num_stops = r.u32()? as usize;
    let num_patterns = r.u32()? as usize;
    let num_route_names = r.u32()? as usize;
    let num_shapes = r.u32()? as usize;
    if num_stops > num_nodes {
        return Err(format!(
            "Header says {num_stops} stops but only {num_nodes} nodes; stops must occupy [0, num_stops)"
        ));
    }
    let header_end = r.pos();
    binary_sections.push(("header", header_end));

    // Nodes (v5): 32-bit fixed-point 0.1 m resolution, SFC-sorted.
    // Header: min_lat, min_lon (f64), lat_scale, lon_scale (f64 = units per degree).
    let t0 = Instant::now();
    let pos_before = r.pos();
    let min_lat = r.f64()?;
    let min_lon = r.f64()?;
    let lat_scale = r.f64()?;
    let lon_scale = r.f64()?;
    let lat_u32: Vec<u32> = r.pco()?;
    let lon_u32: Vec<u32> = r.pco()?;
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
    binary_sections.push(("nodes", r.pos() - pos_before));
    timings.push(("parse nodes", t0.elapsed()));

    // Edges: u, delta=u-v, walk_time (u32 seconds, at 1.4 m/s, min 1).
    // Canonical u > v, sorted by (u, delta).
    let t0 = Instant::now();
    let pos_before = r.pos();
    let edge_u: Vec<u32> = r.pco()?;
    let edge_delta: Vec<u32> = r.pco()?;
    let edge_walk_time: Vec<u32> = r.pco()?;
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
    let mut edges: Vec<EdgeData> = Vec::with_capacity(num_edges);
    for i in 0..num_edges {
        let u = edge_u[i];
        check_index("edge u", u, num_nodes, false)?;
        let v = u
            .checked_sub(edge_delta[i])
            .ok_or_else(|| format!("edge {i}: delta {} exceeds u {u}", edge_delta[i]))?;
        edges.push(EdgeData {
            u,
            v,
            walk_time: edge_walk_time[i] as u16,
        });
    }
    binary_sections.push(("edges", r.pos() - pos_before));
    timings.push(("parse edges", t0.elapsed()));

    // Stops
    let t0 = Instant::now();
    let pos_before = r.pos();
    let mut stops = Vec::with_capacity(num_stops);
    for _ in 0..num_stops {
        let lat = r.f64()?;
        let lon = r.f64()?;
        let name = r.string()?;
        stops.push(StopData { lat, lon, name });
    }
    binary_sections.push(("stops", r.pos() - pos_before));
    timings.push(("parse stops", t0.elapsed()));

    // (v11) Stop↔node mapping is implicit: stops live at node indices
    // [0, num_stops), with stop_idx == node_idx.

    // Route names
    let t0 = Instant::now();
    let pos_before = r.pos();
    let mut route_names = Vec::with_capacity(num_route_names);
    for _ in 0..num_route_names {
        route_names.push(r.string()?);
    }
    binary_sections.push(("route_names", r.pos() - pos_before));
    timings.push(("parse route_names", t0.elapsed()));

    // Route colors
    let t0 = Instant::now();
    let pos_before = r.pos();
    let mut route_colors = Vec::with_capacity(num_route_names);
    for _ in 0..num_route_names {
        let has_color = r.u8()?;
        if has_color != 0 {
            let rgb = r.bytes(3)?;
            route_colors.push(Some(Color {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
            }));
        } else {
            route_colors.push(None);
        }
    }
    binary_sections.push(("route_colors", r.pos() - pos_before));
    timings.push(("parse route_colors", t0.elapsed()));

    // Patterns
    let t0_patterns = Instant::now();
    let pos_before = r.pos();
    let mut total_events = 0usize;
    let total_sentinels = 0usize; // sentinels now included in total_events
    let mut total_freq = 0usize;
    let mut patterns = Vec::with_capacity(num_patterns);
    for pat_idx in 0..num_patterns {
        let _pattern_id = r.u32()?;
        let day_mask = r.u8()?;
        let start_date = days_bound_to_naive_date(r.u32()? as i32)?;
        let end_date = days_bound_to_naive_date(r.u32()? as i32)?;
        let num_add = r.u32()? as usize;
        let mut date_exceptions_add = Vec::with_capacity(num_add.min(1 << 16));
        for _ in 0..num_add {
            date_exceptions_add.push(days_to_naive_date(r.u32()? as i32)?);
        }
        let num_remove = r.u32()? as usize;
        let mut date_exceptions_remove = Vec::with_capacity(num_remove.min(1 << 16));
        for _ in 0..num_remove {
            date_exceptions_remove.push(days_to_naive_date(r.u32()? as i32)?);
        }
        let min_time = r.u32()?;
        let max_time = r.u32()?;

        // v3: events pre-sorted with sentinels and next_event_index precomputed
        // 4 columns + sentinel_routes
        let num_events = r.u32()? as usize;
        total_events += num_events;

        let time_offsets: Vec<u32> = r.pco()?;
        let stop_indices: Vec<u32> = r.pco()?;
        let travel_times: Vec<u32> = r.pco()?;
        let next_event_indices: Vec<u32> = r.pco()?;
        let stop_offsets: Vec<u32> = r.pco()?;
        let sentinel_route_indices: Vec<u32> = r.pco()?;

        check_len("pattern event time_offsets", time_offsets.len(), num_events)?;
        check_len("pattern event stop_indices", stop_indices.len(), num_events)?;
        check_len("pattern event travel_times", travel_times.len(), num_events)?;
        check_len(
            "pattern event next_event_indices",
            next_event_indices.len(),
            num_events,
        )?;
        check_len("pattern stop_offsets", stop_offsets.len(), num_stops + 1)?;
        check_len(
            "pattern sentinel_routes",
            sentinel_route_indices.len(),
            num_events,
        )?;
        if stop_offsets.windows(2).any(|w| w[0] > w[1])
            || stop_offsets
                .last()
                .is_some_and(|&last| last as usize != num_events)
        {
            return Err(format!(
                "pattern {pat_idx}: stop_offsets are not a monotone prefix sum ending at {num_events}"
            ));
        }

        let mut data_vec: Vec<EventData> = Vec::with_capacity(num_events);
        for i in 0..num_events {
            check_index("event stop_index", stop_indices[i], num_stops, false)?;
            check_index(
                "event next_event_index",
                next_event_indices[i],
                num_events,
                true,
            )?;
            data_vec.push(EventData {
                time_offset: min_time + time_offsets[i],
                stop_index: stop_indices[i],
                travel_time: travel_times[i],
                next_event_index: next_event_indices[i],
            });
        }

        let events_by_stop = JaggedArray {
            offsets: stop_offsets,
            data: data_vec,
        };

        let num_freq = r.u32()? as usize;
        total_freq += num_freq;
        let mut freq_entries = Vec::with_capacity(num_freq.min(1 << 16));
        for _ in 0..num_freq {
            let route_index = r.u32()?;
            let stop_index = r.u32()?;
            let start_time = r.u32()?;
            let end_time = r.u32()?;
            let headway_secs = r.u32()?;
            let next_stop_index = r.u32()?;
            let travel_time = r.u32()?;
            let next_freq_index = r.u32()?;
            check_index("freq route_index", route_index, num_route_names, false)?;
            check_index("freq stop_index", stop_index, num_stops, false)?;
            check_index("freq next_stop_index", next_stop_index, num_stops, false)?;
            check_index("freq next_freq_index", next_freq_index, num_freq, true)?;
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
        }
        let freq_indices: Vec<u32> = (0..num_freq as u32).collect();

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
                check_index("sentinel route_index", *route_idx, num_route_names, false)?;
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
    binary_sections.push(("patterns", r.pos() - pos_before));
    timings.push(("parse+index patterns", t0_patterns.elapsed()));

    // Leg shapes (v9): six global PCO columns. Decompress once at load time
    // into flat Vecs so per-hover lookups are a zero-allocation slice.
    let t0 = Instant::now();
    let pos_before = r.pos();
    let routes: Vec<u32> = r.pco()?;
    let from_stops: Vec<u32> = r.pco()?;
    let to_stops: Vec<u32> = r.pco()?;
    let point_counts: Vec<u32> = r.pco()?;
    let leg_shapes_lat: Vec<i32> = r.pco()?;
    let leg_shapes_lon: Vec<i32> = r.pco()?;
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
    binary_sections.push(("leg_shapes", r.pos() - pos_before));
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
