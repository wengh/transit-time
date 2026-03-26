use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::gtfs::Stop;

#[derive(Debug, Clone)]
pub struct OsmNode {
    pub id: u64,
    pub lat: f64,
    pub lon: f64,
    pub index: u32,
}

#[derive(Debug, Clone)]
pub struct OsmEdge {
    pub u: u32, // node index
    pub v: u32, // node index
    pub distance_meters: f32,
}

#[derive(Debug)]
pub struct OsmGraph {
    pub nodes: Vec<OsmNode>,
    pub edges: Vec<OsmEdge>,
}

/// Haversine distance in meters
fn haversine(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_000.0; // Earth radius in meters
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    r * c
}

pub fn build_graph(osm_path: &Path) -> Result<OsmGraph> {
    let xml = std::fs::read_to_string(osm_path)?;
    let mut reader = Reader::from_str(&xml);

    // First pass: collect all nodes and ways
    let mut all_nodes: HashMap<u64, (f64, f64)> = HashMap::new();
    let mut ways: Vec<Vec<u64>> = Vec::new();

    let mut current_way_nodes: Vec<u64> = Vec::new();
    let mut in_way = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                match e.name().as_ref() {
                    b"node" => {
                        let mut id = 0u64;
                        let mut lat = 0.0f64;
                        let mut lon = 0.0f64;
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"id" => id = String::from_utf8_lossy(&attr.value).parse().unwrap_or(0),
                                b"lat" => lat = String::from_utf8_lossy(&attr.value).parse().unwrap_or(0.0),
                                b"lon" => lon = String::from_utf8_lossy(&attr.value).parse().unwrap_or(0.0),
                                _ => {}
                            }
                        }
                        if id != 0 {
                            all_nodes.insert(id, (lat, lon));
                        }
                    }
                    b"way" => {
                        in_way = true;
                        current_way_nodes.clear();
                    }
                    b"nd" if in_way => {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"ref" {
                                if let Ok(node_ref) = String::from_utf8_lossy(&attr.value).parse::<u64>() {
                                    current_way_nodes.push(node_ref);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                if e.name().as_ref() == b"way" && in_way {
                    if current_way_nodes.len() >= 2 {
                        ways.push(current_way_nodes.clone());
                    }
                    in_way = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }

    // Find intersection/endpoint nodes (nodes that appear in multiple ways or are endpoints)
    let mut node_usage_count: HashMap<u64, u32> = HashMap::new();
    for way in &ways {
        for (i, &node_id) in way.iter().enumerate() {
            let count = node_usage_count.entry(node_id).or_insert(0);
            if i == 0 || i == way.len() - 1 {
                *count += 2; // endpoints always become graph nodes
            } else {
                *count += 1;
            }
        }
    }

    // Graph nodes: intersections (count >= 2) and endpoints
    let graph_node_ids: HashSet<u64> = node_usage_count
        .iter()
        .filter(|(_, &count)| count >= 2)
        .map(|(&id, _)| id)
        .collect();

    // Create indexed node list
    let mut node_id_to_index: HashMap<u64, u32> = HashMap::new();
    let mut nodes: Vec<OsmNode> = Vec::new();
    for &node_id in &graph_node_ids {
        if let Some(&(lat, lon)) = all_nodes.get(&node_id) {
            let index = nodes.len() as u32;
            node_id_to_index.insert(node_id, index);
            nodes.push(OsmNode {
                id: node_id,
                lat,
                lon,
                index,
            });
        }
    }

    // Build edges by tracing ways between graph nodes
    let mut edge_set: HashSet<(u32, u32)> = HashSet::new();
    let mut edges: Vec<OsmEdge> = Vec::new();

    for way in &ways {
        let mut seg_start_idx: Option<u32> = None;
        let mut seg_distance = 0.0f64;
        let mut prev_coords: Option<(f64, f64)> = None;

        for &node_id in way {
            if let Some(&(lat, lon)) = all_nodes.get(&node_id) {
                if let Some((plat, plon)) = prev_coords {
                    seg_distance += haversine(plat, plon, lat, lon);
                }
                prev_coords = Some((lat, lon));

                if let Some(&node_idx) = node_id_to_index.get(&node_id) {
                    if let Some(start_idx) = seg_start_idx {
                        if start_idx != node_idx && seg_distance > 0.0 {
                            let (u, v) = if start_idx < node_idx {
                                (start_idx, node_idx)
                            } else {
                                (node_idx, start_idx)
                            };
                            if edge_set.insert((u, v)) {
                                edges.push(OsmEdge {
                                    u: start_idx,
                                    v: node_idx,
                                    distance_meters: seg_distance as f32,
                                });
                            }
                        }
                    }
                    seg_start_idx = Some(node_idx);
                    seg_distance = 0.0;
                }
            }
        }
    }

    Ok(OsmGraph { nodes, edges })
}

/// Snap each transit stop to its nearest OSM node. Returns stop_index -> node_index mapping.
/// Only snaps stops within MAX_SNAP_DISTANCE_METERS of an OSM node.
pub fn snap_stops_to_nodes(stops: &[Stop], graph: &OsmGraph) -> Vec<(u32, u32)> {
    const MAX_SNAP_DISTANCE_METERS: f64 = 400.0; // ~5 min walk
    let mut mapping = Vec::new();
    let mut skipped = 0;

    for stop in stops {
        let mut best_dist = f64::MAX;
        let mut best_node = 0u32;

        for node in &graph.nodes {
            let dist = haversine(stop.lat, stop.lon, node.lat, node.lon);
            if dist < best_dist {
                best_dist = dist;
                best_node = node.index;
            }
        }

        if best_dist <= MAX_SNAP_DISTANCE_METERS {
            mapping.push((stop.index, best_node));
        } else {
            skipped += 1;
        }
    }

    if skipped > 0 {
        eprintln!("Skipped {} stops (too far from OSM graph, >{:.0}m)", skipped, MAX_SNAP_DISTANCE_METERS);
    }

    mapping
}
