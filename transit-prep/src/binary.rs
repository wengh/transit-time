use crate::graph::{OsmEdge, OsmNode};
use crate::gtfs::{Event, FrequencyEntry, ServicePattern, Stop};
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
        write_u32(&mut buf, pattern.events.len() as u32);
        for second_events in &pattern.events {
            write_u16(&mut buf, second_events.len() as u16);
            for event in second_events {
                write_u32(&mut buf, event.stop_index);
                write_u32(&mut buf, event.route_index);
                write_u32(&mut buf, event.trip_index);
                write_u32(&mut buf, event.next_stop_index);
                write_u32(&mut buf, event.travel_time);
            }
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

/// Deserialize binary format (used by transit-router).
/// Returns the raw uncompressed bytes after decompression.
pub fn read_binary(data: &[u8]) -> Result<PreparedDataDeserialized> {
    // Decompress
    use std::io::Read;
    let mut decoder = flate2::read::GzDecoder::new(data);
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf)?;

    let mut pos = 0;

    // Header
    let magic = &buf[pos..pos + 4];
    anyhow::ensure!(magic == b"TRNS", "Invalid magic");
    pos += 4;
    let version = read_u32(&buf, &mut pos);
    anyhow::ensure!(version == 1, "Unsupported version {}", version);
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
        edges.push(EdgeData { u, v, distance_meters: distance });
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

    // Stop-to-node
    let mut stop_to_node = Vec::with_capacity(num_stop_to_node);
    for _ in 0..num_stop_to_node {
        let stop_idx = read_u32(&buf, &mut pos);
        let node_idx = read_u32(&buf, &mut pos);
        stop_to_node.push((stop_idx, node_idx));
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
        let pattern_id = read_u32(&buf, &mut pos);
        let day_mask = buf[pos];
        pos += 1;
        let num_add = read_u32(&buf, &mut pos) as usize;
        let mut date_add = Vec::with_capacity(num_add);
        for _ in 0..num_add {
            date_add.push(read_u32(&buf, &mut pos));
        }
        let num_remove = read_u32(&buf, &mut pos) as usize;
        let mut date_remove = Vec::with_capacity(num_remove);
        for _ in 0..num_remove {
            date_remove.push(read_u32(&buf, &mut pos));
        }
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
            pattern_id,
            day_mask,
            date_exceptions_add: date_add,
            date_exceptions_remove: date_remove,
            min_time,
            max_time,
            events,
            frequency_routes: freq_entries,
        });
    }

    // Shapes
    let mut shapes = Vec::with_capacity(num_shapes);
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
        shapes.push(ShapeData { id: shape_id, points });
    }

    Ok(PreparedDataDeserialized {
        nodes,
        edges,
        stops,
        stop_to_node,
        route_names,
        patterns,
        shapes,
    })
}

// Deserialized types (used by transit-router)
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
    pub pattern_id: u32,
    pub day_mask: u8,
    pub date_exceptions_add: Vec<u32>,
    pub date_exceptions_remove: Vec<u32>,
    pub min_time: u32,
    pub max_time: u32,
    pub events: Vec<Vec<EventData>>,
    pub frequency_routes: Vec<FreqData>,
}

#[derive(Debug, Clone)]
pub struct ShapeData {
    pub id: String,
    pub points: Vec<(f64, f64)>,
}

#[derive(Debug)]
pub struct PreparedDataDeserialized {
    pub nodes: Vec<NodeData>,
    pub edges: Vec<EdgeData>,
    pub stops: Vec<StopData>,
    pub stop_to_node: Vec<(u32, u32)>,
    pub route_names: Vec<String>,
    pub patterns: Vec<PatternData>,
    pub shapes: Vec<ShapeData>,
}

// Helper functions
fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_f32(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_f64(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_le_bytes());
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
