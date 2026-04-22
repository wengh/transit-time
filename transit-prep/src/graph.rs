use anyhow::{bail, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Extract bounding box from a PBF file's header block.
/// Returns (min_lon, min_lat, max_lon, max_lat).
pub fn extract_pbf_bbox(path: &Path) -> Result<(f64, f64, f64, f64)> {
    use osmpbf::blob::BlobReader;
    let reader = BlobReader::from_path(path)?;
    for blob in reader {
        let blob = blob?;
        let header = blob.to_headerblock()?;
        if let Some(bbox) = header.bbox() {
            return Ok((bbox.left, bbox.bottom, bbox.right, bbox.top));
        }
    }
    bail!("PBF file has no bounding box in header")
}

use crate::gtfs::Stop;

const PEDESTRIAN_HIGHWAYS: &[&str] = &[
    "footway",
    "pedestrian",
    "path",
    "steps",
    "residential",
    "living_street",
    "tertiary",
    "secondary",
    "primary",
    "trunk",
    "service",
    "unclassified",
    "crossing",
    "cycleway",
    "track",
    "corridor",
];

#[derive(Debug, Clone)]
pub struct OsmNode {
    pub id: u64,
    pub lat: f64,
    pub lon: f64,
    pub index: u32,
    pub is_entrance: bool,
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
pub fn haversine(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    r * c
}

/// Raw parsed data before graph construction
struct RawOsmData {
    all_nodes: HashMap<u64, (f64, f64)>,
    entrance_node_ids: HashSet<u64>,
    ways: Vec<Vec<u64>>,
}

pub fn build_graph(osm_path: &Path, bbox: (f64, f64, f64, f64)) -> Result<OsmGraph> {
    let ext = osm_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let raw = if ext == "pbf" {
        parse_pbf(osm_path, bbox)?
    } else {
        parse_xml(osm_path)?
    };

    let mut graph = build_graph_from_raw(raw)?;
    remove_small_components(&mut graph);
    Ok(graph)
}

fn parse_xml(osm_path: &Path) -> Result<RawOsmData> {
    let xml = std::fs::read_to_string(osm_path)?;
    let mut reader = Reader::from_str(&xml);

    let mut all_nodes: HashMap<u64, (f64, f64)> = HashMap::new();
    let mut entrance_node_ids: HashSet<u64> = HashSet::new();
    let mut ways: Vec<Vec<u64>> = Vec::new();

    let mut current_way_nodes: Vec<u64> = Vec::new();
    let mut in_way = false;
    let mut current_node_id: u64 = 0;
    let mut in_node = false;
    let mut node_has_entrance_tag = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                b"node" => {
                    let mut id = 0u64;
                    let mut lat = 0.0f64;
                    let mut lon = 0.0f64;
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"id" => id = String::from_utf8_lossy(&attr.value).parse().unwrap_or(0),
                            b"lat" => {
                                lat = String::from_utf8_lossy(&attr.value).parse().unwrap_or(0.0)
                            }
                            b"lon" => {
                                lon = String::from_utf8_lossy(&attr.value).parse().unwrap_or(0.0)
                            }
                            _ => {}
                        }
                    }
                    if id != 0 {
                        all_nodes.insert(id, (lat, lon));
                        current_node_id = id;
                        in_node = true;
                        node_has_entrance_tag = false;
                    }
                }
                b"way" => {
                    in_way = true;
                    current_way_nodes.clear();
                }
                _ => {}
            },
            Ok(Event::Empty(ref e)) => match e.name().as_ref() {
                b"node" => {
                    let mut id = 0u64;
                    let mut lat = 0.0f64;
                    let mut lon = 0.0f64;
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"id" => id = String::from_utf8_lossy(&attr.value).parse().unwrap_or(0),
                            b"lat" => {
                                lat = String::from_utf8_lossy(&attr.value).parse().unwrap_or(0.0)
                            }
                            b"lon" => {
                                lon = String::from_utf8_lossy(&attr.value).parse().unwrap_or(0.0)
                            }
                            _ => {}
                        }
                    }
                    if id != 0 {
                        all_nodes.insert(id, (lat, lon));
                    }
                }
                b"nd" if in_way => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"ref" {
                            if let Ok(node_ref) =
                                String::from_utf8_lossy(&attr.value).parse::<u64>()
                            {
                                current_way_nodes.push(node_ref);
                            }
                        }
                    }
                }
                b"tag" if in_node => {
                    let mut key = Vec::new();
                    let mut val = Vec::new();
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"k" => key = attr.value.to_vec(),
                            b"v" => val = attr.value.to_vec(),
                            _ => {}
                        }
                    }
                    let k = String::from_utf8_lossy(&key);
                    let v = String::from_utf8_lossy(&val);
                    if k == "railway" && v == "subway_entrance" {
                        node_has_entrance_tag = true;
                    }
                }
                _ => {}
            },
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                b"node" => {
                    if in_node && node_has_entrance_tag {
                        entrance_node_ids.insert(current_node_id);
                    }
                    in_node = false;
                }
                b"way" => {
                    if in_way && current_way_nodes.len() >= 2 {
                        ways.push(current_way_nodes.clone());
                    }
                    in_way = false;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }

    Ok(RawOsmData {
        all_nodes,
        entrance_node_ids,
        ways,
    })
}

fn parse_pbf(osm_path: &Path, bbox: (f64, f64, f64, f64)) -> Result<RawOsmData> {
    use osmpbf::{Element, ElementReader};

    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    let reader = ElementReader::from_path(osm_path)?;

    let mut all_nodes: HashMap<u64, (f64, f64)> = HashMap::new();
    let mut entrance_node_ids: HashSet<u64> = HashSet::new();
    let mut ways: Vec<Vec<u64>> = Vec::new();

    // Collect all way node references to know which nodes to keep
    let mut way_node_refs: HashSet<i64> = HashSet::new();

    // First pass: collect ways within bbox and their node references
    eprintln!("PBF pass 1: collecting ways...");
    let reader1 = ElementReader::from_path(osm_path)?;
    reader1.for_each(|element| {
        if let Element::Way(way) = element {
            // Check if this way has a pedestrian highway tag
            let mut is_pedestrian = false;
            for (key, value) in way.tags() {
                if key == "highway" && PEDESTRIAN_HIGHWAYS.contains(&value) {
                    is_pedestrian = true;
                    break;
                }
            }
            if !is_pedestrian {
                return;
            }
            let refs: Vec<i64> = way.refs().collect();
            for &r in &refs {
                way_node_refs.insert(r);
            }
        }
    })?;

    eprintln!("PBF pass 1: {} way node refs", way_node_refs.len());

    // Second pass: collect nodes and entrance nodes, and re-collect ways
    eprintln!("PBF pass 2: collecting nodes and building ways...");
    reader.for_each(|element| {
        match element {
            Element::Node(node) => {
                let id = node.id();
                let lat = node.lat();
                let lon = node.lon();

                // Keep node if: referenced by a way, or is within bbox
                let in_bbox = lat >= min_lat && lat <= max_lat && lon >= min_lon && lon <= max_lon;
                let needed = way_node_refs.contains(&id) || in_bbox;

                if needed {
                    all_nodes.insert(id as u64, (lat, lon));
                }

                // Check for subway entrance tag (only within bbox)
                if in_bbox {
                    for (key, value) in node.tags() {
                        if key == "railway" && value == "subway_entrance" {
                            all_nodes.insert(id as u64, (lat, lon));
                            entrance_node_ids.insert(id as u64);
                            break;
                        }
                    }
                }
            }
            Element::DenseNode(node) => {
                let id = node.id();
                let lat = node.lat();
                let lon = node.lon();

                let in_bbox = lat >= min_lat && lat <= max_lat && lon >= min_lon && lon <= max_lon;
                let needed = way_node_refs.contains(&id) || in_bbox;

                if needed {
                    all_nodes.insert(id as u64, (lat, lon));
                }

                if in_bbox {
                    for (key, value) in node.tags() {
                        if key == "railway" && value == "subway_entrance" {
                            all_nodes.insert(id as u64, (lat, lon));
                            entrance_node_ids.insert(id as u64);
                            break;
                        }
                    }
                }
            }
            Element::Way(way) => {
                let mut is_pedestrian = false;
                for (key, value) in way.tags() {
                    if key == "highway" && PEDESTRIAN_HIGHWAYS.contains(&value) {
                        is_pedestrian = true;
                        break;
                    }
                }
                if !is_pedestrian {
                    return;
                }

                let refs: Vec<u64> = way.refs().map(|r| r as u64).collect();

                // Only include ways that have at least one node in bbox
                let has_bbox_node = refs.iter().any(|&r| {
                    all_nodes.get(&r).is_some_and(|&(lat, lon)| {
                        lat >= min_lat && lat <= max_lat && lon >= min_lon && lon <= max_lon
                    })
                });

                if has_bbox_node && refs.len() >= 2 {
                    ways.push(refs);
                }
            }
            _ => {}
        }
    })?;

    eprintln!(
        "PBF: {} nodes, {} entrance nodes, {} ways",
        all_nodes.len(),
        entrance_node_ids.len(),
        ways.len()
    );

    Ok(RawOsmData {
        all_nodes,
        entrance_node_ids,
        ways,
    })
}

fn build_graph_from_raw(raw: RawOsmData) -> Result<OsmGraph> {
    let RawOsmData {
        all_nodes,
        entrance_node_ids,
        ways,
    } = raw;

    eprintln!("Found {} station entrance nodes", entrance_node_ids.len());

    // Find intersection/endpoint nodes
    let mut node_usage_count: HashMap<u64, u32> = HashMap::new();
    for way in &ways {
        for (i, &node_id) in way.iter().enumerate() {
            let count = node_usage_count.entry(node_id).or_insert(0);
            if i == 0 || i == way.len() - 1 {
                *count += 2;
            } else {
                *count += 1;
            }
        }
    }

    // Graph nodes: intersections + endpoints + entrance nodes
    let mut graph_node_ids: HashSet<u64> = node_usage_count
        .iter()
        .filter(|&(_, &count)| count >= 2)
        .map(|(&id, _)| id)
        .collect();

    for &entrance_id in &entrance_node_ids {
        graph_node_ids.insert(entrance_id);
    }

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
                is_entrance: entrance_node_ids.contains(&node_id),
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

    // Connect entrance nodes that aren't part of any way to nearest street node
    let entrance_only: Vec<u32> = nodes
        .iter()
        .filter(|n| n.is_entrance && !node_usage_count.contains_key(&n.id))
        .map(|n| n.index)
        .collect();

    // Create a spatial grid to accelerate nearest street node lookup
    let cell_size = 0.002; // ~220m
    let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
    for node in &nodes {
        if node.is_entrance || !node_usage_count.contains_key(&node.id) {
            continue;
        }
        let lat_idx = (node.lat / cell_size).floor() as i32;
        let lon_idx = (node.lon / cell_size).floor() as i32;
        grid.entry((lat_idx, lon_idx)).or_default().push(node.index);
    }

    for &ent_idx in &entrance_only {
        let ent = &nodes[ent_idx as usize];
        let mut best_dist = f64::MAX;
        let mut best_idx = None;

        let lat_idx = (ent.lat / cell_size).floor() as i32;
        let lon_idx = (ent.lon / cell_size).floor() as i32;

        for dlat in -1..=1 {
            for dlon in -1..=1 {
                if let Some(cell_nodes) = grid.get(&(lat_idx + dlat, lon_idx + dlon)) {
                    for &node_idx in cell_nodes {
                        let node = &nodes[node_idx as usize];
                        let dist = haversine(ent.lat, ent.lon, node.lat, node.lon);
                        if dist < best_dist && dist < 200.0 {
                            best_dist = dist;
                            best_idx = Some(node.index);
                        }
                    }
                }
            }
        }

        if let Some(street_idx) = best_idx {
            let (u, v) = if ent_idx < street_idx {
                (ent_idx, street_idx)
            } else {
                (street_idx, ent_idx)
            };
            if edge_set.insert((u, v)) {
                edges.push(OsmEdge {
                    u: ent_idx,
                    v: street_idx,
                    distance_meters: best_dist as f32,
                });
            }
        }
    }

    if !entrance_only.is_empty() {
        eprintln!(
            "Connected {} standalone entrance nodes to street network",
            entrance_only.len()
        );
    }

    Ok(OsmGraph { nodes, edges })
}

/// Project `p` onto segment `a`→`b` using a caller-supplied `cos_lat`
/// (typically the cosine of a region-representative latitude).
/// Returns (t, projected point, squared distance from p to projection),
/// where t is the clamped fractional position along AB (0.0 = A, 1.0 = B).
pub(crate) fn project_on_segment(
    p: (f64, f64),
    a: (f64, f64),
    b: (f64, f64),
    cos_lat: f64,
) -> (f64, (f64, f64), f64) {
    let dx = (b.1 - a.1) * cos_lat;
    let dy = b.0 - a.0;
    let len_sq = dx * dx + dy * dy;
    let t = if len_sq < 1e-20 {
        0.0
    } else {
        let num = ((p.1 - a.1) * cos_lat) * dx + (p.0 - a.0) * dy;
        (num / len_sq).clamp(0.0, 1.0)
    };
    let proj = (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
    let ddlat = p.0 - proj.0;
    let ddlon = (p.1 - proj.1) * cos_lat;
    (t, proj, ddlat * ddlat + ddlon * ddlon)
}

struct SnapResult {
    stop_index: u32,
    edge_index: usize,
    t: f64, // fractional position along edge
    proj_lat: f64,
    proj_lon: f64,
    dist: f64, // distance from stop to projected point in meters
}

/// Snap each transit stop to its nearest point on an OSM edge.
/// For each stop, creates a node at the stop's original lat/lon and connects
/// it with an edge to the nearest point on the graph (splitting the edge if needed).
/// Each stop is guaranteed to have a unique new node, so no multiple stops share a node.
pub fn snap_stops_to_nodes(stops: &[Stop], graph: &mut OsmGraph) -> Vec<(u32, u32)> {
    const MAX_SNAP_DISTANCE_METERS: f64 = 400.0;
    const CELL_SIZE_LAT: f64 = 0.0045;
    const CELL_SIZE_LON: f64 = 0.006;

    // Region-representative cos(lat) for cheap planar projection.
    // For a city-scale graph the error vs. per-segment midpoint cos_lat is
    // negligible, and we save a trig call per edge projection.
    let cos_lat = if stops.is_empty() {
        1.0
    } else {
        let (mut min_lat, mut max_lat) = (f64::INFINITY, f64::NEG_INFINITY);
        for s in stops {
            min_lat = min_lat.min(s.lat);
            max_lat = max_lat.max(s.lat);
        }
        ((min_lat + max_lat) / 2.0).to_radians().cos()
    };

    // Pass 1: Compute snap points (read-only on graph) — run in parallel per stop.
    // Wrapped in a block so the immutable reborrow of `graph` is released before
    // Pass 2 takes `&mut graph`.
    let snap_results: Vec<SnapResult> = {
        let graph_ref: &OsmGraph = &*graph; // immutable reborrow from &mut

        // Build spatial grid index over edges (indexed by all bounding-box cells)
        let mut edge_grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (i, edge) in graph_ref.edges.iter().enumerate() {
            let u = &graph_ref.nodes[edge.u as usize];
            let v = &graph_ref.nodes[edge.v as usize];
            let min_lat = (u.lat.min(v.lat) / CELL_SIZE_LAT).floor() as i32;
            let max_lat = (u.lat.max(v.lat) / CELL_SIZE_LAT).floor() as i32;
            let min_lon = (u.lon.min(v.lon) / CELL_SIZE_LON).floor() as i32;
            let max_lon = (u.lon.max(v.lon) / CELL_SIZE_LON).floor() as i32;
            for lat_cell in min_lat..=max_lat {
                for lon_cell in min_lon..=max_lon {
                    edge_grid.entry((lat_cell, lon_cell)).or_default().push(i);
                }
            }
        }

        stops
            .par_iter()
            .filter_map(|stop| {
                let cell_lat = (stop.lat / CELL_SIZE_LAT).floor() as i32;
                let cell_lon = (stop.lon / CELL_SIZE_LON).floor() as i32;

                let mut best_dist = f64::MAX;
                let mut best_snap: Option<SnapResult> = None;
                let mut seen_edges: HashSet<usize> = HashSet::new(); // local per stop

                for dlat in -1..=1 {
                    for dlon in -1..=1 {
                        if let Some(edge_indices) =
                            edge_grid.get(&(cell_lat + dlat, cell_lon + dlon))
                        {
                            for &ei in edge_indices {
                                if !seen_edges.insert(ei) {
                                    continue;
                                }
                                let edge = &graph_ref.edges[ei];
                                let u = &graph_ref.nodes[edge.u as usize];
                                let v = &graph_ref.nodes[edge.v as usize];
                                let (t, (proj_lat, proj_lon), _) = project_on_segment(
                                    (stop.lat, stop.lon),
                                    (u.lat, u.lon),
                                    (v.lat, v.lon),
                                    cos_lat,
                                );
                                let dist = haversine(stop.lat, stop.lon, proj_lat, proj_lon);
                                if dist < best_dist {
                                    best_dist = dist;
                                    best_snap = Some(SnapResult {
                                        stop_index: stop.index,
                                        edge_index: ei,
                                        t,
                                        proj_lat,
                                        proj_lon,
                                        dist,
                                    });
                                }
                            }
                        }
                    }
                }

                best_snap.filter(|s| s.dist <= MAX_SNAP_DISTANCE_METERS)
            })
            .collect()
        // graph_ref and edge_grid are dropped here → exclusive borrow of graph restored
    };

    let skipped = stops.len() - snap_results.len();

    // Pass 2: Group snaps by edge, sort by t, mutate graph
    let mut snaps_by_edge: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, snap) in snap_results.iter().enumerate() {
        snaps_by_edge.entry(snap.edge_index).or_default().push(i);
    }

    let mut mapping: Vec<(u32, u32)> = Vec::new();
    let mut edges_to_remove: HashSet<usize> = HashSet::new();

    for (edge_idx, mut snap_indices) in snaps_by_edge {
        // Sort snaps along the edge by t
        snap_indices.sort_by(|&a, &b| snap_results[a].t.partial_cmp(&snap_results[b].t).unwrap());

        let orig_edge = &graph.edges[edge_idx];
        let orig_u = orig_edge.u;
        let orig_v = orig_edge.v;
        let orig_dist = orig_edge.distance_meters;
        edges_to_remove.insert(edge_idx);

        // Walk along the edge, splitting at each snap point
        let mut prev_node = orig_u;
        let mut prev_t = 0.0f64;

        for &si in &snap_indices {
            let snap = &snap_results[si];

            // Determine the connection node on the edge
            let conn_node = if snap.t < 0.001 {
                // Near endpoint u — reuse it, no split needed
                orig_u
            } else if snap.t > 0.999 {
                // Near endpoint v — reuse it, no split needed
                orig_v
            } else {
                // Edge from previous split point to projection node.
                // If seg_dist rounds to 0 (two snaps at nearly the same t),
                // reuse prev_node rather than creating a dangling projection node.
                let seg_dist = ((snap.t - prev_t) * orig_dist as f64) as f32;
                if seg_dist == 0.0 {
                    prev_node
                } else {
                    let proj_index = graph.nodes.len() as u32;
                    graph.nodes.push(OsmNode {
                        id: 0,
                        lat: snap.proj_lat,
                        lon: snap.proj_lon,
                        index: proj_index,
                        is_entrance: false,
                    });
                    graph.edges.push(OsmEdge {
                        u: prev_node,
                        v: proj_index,
                        distance_meters: seg_dist,
                    });
                    prev_node = proj_index;
                    prev_t = snap.t;
                    proj_index
                }
            };

            // Create stop node at the original stop position and connect it.
            // If snap.dist is 0 the stop is exactly on the edge; reuse conn_node
            // directly so we don't create an isolated zero-distance duplicate,
            // unless the conn_node is an existing node.
            if snap.dist > 0.0 || conn_node == orig_u || conn_node == orig_v {
                let stop_node = graph.nodes.len() as u32;
                graph.nodes.push(OsmNode {
                    id: 0,
                    lat: stops[snap.stop_index as usize].lat,
                    lon: stops[snap.stop_index as usize].lon,
                    index: stop_node,
                    is_entrance: false,
                });
                graph.edges.push(OsmEdge {
                    u: stop_node,
                    v: conn_node,
                    distance_meters: snap.dist as f32,
                });
                mapping.push((snap.stop_index, stop_node));
            } else {
                mapping.push((snap.stop_index, conn_node));
            }
        }

        // Final segment from last split point to original endpoint v
        let final_dist = ((1.0 - prev_t) * orig_dist as f64) as f32;
        if final_dist > 0.0 {
            graph.edges.push(OsmEdge {
                u: prev_node,
                v: orig_v,
                distance_meters: final_dist,
            });
        }
    }

    // Remove original split edges (swap-remove in reverse order to keep indices valid)
    let mut remove_sorted: Vec<usize> = edges_to_remove.into_iter().collect();
    remove_sorted.sort_unstable_by(|a, b| b.cmp(a));
    let num_splits = remove_sorted.len();
    for idx in remove_sorted {
        graph.edges.swap_remove(idx);
    }

    if skipped > 0 {
        eprintln!(
            "Skipped {} stops (too far from OSM graph, >{:.0}m)",
            skipped, MAX_SNAP_DISTANCE_METERS
        );
    }
    eprintln!(
        "Snapped {} stops ({} required edge splits)",
        snap_results.len(),
        num_splits
    );

    mapping
}

/// Prune all graph nodes unreachable from any transit stop via walking edges.
/// Remaps node indices in the graph and stop_to_node mapping.
/// Remove nodes (and their edges) where `keep[i]` is false, remapping indices.
/// Returns the remap table (old index → new index, u32::MAX if removed).
fn retain_nodes(graph: &mut OsmGraph, keep: &[bool]) -> Vec<u32> {
    let n = graph.nodes.len();
    let mut remap: Vec<u32> = vec![u32::MAX; n];
    let mut new_idx = 0u32;
    for i in 0..n {
        if keep[i] {
            remap[i] = new_idx;
            new_idx += 1;
        }
    }

    graph.nodes = graph
        .nodes
        .drain(..)
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(_, mut node)| {
            node.index = remap[node.index as usize];
            node
        })
        .collect();

    graph.edges = graph
        .edges
        .drain(..)
        .filter(|e| keep[e.u as usize] && keep[e.v as usize])
        .map(|mut e| {
            e.u = remap[e.u as usize];
            e.v = remap[e.v as usize];
            e
        })
        .collect();

    remap
}

/// Build an adjacency list for the current graph.
fn build_adj(graph: &OsmGraph) -> Vec<Vec<u32>> {
    let n = graph.nodes.len();
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
    for edge in &graph.edges {
        adj[edge.u as usize].push(edge.v);
        adj[edge.v as usize].push(edge.u);
    }
    adj
}

/// Remove connected components with fewer than `min_size` nodes.
/// This prevents transit stops from snapping to tiny disconnected street fragments
/// (e.g. opposite sides of elevated tracks that aren't connected in OSM data).
fn remove_small_components(graph: &mut OsmGraph) {
    const MIN_COMPONENT_SIZE: usize = 50;

    let n = graph.nodes.len();
    if n == 0 {
        return;
    }

    let adj = build_adj(graph);

    // Label each node with its component and track component sizes
    let mut component: Vec<u32> = vec![u32::MAX; n];
    let mut component_sizes: Vec<usize> = Vec::new();
    let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();

    for start in 0..n {
        if component[start] != u32::MAX {
            continue;
        }
        let comp_id = component_sizes.len() as u32;
        let mut size = 0usize;
        component[start] = comp_id;
        queue.push_back(start as u32);
        while let Some(u) = queue.pop_front() {
            size += 1;
            for &v in &adj[u as usize] {
                if component[v as usize] == u32::MAX {
                    component[v as usize] = comp_id;
                    queue.push_back(v);
                }
            }
        }
        component_sizes.push(size);
    }

    let keep: Vec<bool> = (0..n)
        .map(|i| component_sizes[component[i] as usize] >= MIN_COMPONENT_SIZE)
        .collect();
    let remove_count = keep.iter().filter(|&&k| !k).count();
    let num_small = component_sizes
        .iter()
        .filter(|&&s| s < MIN_COMPONENT_SIZE)
        .count();

    if remove_count == 0 {
        eprintln!(
            "No small components to remove ({} nodes in {} components)",
            n,
            component_sizes.len()
        );
        return;
    }

    retain_nodes(graph, &keep);
    eprintln!(
        "Removed {} small components ({} nodes), kept {} nodes",
        num_small,
        remove_count,
        n - remove_count
    );
}

pub fn prune_unreachable_nodes(
    graph: &mut OsmGraph,
    stop_to_node: Vec<(u32, u32)>,
) -> Vec<(u32, u32)> {
    let n = graph.nodes.len();
    let adj = build_adj(graph);

    // BFS from all stop nodes to find everything reachable by walking
    let mut keep = vec![false; n];
    let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    for &(_, node) in &stop_to_node {
        if !keep[node as usize] {
            keep[node as usize] = true;
            queue.push_back(node);
        }
    }
    while let Some(u) = queue.pop_front() {
        for &v in &adj[u as usize] {
            if !keep[v as usize] {
                keep[v as usize] = true;
                queue.push_back(v);
            }
        }
    }

    let pruned = keep.iter().filter(|&&k| !k).count();
    let kept = n - pruned;

    if pruned == 0 {
        eprintln!("All {} nodes reachable from stops, nothing to prune", n);
        return stop_to_node;
    }

    let remap = retain_nodes(graph, &keep);

    eprintln!(
        "Pruned {} unreachable nodes ({} kept, {:.1}% reduction)",
        pruned,
        kept,
        pruned as f64 / n as f64 * 100.0
    );

    // Remap stop_to_node
    stop_to_node
        .into_iter()
        .map(|(stop, node)| (stop, remap[node as usize]))
        .collect()
}
