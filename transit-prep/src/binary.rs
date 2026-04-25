use crate::graph::{OsmEdge, OsmNode};
use crate::gtfs::{Color, ServicePattern, Stop};
use anyhow::Result;
use std::io::Write;
use std::path::Path;

/// All prepared data ready for serialization.
pub struct PreparedData {
    pub nodes: Vec<OsmNode>,
    pub edges: Vec<OsmEdge>,
    pub stops: Vec<Stop>,
    pub stop_to_node: Vec<(u32, u32)>, // (stop_index, node_index)
    pub patterns: Vec<ServicePattern>,
    pub route_names: Vec<String>,
    pub route_colors: Vec<Option<Color>>,
    /// Pre-sliced shape polylines per transit leg: (route, from_stop, to_stop) -> [(lat, lon)]
    pub leg_shapes: Vec<((u32, u32, u32), Vec<(f64, f64)>)>,
}

// Binary format v11:
// Header:
//   magic: [u8; 4] = "TRNS"
//   version: u32 = 11
//   num_nodes: u32
//   num_edges: u32
//   num_stops: u32
//   num_patterns: u32
//   num_route_names: u32
//   num_leg_shapes: u32
//
// Node ordering invariant (v11): the first `num_stops` nodes are exactly the
// transit-stop-bearing nodes, with `stop_idx == node_idx` for every stop.
// Non-stop nodes follow, Morton-sorted. This lets us drop the explicit
// stop_to_node mapping: `node_to_stop(n) = (n < num_stops).then_some(n)` and
// `stop_to_node(s) = s`.
//
// Nodes section (Morton-sorted within each of the two blocks, 32-bit fixed-
// point 0.1 m resolution):
//   min_lat: f64, min_lon: f64      // bbox origin
//   lat_scale: f64, lon_scale: f64  // units per degree (1 unit = 0.1 m)
//   pco_len: u32, pco_data: [u8]    // PCO u32 lat offsets
//   pco_len: u32, pco_data: [u8]    // PCO u32 lon offsets
//
// Edges section (canonical u>v, sorted by (u, u-v)):
//   pco_len: u32, pco_data: [u8]  // PCO u32: u values
//   pco_len: u32, pco_data: [u8]  // PCO u32: delta = u-v
//   pco_len: u32, pco_data: [u8]  // PCO u32: walk_time seconds (1.4 m/s, min 1)
//
// Stops section: [Stop; num_stops], written in new_stop_idx order so that
// stops[s] is the stop residing at node s.
//   lat: f64, lon: f64, name_len: u32, name: [u8; name_len]
//
// Route names: [name_len: u32, name: [u8; name_len]; num_route_names]
//
// Route colors: per route: has_color: u8, [r: u8, g: u8, b: u8 if has_color]
//
// Patterns section: for each pattern:
//   pattern_id: u32, day_mask: u8, start_date: u32, end_date: u32
//   num_date_add: u32, dates_add: [u32; n]
//   num_date_remove: u32, dates_remove: [u32; n]
//   min_time: u32, max_time: u32
//   num_events: u32
//   [PCO columns: time_offsets, stop_indices, travel_times, next_event_indices]
//   [PCO stop_offsets, PCO sentinel_routes]
//   num_freq: u32, freq_entries: [FreqEntry; num_freq]
//     FreqEntry: route_index, stop_index, start_time, end_time, headway_secs,
//                next_stop_index, travel_time, next_freq_index (all u32)
//
// Leg shapes section (sorted by key for binary search):
//   Six global PCO columns — keys, per-leg point counts, and the fully
//   concatenated lat/lon offsets. Shape points are i32 signed offsets at
//   0.1 m resolution against the node-section origin/scale. Concatenating
//   avoids paying PCO's per-frame overhead once per leg.
//     PCO u32: routes       (len = num_leg_shapes)
//     PCO u32: from_stops   (len = num_leg_shapes)
//     PCO u32: to_stops     (len = num_leg_shapes)
//     PCO u32: point_counts (len = num_leg_shapes)
//     PCO i32: lats_global  (len = sum of point_counts)
//     PCO i32: lons_global  (len = sum of point_counts)

/// Spread 16-bit integer bits for Morton interleaving.
fn spread_bits_16(mut x: u32) -> u32 {
    x &= 0xFFFF;
    x = (x | (x << 8)) & 0x00FF_00FF;
    x = (x | (x << 4)) & 0x0F0F_0F0F;
    x = (x | (x << 2)) & 0x3333_3333;
    x = (x | (x << 1)) & 0x5555_5555;
    x
}

/// Compute 32-bit Morton (Z-order) code from two 16-bit values.
/// Interleaves bits: result = ...y1x1y0x0 (x in even bits, y in odd bits).
fn morton_encode(x: u16, y: u16) -> u32 {
    spread_bits_16(x as u32) | (spread_bits_16(y as u32) << 1)
}

pub fn write_binary(data: &PreparedData, path: &Path) -> Result<()> {
    let num_nodes = data.nodes.len();

    // --- SFC reordering of nodes ---
    // Compute bounding box
    let min_lat = data
        .nodes
        .iter()
        .map(|n| n.lat)
        .fold(f64::INFINITY, f64::min);
    let max_lat = data
        .nodes
        .iter()
        .map(|n| n.lat)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_lon = data
        .nodes
        .iter()
        .map(|n| n.lon)
        .fold(f64::INFINITY, f64::min);
    let max_lon = data
        .nodes
        .iter()
        .map(|n| n.lon)
        .fold(f64::NEG_INFINITY, f64::max);
    let lat_range = (max_lat - min_lat).max(1e-10);
    let lon_range = (max_lon - min_lon).max(1e-10);

    // Fixed-point scales: 1 unit = 0.1 m
    const METERS_PER_DEG_LAT: f64 = 111_320.0;
    let center_lat = (min_lat + max_lat) / 2.0;
    let lat_scale = METERS_PER_DEG_LAT * 10.0; // units per degree of latitude
    let lon_scale = METERS_PER_DEG_LAT * center_lat.to_radians().cos() * 10.0;

    let scale = (1u32 << 16) as f64;

    let node_morton = |i: usize| -> u32 {
        let n = &data.nodes[i];
        let x = ((n.lon - min_lon) / lon_range * scale).min(scale - 1.0) as u16;
        let y = ((n.lat - min_lat) / lat_range * scale).min(scale - 1.0) as u16;
        morton_encode(x, y)
    };

    // Mark which old node indices carry a transit stop (and which old stop).
    // Stop-bearing nodes are pulled to the front of the array so that
    // `new_node_idx < num_stops` iff the node is a transit stop, and
    // `stop_idx == node_idx` for those slots.
    let num_stops = data.stops.len();
    let mut old_node_to_old_stop: Vec<u32> = vec![u32::MAX; num_nodes];
    for &(si, ni) in &data.stop_to_node {
        debug_assert_eq!(
            old_node_to_old_stop[ni as usize],
            u32::MAX,
            "node {ni} already mapped to a stop"
        );
        old_node_to_old_stop[ni as usize] = si;
    }

    // new_order[new_idx] = old_idx
    // Sort key: (stop-bucket, Morton). Bucket 0 is stop-bearing nodes, bucket 1
    // everything else. Within a bucket, Morton gives spatial locality.
    let mut new_order: Vec<u32> = (0..num_nodes as u32).collect();
    new_order.sort_unstable_by_key(|&i| {
        let bucket: u32 = if old_node_to_old_stop[i as usize] != u32::MAX {
            0
        } else {
            1
        };
        (bucket, node_morton(i as usize))
    });

    // old_to_new[old_idx] = new_idx
    let mut old_to_new = vec![0u32; num_nodes];
    for (new_idx, &old_idx) in new_order.iter().enumerate() {
        old_to_new[old_idx as usize] = new_idx as u32;
    }

    // old_to_new_stop[old_stop_idx] = new_stop_idx = new_node_idx
    // (the new node index happens to be < num_stops by construction).
    let mut old_to_new_stop: Vec<u32> = vec![u32::MAX; num_stops];
    for &(si, old_ni) in &data.stop_to_node {
        let new_ni = old_to_new[old_ni as usize];
        debug_assert!(
            (new_ni as usize) < num_stops,
            "stop-bearing node landed at {new_ni} outside [0, {num_stops})"
        );
        old_to_new_stop[si as usize] = new_ni;
    }
    debug_assert!(
        old_to_new_stop.iter().all(|&s| (s as usize) < num_stops),
        "every stop must map to a slot in [0, num_stops)"
    );

    // Helper: remap a stop index reference from old to new numbering.
    let remap_stop = |old: u32| -> u32 { old_to_new_stop[old as usize] };

    // Relabel edges: use new node indices, canonicalize u > v.
    // Walk time is precomputed once here (1.4 m/s walking speed, min 1 second).
    const WALKING_SPEED_MPS: f32 = 1.4;
    let mut canon_edges: Vec<(u32, u32, u16)> = data
        .edges
        .iter()
        .map(|e| {
            let new_u = old_to_new[e.u as usize];
            let new_v = old_to_new[e.v as usize];
            let (cu, cv) = if new_u > new_v {
                (new_u, new_v)
            } else {
                (new_v, new_u)
            };
            let walk_time = ((e.distance_meters / WALKING_SPEED_MPS).round() as u16).max(1);
            (cu, cu - cv, walk_time)
        })
        .collect();
    canon_edges.sort_unstable_by_key(|&(u, delta, _)| (u, delta));

    // new_stop_order[new_stop_idx] = old_stop_idx. This is the inverse of
    // old_to_new_stop and lets us iterate stops in their new order when
    // writing the stops section.
    let mut new_stop_order: Vec<u32> = vec![u32::MAX; num_stops];
    for (old_s, &new_s) in old_to_new_stop.iter().enumerate() {
        new_stop_order[new_s as usize] = old_s as u32;
    }

    let mut buf: Vec<u8> = Vec::new();

    // Header
    buf.extend_from_slice(b"TRNS");
    write_u32(&mut buf, 11); // version
    write_u32(&mut buf, num_nodes as u32);
    write_u32(&mut buf, data.edges.len() as u32);
    write_u32(&mut buf, data.stops.len() as u32);
    write_u32(&mut buf, data.patterns.len() as u32);
    write_u32(&mut buf, data.route_names.len() as u32);
    write_u32(&mut buf, data.leg_shapes.len() as u32);

    // Nodes (v5): 32-bit fixed-point, 0.1 m resolution, SFC-sorted
    {
        write_f64(&mut buf, min_lat);
        write_f64(&mut buf, min_lon);
        write_f64(&mut buf, lat_scale);
        write_f64(&mut buf, lon_scale);
        let lat_u32: Vec<u32> = new_order
            .iter()
            .map(|&i| ((data.nodes[i as usize].lat - min_lat) * lat_scale).round() as u32)
            .collect();
        let lon_u32: Vec<u32> = new_order
            .iter()
            .map(|&i| ((data.nodes[i as usize].lon - min_lon) * lon_scale).round() as u32)
            .collect();
        write_pco_u32(&mut buf, &lat_u32);
        write_pco_u32(&mut buf, &lon_u32);
    }

    // Edges: u, delta (=u-v), walk_time (seconds at 1.4 m/s, min 1)
    // Sorted by (u, delta), canonical with u > v
    {
        let us: Vec<u32> = canon_edges.iter().map(|&(u, _, _)| u).collect();
        let deltas: Vec<u32> = canon_edges.iter().map(|&(_, d, _)| d).collect();
        let walk_times: Vec<u32> = canon_edges.iter().map(|&(_, _, w)| w as u32).collect();
        write_pco_u32(&mut buf, &us);
        write_pco_u32(&mut buf, &deltas);
        write_pco_u32(&mut buf, &walk_times);
    }

    // Stops — written in new_stop_idx order, so stops[s] is the stop that
    // lives at node s (given the stop-first node ordering above).
    for &old_s in &new_stop_order {
        let stop = &data.stops[old_s as usize];
        write_f64(&mut buf, stop.lat);
        write_f64(&mut buf, stop.lon);
        let name_bytes = stop.name.as_bytes();
        write_u32(&mut buf, name_bytes.len() as u32);
        buf.extend_from_slice(name_bytes);
    }

    // Route names
    for name in &data.route_names {
        let name_bytes = name.as_bytes();
        write_u32(&mut buf, name_bytes.len() as u32);
        buf.extend_from_slice(name_bytes);
    }

    // Route colors (3 bytes each: R, G, B)
    for color in &data.route_colors {
        match color {
            Some(c) => {
                buf.push(1); // has color
                buf.push(c.r);
                buf.push(c.g);
                buf.push(c.b);
            }
            None => {
                buf.push(0); // no color
            }
        }
    }

    // Patterns
    for pattern in &data.patterns {
        write_u32(&mut buf, pattern.pattern_id);
        buf.push(pattern.day_mask);
        write_u32(&mut buf, pattern.start_date);
        write_u32(&mut buf, pattern.end_date);
        write_u32(&mut buf, pattern.date_exceptions_add.len() as u32);
        for &d in &pattern.date_exceptions_add {
            write_u32(&mut buf, d);
        }
        write_u32(&mut buf, pattern.date_exceptions_remove.len() as u32);
        for &d in &pattern.date_exceptions_remove {
            write_u32(&mut buf, d);
        }
        write_u32(&mut buf, pattern.min_time);
        write_u32(&mut buf, pattern.max_time);

        // Convert (dep_time, Event) pairs into flat events with time_offset.
        struct FlatEvent {
            time_offset: u32,
            stop_index: u32,
            route_index: u32,
            trip_index: u32,
            next_stop_index: u32,
            travel_time: u32,
        }
        let mut flat_events: Vec<FlatEvent> = Vec::with_capacity(pattern.events.len());
        for (dep_time, event) in &pattern.events {
            flat_events.push(FlatEvent {
                time_offset: dep_time.saturating_sub(pattern.min_time),
                stop_index: remap_stop(event.stop_index),
                route_index: event.route_index,
                trip_index: event.trip_index,
                next_stop_index: remap_stop(event.next_stop_index),
                travel_time: event.travel_time,
            });
        }

        // Sort by trip to group trip events together
        flat_events.sort_unstable_by_key(|e| (e.trip_index, e.time_offset));

        // Add sentinel events (arrival at final stop of each trip)
        let n = flat_events.len();
        let mut with_sentinels: Vec<FlatEvent> = Vec::with_capacity(n + n / 10);
        for i in 0..n {
            let e = &flat_events[i];
            with_sentinels.push(FlatEvent {
                time_offset: e.time_offset,
                stop_index: e.stop_index,
                route_index: e.route_index,
                trip_index: e.trip_index,
                next_stop_index: e.next_stop_index,
                travel_time: e.travel_time,
            });
            let is_last = i + 1 == n || flat_events[i + 1].trip_index != e.trip_index;
            if is_last && e.travel_time > 0 {
                with_sentinels.push(FlatEvent {
                    time_offset: e.time_offset + e.travel_time,
                    stop_index: e.next_stop_index,
                    route_index: e.route_index,
                    trip_index: e.trip_index,
                    next_stop_index: u32::MAX,
                    travel_time: 0,
                });
            }
        }

        // Compute next_event_index within each trip (indices into with_sentinels)
        let total = with_sentinels.len();
        let mut next_event_index = vec![u32::MAX; total];
        for i in 0..total.saturating_sub(1) {
            if with_sentinels[i].trip_index == with_sentinels[i + 1].trip_index {
                next_event_index[i] = (i + 1) as u32;
            }
        }

        // Sort by (stop_index, time_offset) for direct JaggedArray construction
        // Track the permutation so we can remap next_event_index
        let mut order: Vec<u32> = (0..total as u32).collect();
        order.sort_unstable_by_key(|&i| {
            let e = &with_sentinels[i as usize];
            (e.stop_index, e.time_offset)
        });

        // Build inverse permutation: inv[old_pos] = new_pos
        let mut inv = vec![0u32; total];
        for (new_pos, &old_pos) in order.iter().enumerate() {
            inv[old_pos as usize] = new_pos as u32;
        }

        // Apply permutation and remap next_event_index
        let sorted_events: Vec<FlatEvent> = order
            .iter()
            .map(|&i| {
                let e = &with_sentinels[i as usize];
                FlatEvent {
                    time_offset: e.time_offset,
                    stop_index: e.stop_index,
                    route_index: e.route_index,
                    trip_index: e.trip_index,
                    next_stop_index: e.next_stop_index,
                    travel_time: e.travel_time,
                }
            })
            .collect();
        let remapped_nei: Vec<u32> = order
            .iter()
            .map(|&i| {
                let nei = next_event_index[i as usize];
                if nei == u32::MAX {
                    u32::MAX
                } else {
                    inv[nei as usize]
                }
            })
            .collect();

        // Compute stop offsets for JaggedArray
        let num_stops = data.stops.len() as u32;
        let mut stop_offsets: Vec<u32> = vec![0; num_stops as usize + 1];
        for e in &sorted_events {
            if e.stop_index < num_stops {
                stop_offsets[e.stop_index as usize + 1] += 1;
            }
        }
        for i in 1..stop_offsets.len() {
            stop_offsets[i] += stop_offsets[i - 1];
        }

        // Serialize: num_events, 4 PCO columns (no route_index), stop_offsets, sentinel_routes
        // route_index will be reconstructed from sentinels at query time
        write_u32(&mut buf, sorted_events.len() as u32);
        let cols: [Vec<u32>; 4] = [
            sorted_events.iter().map(|e| e.time_offset).collect(),
            sorted_events.iter().map(|e| e.stop_index).collect(),
            sorted_events.iter().map(|e| e.travel_time).collect(),
            remapped_nei,
        ];
        for col in &cols {
            let compressed = pco::standalone::simple_compress(col, &pco::ChunkConfig::default())
                .expect("pco compress failed");
            write_u32(&mut buf, compressed.len() as u32);
            buf.extend_from_slice(&compressed);
        }

        // Stop offsets (num_stops + 1 entries)
        let compressed_offsets =
            pco::standalone::simple_compress(&stop_offsets, &pco::ChunkConfig::default())
                .expect("pco compress failed");
        write_u32(&mut buf, compressed_offsets.len() as u32);
        buf.extend_from_slice(&compressed_offsets);

        // Sentinel routes: for each event, if it's a sentinel (travel_time == 0), store its route_index
        let sentinel_routes: Vec<u32> = sorted_events
            .iter()
            .map(|e| if e.travel_time == 0 { e.route_index } else { 0 })
            .collect();
        let compressed_sentinel_routes =
            pco::standalone::simple_compress(&sentinel_routes, &pco::ChunkConfig::default())
                .expect("pco compress failed");
        write_u32(&mut buf, compressed_sentinel_routes.len() as u32);
        buf.extend_from_slice(&compressed_sentinel_routes);

        write_u32(&mut buf, pattern.frequency_routes.len() as u32);
        for freq in &pattern.frequency_routes {
            write_u32(&mut buf, freq.route_index);
            write_u32(&mut buf, remap_stop(freq.stop_index));
            write_u32(&mut buf, freq.start_time);
            write_u32(&mut buf, freq.end_time);
            write_u32(&mut buf, freq.headway_secs);
            write_u32(&mut buf, remap_stop(freq.next_stop_index));
            write_u32(&mut buf, freq.travel_time);
            write_u32(&mut buf, freq.next_freq_index);
        }
    }

    // Leg shapes (v9): six global PCO columns. Concatenating across legs avoids
    // paying PCO's per-frame overhead 2× per leg (was dominant for short legs).
    // Signed i32 offsets at 0.1 m let shape points extend beyond the pedestrian
    // node bbox; ±214 km range is ample.
    {
        let n = data.leg_shapes.len();
        let mut routes: Vec<u32> = Vec::with_capacity(n);
        let mut from_stops: Vec<u32> = Vec::with_capacity(n);
        let mut to_stops: Vec<u32> = Vec::with_capacity(n);
        let mut point_counts: Vec<u32> = Vec::with_capacity(n);
        let total_points: usize = data.leg_shapes.iter().map(|(_, p)| p.len()).sum();
        let mut lats_global: Vec<i32> = Vec::with_capacity(total_points);
        let mut lons_global: Vec<i32> = Vec::with_capacity(total_points);
        // Remap stop indices, then re-sort by the *new* key so the load-time
        // binary_search on (route, from_stop, to_stop) still holds.
        let mut remapped: Vec<((u32, u32, u32), usize)> = data
            .leg_shapes
            .iter()
            .enumerate()
            .map(|(i, &((route, from_stop, to_stop), _))| {
                ((route, remap_stop(from_stop), remap_stop(to_stop)), i)
            })
            .collect();
        remapped.sort_by_key(|&(k, _)| k);
        for &((route, from_stop, to_stop), orig_i) in &remapped {
            let points = &data.leg_shapes[orig_i].1;
            routes.push(route);
            from_stops.push(from_stop);
            to_stops.push(to_stop);
            point_counts.push(points.len() as u32);
            for &(lat, lon) in points {
                lats_global.push(((lat - min_lat) * lat_scale).round() as i32);
                lons_global.push(((lon - min_lon) * lon_scale).round() as i32);
            }
        }
        write_pco_u32(&mut buf, &routes);
        write_pco_u32(&mut buf, &from_stops);
        write_pco_u32(&mut buf, &to_stops);
        write_pco_u32(&mut buf, &point_counts);
        write_pco_i32(&mut buf, &lats_global);
        write_pco_i32(&mut buf, &lons_global);
    }

    // Compress with gzip
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&buf)?;
    let compressed = encoder.finish()?;

    std::fs::write(path, &compressed)?;
    eprintln!(
        "Binary: {:.2} MB uncompressed, {:.2} MB compressed",
        buf.len() as f64 / 1_048_576.0,
        compressed.len() as f64 / 1_048_576.0,
    );

    Ok(())
}

fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_f64(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_pco_u32(buf: &mut Vec<u8>, data: &[u32]) {
    let compressed = if data.is_empty() {
        Vec::new()
    } else {
        pco::standalone::simple_compress(data, &pco::ChunkConfig::default())
            .expect("pco compress failed")
    };
    write_u32(buf, compressed.len() as u32);
    buf.extend_from_slice(&compressed);
}

fn write_pco_i32(buf: &mut Vec<u8>, data: &[i32]) {
    let compressed = if data.is_empty() {
        Vec::new()
    } else {
        pco::standalone::simple_compress(data, &pco::ChunkConfig::default())
            .expect("pco compress failed")
    };
    write_u32(buf, compressed.len() as u32);
    buf.extend_from_slice(&compressed);
}
