use anyhow::{Result, bail};
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
    let mut ways: Vec<Vec<u64>> = Vec::new();

    let mut current_way_nodes: Vec<u64> = Vec::new();
    let mut in_way = false;

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
                _ => {}
            },
            Ok(Event::End(ref e)) => match e.name().as_ref() {
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

    Ok(RawOsmData { all_nodes, ways })
}

fn parse_pbf(osm_path: &Path, bbox: (f64, f64, f64, f64)) -> Result<RawOsmData> {
    use osmpbf::{Element, ElementReader};

    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    let reader = ElementReader::from_path(osm_path)?;

    let mut all_nodes: HashMap<u64, (f64, f64)> = HashMap::new();
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

    // Second pass: collect nodes and re-collect ways
    eprintln!("PBF pass 2: collecting nodes and building ways...");
    reader.for_each(|element| {
        match element {
            Element::Node(node) => {
                let id = node.id();
                let lat = node.lat();
                let lon = node.lon();

                if way_node_refs.contains(&id) {
                    all_nodes.insert(id as u64, (lat, lon));
                }
            }
            Element::DenseNode(node) => {
                let id = node.id();
                let lat = node.lat();
                let lon = node.lon();

                if way_node_refs.contains(&id) {
                    all_nodes.insert(id as u64, (lat, lon));
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

    eprintln!("PBF: {} nodes, {} ways", all_nodes.len(), ways.len());

    Ok(RawOsmData { all_nodes, ways })
}

fn build_graph_from_raw(raw: RawOsmData) -> Result<OsmGraph> {
    let RawOsmData { all_nodes, ways } = raw;

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

    // Graph nodes: intersections + endpoints
    let graph_node_ids: HashSet<u64> = node_usage_count
        .iter()
        .filter(|&(_, &count)| count >= 2)
        .map(|(&id, _)| id)
        .collect();

    // Create indexed node list
    let mut node_id_to_index: HashMap<u64, u32> = HashMap::new();
    let mut nodes: Vec<OsmNode> = Vec::new();
    for &node_id in &graph_node_ids {
        if let Some(&(lat, lon)) = all_nodes.get(&node_id) {
            let index = nodes.len() as u32;
            node_id_to_index.insert(node_id, index);
            nodes.push(OsmNode { lat, lon, index });
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
                        lat: snap.proj_lat,
                        lon: snap.proj_lon,
                        index: proj_index,
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
                    lat: stops[snap.stop_index as usize].lat,
                    lon: stops[snap.stop_index as usize].lon,
                    index: stop_node,
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

/// Iteratively remove degree-1 "leaf" nodes that aren't transit stops. These
/// are pedestrian dead-ends (driveways, footway stubs, service-road tails)
/// that no shortest path can ever traverse meaningfully — you'd only walk in
/// to immediately walk back out. Cascades: when a leaf is dropped, its sole
/// neighbor may itself become a leaf and be dropped too.
///
/// Runs after `prune_unreachable_nodes`, so every kept node is already
/// connected to a stop and degree-1 removal cannot disconnect the graph.
/// Remaps `stop_to_node` like `prune_unreachable_nodes`.
pub fn prune_leaf_nodes(graph: &mut OsmGraph, stop_to_node: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    let n = graph.nodes.len();
    if n == 0 {
        return stop_to_node;
    }

    let stop_nodes: HashSet<u32> = stop_to_node.iter().map(|&(_, node)| node).collect();
    let adj = build_adj(graph);
    let mut degree: Vec<u32> = adj.iter().map(|a| a.len() as u32).collect();
    let mut keep = vec![true; n];

    let mut worklist: Vec<u32> = (0..n as u32)
        .filter(|&u| degree[u as usize] == 1 && !stop_nodes.contains(&u))
        .collect();

    let mut removed = 0u32;
    while let Some(u) = worklist.pop() {
        if !keep[u as usize] || degree[u as usize] != 1 {
            continue;
        }
        let neighbor = adj[u as usize].iter().copied().find(|&v| keep[v as usize]);
        keep[u as usize] = false;
        removed += 1;
        if let Some(v) = neighbor {
            degree[v as usize] -= 1;
            if degree[v as usize] == 1 && !stop_nodes.contains(&v) {
                worklist.push(v);
            }
        }
    }

    if removed == 0 {
        eprintln!("No leaf nodes to prune ({} nodes)", n);
        return stop_to_node;
    }

    let remap = retain_nodes(graph, &keep);
    eprintln!(
        "Pruned {} leaf nodes ({} kept, {:.1}% reduction)",
        removed,
        n - removed as usize,
        removed as f64 / n as f64 * 100.0
    );

    stop_to_node
        .into_iter()
        .map(|(stop, node)| (stop, remap[node as usize]))
        .collect()
}

/// Contract maximal chains of degree-2, non-stop nodes into single edges.
///
/// A chain `A — N1 — N2 — ... — Nk — B` (where every internal Ni has exactly
/// two graph-edges and is not a transit stop) is replaced by one edge `A — B`
/// whose `distance_meters` is the sum of the chain. This is **distance-perfect
/// for routing** — any shortest path that traversed the chain had no choice
/// but to walk every node in order, so summing the segment lengths preserves
/// the cost exactly. The only loss is geometric: visualizing a walk leg now
/// straight-lines across the kinks the chain encoded.
///
/// Subtleties:
///   - Self-loop chains (chain returns to its own start anchor) carry no
///     useful information and are dropped entirely.
///   - If two chains connect the same pair `(A, B)` — or if a direct edge
///     and a chain both connect them — only the shortest is kept, since
///     routing will only ever use the shortest one.
///   - The dedup in the previous bullet can leave a previously-deg-3 anchor
///     with two distinct neighbors and degree 2, which would itself be
///     collapsible. Iterating until stable handles this; in practice 1–2
///     passes suffice.
///
/// Runs after `prune_leaf_nodes`, so chain endpoints are guaranteed to be
/// either real intersections (deg≥3) or transit stops. Remaps `stop_to_node`.
pub fn collapse_degree2_nodes(
    graph: &mut OsmGraph,
    mut stop_to_node: Vec<(u32, u32)>,
) -> Vec<(u32, u32)> {
    let initial_nodes = graph.nodes.len();
    let initial_edges = graph.edges.len();
    if initial_nodes == 0 {
        return stop_to_node;
    }

    let mut iterations = 0u32;
    loop {
        let stop_nodes: HashSet<u32> = stop_to_node.iter().map(|&(_, n)| n).collect();
        let n = graph.nodes.len();
        let m = graph.edges.len();

        // Adjacency with edge indices (so we can sum exact distances along chains).
        let mut adj: Vec<Vec<(u32, u32)>> = vec![Vec::new(); n];
        for (ei, e) in graph.edges.iter().enumerate() {
            adj[e.u as usize].push((e.v, ei as u32));
            adj[e.v as usize].push((e.u, ei as u32));
        }

        let is_anchor = |u: u32, adj: &[Vec<(u32, u32)>]| -> bool {
            adj[u as usize].len() != 2 || stop_nodes.contains(&u)
        };

        let mut keep = vec![true; n];
        let mut walked = vec![false; m];
        // Dedup: keep shortest distance per unordered (u, v) pair.
        let mut new_edges: HashMap<(u32, u32), f32> = HashMap::new();

        for start in 0..n as u32 {
            if !is_anchor(start, &adj) {
                continue;
            }
            // Snapshot neighbors: walking may not mutate adj, but the borrow checker
            // is happier with a copy of this small list.
            let nbrs = adj[start as usize].clone();
            for (n0, e0) in nbrs {
                if walked[e0 as usize] {
                    continue;
                }
                walked[e0 as usize] = true;

                let mut total = graph.edges[e0 as usize].distance_meters as f64;
                let mut curr = n0;

                while !is_anchor(curr, &adj) {
                    keep[curr as usize] = false;
                    // Deg-2 non-stop: exactly two incident edges, one walked, one not.
                    let next = adj[curr as usize]
                        .iter()
                        .find(|&&(_, e)| !walked[e as usize])
                        .copied();
                    let Some((nxt, ei)) = next else {
                        // Both edges already walked: chain bit itself, treat as self-loop.
                        break;
                    };
                    walked[ei as usize] = true;
                    total += graph.edges[ei as usize].distance_meters as f64;
                    curr = nxt;
                }

                let end = curr;
                if end == start {
                    // Self-loop chain — drop the entire chain (no routing value).
                    continue;
                }
                let key = if start < end {
                    (start, end)
                } else {
                    (end, start)
                };
                let d = total as f32;
                new_edges
                    .entry(key)
                    .and_modify(|prev| {
                        if d < *prev {
                            *prev = d;
                        }
                    })
                    .or_insert(d);
            }
        }

        let removed = keep.iter().filter(|&&k| !k).count();
        if removed == 0 {
            break;
        }

        graph.edges = new_edges
            .into_iter()
            .map(|((u, v), d)| OsmEdge {
                u,
                v,
                distance_meters: d,
            })
            .collect();
        let remap = retain_nodes(graph, &keep);
        stop_to_node = stop_to_node
            .into_iter()
            .map(|(s, node)| (s, remap[node as usize]))
            .collect();
        iterations += 1;
    }

    let final_nodes = graph.nodes.len();
    let final_edges = graph.edges.len();
    if iterations == 0 {
        eprintln!("No deg-2 nodes to collapse ({} nodes)", initial_nodes);
    } else {
        eprintln!(
            "Collapsed {} deg-2 nodes in {} pass{} ({} -> {} nodes, {} -> {} edges, {:.1}% node reduction)",
            initial_nodes - final_nodes,
            iterations,
            if iterations == 1 { "" } else { "es" },
            initial_nodes,
            final_nodes,
            initial_edges,
            final_edges,
            (initial_nodes - final_nodes) as f64 / initial_nodes as f64 * 100.0,
        );
    }

    stop_to_node
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
