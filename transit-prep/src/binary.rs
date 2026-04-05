use crate::graph::{OsmEdge, OsmNode};
use crate::gtfs::{Color, ServicePattern, Stop};
use anyhow::Result;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// All prepared data ready for serialization.
pub struct PreparedData {
    pub nodes: Vec<OsmNode>,
    pub edges: Vec<OsmEdge>,
    pub stops: Vec<Stop>,
    pub stop_to_node: Vec<(u32, u32)>, // (stop_index, node_index)
    pub patterns: Vec<ServicePattern>,
    pub shapes: HashMap<String, Vec<(f64, f64)>>,
    pub route_names: Vec<String>,
    pub route_colors: Vec<Option<Color>>,
    pub route_shapes: Vec<Vec<String>>, // route_index -> shape_id
}

// Binary format:
// Header:
//   magic: [u8; 4] = "TRNS"
//   version: u32 = 1
//   num_nodes: u32
//   num_edges: u32
//   num_stops: u32
//   num_stop_to_node: u32
//   num_patterns: u32
//   num_route_names: u32
//   num_shapes: u32
//
// Nodes section: [Node; num_nodes]
//   lat: f64, lon: f64
//
// Edges section: [Edge; num_edges]
//   u: u32, v: u32, distance_meters: f32
//
// Stops section: [Stop; num_stops]
//   lat: f64, lon: f64, name_len: u32, name: [u8; name_len]
//
// Stop-to-node mapping: [(u32, u32); num_stop_to_node]
//
// Route names: [name_len: u32, name: [u8; name_len]; num_route_names]
//
// Patterns section: for each pattern:
//   pattern_id: u32
//   day_mask: u8
//   num_date_add: u32, dates_add: [u32; n]
//   num_date_remove: u32, dates_remove: [u32; n]
//   min_time: u32
//   max_time: u32
//   event_array_len: u32
//   for each second:
//     num_events: u16
//     events: [Event; num_events]
//       stop_index: u32, route_index: u32, trip_index: u32, next_stop_index: u32, travel_time: u32
//   num_freq: u32
//   freq_entries: [FreqEntry; num_freq]
//
// Shapes section: for each shape:
//   shape_id_len: u32, shape_id: [u8; n]
//   num_points: u32
//   points: [(f64, f64); num_points]

pub fn write_binary(data: &PreparedData, path: &Path) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();

    // Header
    buf.extend_from_slice(b"TRNS");
    write_u32(&mut buf, 4); // version
    write_u32(&mut buf, data.nodes.len() as u32);
    write_u32(&mut buf, data.edges.len() as u32);
    write_u32(&mut buf, data.stops.len() as u32);
    write_u32(&mut buf, data.stop_to_node.len() as u32);
    write_u32(&mut buf, data.patterns.len() as u32);
    write_u32(&mut buf, data.route_names.len() as u32);
    write_u32(&mut buf, data.shapes.len() as u32);

    // Nodes
    for node in &data.nodes {
        write_f64(&mut buf, node.lat);
        write_f64(&mut buf, node.lon);
    }

    // Edges
    for edge in &data.edges {
        write_u32(&mut buf, edge.u);
        write_u32(&mut buf, edge.v);
        write_f32(&mut buf, edge.distance_meters);
    }

    // Stops
    for stop in &data.stops {
        write_f64(&mut buf, stop.lat);
        write_f64(&mut buf, stop.lon);
        let name_bytes = stop.name.as_bytes();
        write_u32(&mut buf, name_bytes.len() as u32);
        buf.extend_from_slice(name_bytes);
    }

    // Stop-to-node mapping
    for &(stop_idx, node_idx) in &data.stop_to_node {
        write_u32(&mut buf, stop_idx);
        write_u32(&mut buf, node_idx);
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

        // Collect flat events from the per-second arrays
        struct FlatEvent {
            time_offset: u32,
            stop_index: u32,
            route_index: u32,
            trip_index: u32,
            next_stop_index: u32,
            travel_time: u32,
        }
        let mut flat_events: Vec<FlatEvent> = Vec::new();
        for (time_offset, second_events) in pattern.events.iter().enumerate() {
            for event in second_events {
                flat_events.push(FlatEvent {
                    time_offset: time_offset as u32,
                    stop_index: event.stop_index,
                    route_index: event.route_index,
                    trip_index: event.trip_index,
                    next_stop_index: event.next_stop_index,
                    travel_time: event.travel_time,
                });
            }
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

        // Serialize events as JaggedArray<EventData>:
        // num_stops, offsets[num_stops+1], flat events (4 u32s each)
        write_u32(&mut buf, num_stops);
        for &offset in &stop_offsets {
            write_u32(&mut buf, offset);
        }
        for (i, e) in sorted_events.iter().enumerate() {
            write_u32(&mut buf, e.time_offset);
            write_u32(&mut buf, e.stop_index);
            write_u32(&mut buf, e.travel_time);
            write_u32(&mut buf, remapped_nei[i]);
        }

        // Sentinel routes: sparse (num_sentinels, then (event_idx, route_idx) pairs)
        let sentinels: Vec<(u32, u32)> = sorted_events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.travel_time == 0 && e.route_index != 0)
            .map(|(i, e)| (i as u32, e.route_index))
            .collect();
        write_u32(&mut buf, sentinels.len() as u32);
        for (idx, route_idx) in sentinels {
            write_u32(&mut buf, idx);
            write_u32(&mut buf, route_idx);
        }

        write_u32(&mut buf, pattern.frequency_routes.len() as u32);
        for freq in &pattern.frequency_routes {
            write_u32(&mut buf, freq.route_index);
            write_u32(&mut buf, freq.stop_index);
            write_u32(&mut buf, freq.start_time);
            write_u32(&mut buf, freq.end_time);
            write_u32(&mut buf, freq.headway_secs);
            write_u32(&mut buf, freq.next_stop_index);
            write_u32(&mut buf, freq.travel_time);
        }
    }

    // Shapes: build index mapping and compress with PCO per shape
    // Sort shape IDs for consistent output order
    let mut sorted_shape_ids: Vec<_> = data.shapes.keys().collect();
    sorted_shape_ids.sort();

    // Map shape_id -> shape_index based on sorted order
    let mut shape_id_to_index: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    for (idx, shape_id) in sorted_shape_ids.iter().enumerate() {
        shape_id_to_index.insert((*shape_id).clone(), idx as u32);
    }

    // Write shapes as JaggedArray<u8>: offsets[num_shapes+1], then all compressed data
    // num_shapes already written in header
    let mut shapes_offsets: Vec<u32> = vec![0];
    let mut shapes_data: Vec<u8> = Vec::new();

    for shape_id_ref in &sorted_shape_ids {
        let shape_id = *shape_id_ref;
        let points = &data.shapes[shape_id];

        let coords: Vec<u32> = points
            .iter()
            .flat_map(|&(lat, lon)| [(lat as f32).to_bits(), (lon as f32).to_bits()])
            .collect();

        let compressed = pco::standalone::simple_compress(&coords, &pco::ChunkConfig::default())
            .expect("pco compress failed");

        shapes_data.extend_from_slice(&compressed);
        shapes_offsets.push(shapes_data.len() as u32);
    }

    for &offset in &shapes_offsets {
        write_u32(&mut buf, offset);
    }
    buf.extend_from_slice(&shapes_data);

    // Route-to-shape mapping: use shape indices instead of IDs
    write_u32(&mut buf, data.route_shapes.len() as u32);
    for shapes_for_route in &data.route_shapes {
        write_u32(&mut buf, shapes_for_route.len() as u32);
        for shape_id in shapes_for_route {
            // Look up the shape index from the sorted mapping
            let shape_idx = shape_id_to_index.get(shape_id).copied().unwrap_or(u32::MAX);
            write_u32(&mut buf, shape_idx);
        }
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

fn write_f32(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_f64(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_le_bytes());
}
