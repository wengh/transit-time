use crate::graph::{OsmEdge, OsmNode};
use crate::gtfs::{ServicePattern, Stop};
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
    write_u32(&mut buf, 1); // version
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

        let mut flat_events = Vec::new();
        for (time_offset, second_events) in pattern.events.iter().enumerate() {
            for event in second_events {
                flat_events.push((time_offset as u32, event));
            }
        }

        flat_events.sort_unstable_by_key(|&(time_offset, e)| (e.trip_index, time_offset));

        write_u32(&mut buf, flat_events.len() as u32);
        for &(time_offset, event) in &flat_events {
            write_u32(&mut buf, time_offset);
            write_u32(&mut buf, event.stop_index);
            write_u32(&mut buf, event.route_index);
            write_u32(&mut buf, event.trip_index);
            write_u32(&mut buf, event.next_stop_index);
            write_u32(&mut buf, event.travel_time);
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

    // Shapes
    for (shape_id, points) in &data.shapes {
        let id_bytes = shape_id.as_bytes();
        write_u32(&mut buf, id_bytes.len() as u32);
        buf.extend_from_slice(id_bytes);
        write_u32(&mut buf, points.len() as u32);
        for &(lat, lon) in points {
            write_f64(&mut buf, lat);
            write_f64(&mut buf, lon);
        }
    }

    // Route-to-shape mapping
    write_u32(&mut buf, data.route_shapes.len() as u32);
    for shapes in &data.route_shapes {
        write_u32(&mut buf, shapes.len() as u32);
        for shape_id in shapes {
            let id_bytes = shape_id.as_bytes();
            write_u32(&mut buf, id_bytes.len() as u32);
            buf.extend_from_slice(id_bytes);
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
