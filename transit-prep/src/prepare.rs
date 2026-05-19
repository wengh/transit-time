//! End-to-end build: GTFS + OSM → `city.bin`.
//!
//! [`prepare`] is the library entry point. It expects local file paths only —
//! downloading feeds and OSM extracts is the responsibility of an external
//! orchestrator (see the `city-builder` crate).

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use std::collections::BTreeSet;

use crate::shape_match::match_stops_to_shape;
use crate::stale::{apply_stale_policy, unix_days_now, warn_if_expired};
use crate::{binary, graph, gtfs};

fn build_remap(used: &BTreeSet<u32>) -> HashMap<u32, u32> {
    used.iter()
        .enumerate()
        .map(|(new_idx, &old_idx)| (old_idx, new_idx as u32))
        .collect()
}

/// Build a `city.bin` from a city's GTFS feeds and OSM extract.
///
/// * `city_id` — identifier used in log lines.
/// * `gtfs_paths` — one or more GTFS `.zip` files; merged in the given order.
/// * `osm_path` — OSM PBF or XML extract covering `bbox`.
/// * `bbox` — `(min_lon, min_lat, max_lon, max_lat)`. Stops outside the box
///   are dropped.
/// * `output` — destination `.bin` path.
/// * `allow_stale` — `Some(true)` forces unbounded service windows;
///   `Some(false)` disables the policy; `None` applies the default heuristic
///   in [`crate::stale::apply_stale_policy`].
pub fn prepare(
    city_id: &str,
    gtfs_paths: &[PathBuf],
    osm_path: &Path,
    bbox: (f64, f64, f64, f64),
    output: &Path,
    allow_stale: Option<bool>,
) -> Result<()> {
    eprintln!("=== Transit Prep for '{}' ===", city_id);
    eprintln!("Bounding box: {:?}", bbox);

    // Per-feed parse is parallel; the merge has to be sequential because feed
    // index prefixes are derived from `self.stops.len()` at merge time.
    eprintln!("\n--- Parsing GTFS data ---");

    use rayon::prelude::*;

    let today_days = unix_days_now();
    let parsed: Vec<gtfs::GtfsData> = gtfs_paths
        .par_iter()
        .map(|path| -> Result<gtfs::GtfsData> {
            let mut data = gtfs::parse_gtfs(path, bbox)?;
            eprintln!(
                "  {:?}: {} stops, {} routes, {} trips",
                path.file_name().unwrap_or_default(),
                data.stops.len(),
                data.routes.len(),
                data.trips.len()
            );
            warn_if_expired(&path.to_string_lossy(), &data);
            apply_stale_policy(&mut data, allow_stale, today_days);
            Ok(data)
        })
        .collect::<Result<Vec<_>>>()?;

    let mut merged: Option<gtfs::GtfsData> = None;
    for data in parsed {
        match merged {
            Some(ref mut m) => m.merge(data),
            None => merged = Some(data),
        }
    }
    let mut gtfs_data = merged.unwrap();

    eprintln!("\n--- GTFS summary ---");
    eprintln!(
        "Parsed {} stops, {} routes, {} trips, {} stop_times, {} services",
        gtfs_data.stops.len(),
        gtfs_data.routes.len(),
        gtfs_data.trips.len(),
        gtfs_data.stop_times.len(),
        gtfs_data.services.len(),
    );

    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    gtfs_data
        .stops
        .retain(|s| s.lat >= min_lat && s.lat <= max_lat && s.lon >= min_lon && s.lon <= max_lon);
    let stop_index_remap: HashMap<u32, u32> = gtfs_data
        .stops
        .iter()
        .enumerate()
        .map(|(new_idx, stop)| (stop.index, new_idx as u32))
        .collect();
    for (i, stop) in gtfs_data.stops.iter_mut().enumerate() {
        stop.index = i as u32;
    }
    gtfs_data.stop_times.retain_mut(|st| {
        if let Some(&new_idx) = stop_index_remap.get(&st.stop_index) {
            st.stop_index = new_idx;
            true
        } else {
            false
        }
    });
    gtfs_data.stop_times.shrink_to_fit();
    eprintln!("  {} stops within bbox", gtfs_data.stops.len());

    let mut stops_per_trip: HashMap<u32, usize> = HashMap::new();
    for st in &gtfs_data.stop_times {
        *stops_per_trip.entry(st.trip_index).or_default() += 1;
    }
    let valid_trip_indices: HashSet<u32> = stops_per_trip
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|(idx, _)| idx)
        .collect();
    eprintln!(
        "  {} trips with ≥2 in-bbox stops (of {} total)",
        valid_trip_indices.len(),
        gtfs_data.trips.len()
    );

    eprintln!("\n--- Building OSM graph ---");
    let mut osm_graph = graph::build_graph(osm_path, bbox)?;
    eprintln!(
        "Graph: {} nodes, {} edges",
        osm_graph.nodes.len(),
        osm_graph.edges.len(),
    );

    eprintln!("\n--- Snapping stops to OSM edges ---");
    let stop_to_node = graph::snap_stops_to_nodes(&gtfs_data.stops, &mut osm_graph);
    eprintln!("Snapped {} stops", stop_to_node.len());
    let stop_to_node = graph::prune_unreachable_nodes(&mut osm_graph, stop_to_node);
    let stop_to_node = graph::prune_leaf_nodes(&mut osm_graph, stop_to_node);
    // Distance-perfect: degree-2 collapse loses only the kink geometry at
    // intermediate nodes, which walk-leg display already straight-lines over.
    let stop_to_node = graph::collapse_degree2_nodes(&mut osm_graph, stop_to_node);

    {
        let mapped_stops: std::collections::HashSet<u32> =
            stop_to_node.iter().map(|&(s, _)| s).collect();
        let before = gtfs_data.stop_times.len();
        gtfs_data
            .stop_times
            .retain(|st| mapped_stops.contains(&st.stop_index));
        let dropped = before - gtfs_data.stop_times.len();
        if dropped > 0 {
            eprintln!(
                "Dropped {} stop_times rows referencing {} unmapped stops",
                dropped,
                gtfs_data.stops.len() - mapped_stops.len(),
            );
        }
    }

    gtfs_data
        .stop_times
        .sort_unstable_by_key(|st| (st.trip_index, st.stop_sequence));
    eprintln!("\n--- Building service patterns ---");
    let mut patterns = gtfs::build_service_patterns(&gtfs_data);
    eprintln!("Built {} service patterns", patterns.len());

    let mut used_route_indices: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for pattern in &patterns {
        for (_, event) in &pattern.events {
            used_route_indices.insert(event.route_index);
        }
        for freq in &pattern.frequency_routes {
            used_route_indices.insert(freq.route_index);
        }
    }
    let route_remap = build_remap(&used_route_indices);
    for pattern in &mut patterns {
        for (_, event) in &mut pattern.events {
            event.route_index = route_remap[&event.route_index];
        }
        for freq in &mut pattern.frequency_routes {
            freq.route_index = route_remap[&freq.route_index];
        }
    }
    eprintln!(
        "  {} routes with events (of {} total)",
        used_route_indices.len(),
        gtfs_data.routes.len()
    );

    let mut used_stop_indices: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for pattern in &patterns {
        for (_, event) in &pattern.events {
            used_stop_indices.insert(event.stop_index);
            used_stop_indices.insert(event.next_stop_index);
        }
        for freq in &pattern.frequency_routes {
            used_stop_indices.insert(freq.stop_index);
            used_stop_indices.insert(freq.next_stop_index);
        }
    }
    let stop_remap = build_remap(&used_stop_indices);
    for pattern in &mut patterns {
        for (_, event) in &mut pattern.events {
            event.stop_index = stop_remap[&event.stop_index];
            event.next_stop_index = stop_remap[&event.next_stop_index];
        }
        for freq in &mut pattern.frequency_routes {
            freq.stop_index = stop_remap[&freq.stop_index];
            freq.next_stop_index = stop_remap[&freq.next_stop_index];
        }
    }
    let stop_to_node: Vec<(u32, u32)> = stop_to_node
        .into_iter()
        .filter_map(|(old_idx, node)| stop_remap.get(&old_idx).map(|&new_idx| (new_idx, node)))
        .collect();
    let total_stops = gtfs_data.stops.len();
    let compacted_stops: Vec<_> = used_stop_indices
        .iter()
        .enumerate()
        .map(|(new_idx, &old_idx)| {
            let mut stop = gtfs_data.stops[old_idx as usize].clone();
            stop.index = new_idx as u32;
            stop
        })
        .collect();
    gtfs_data.stops = compacted_stops;
    gtfs_data.stop_times.retain_mut(|st| {
        if let Some(&new_idx) = stop_remap.get(&st.stop_index) {
            st.stop_index = new_idx;
            true
        } else {
            false
        }
    });
    eprintln!(
        "  {} stops with events (of {} in bbox)",
        used_stop_indices.len(),
        total_stops
    );

    let mut route_names: Vec<String> = Vec::new();
    let mut route_colors: Vec<Option<gtfs::Color>> = Vec::new();
    for &old_idx in &used_route_indices {
        let route = &gtfs_data.routes[old_idx as usize];
        route_names.push(route.short_name.clone());
        route_colors.push(route.color);
    }

    eprintln!("\n--- Building leg shapes ---");
    let leg_shapes = build_leg_shapes(
        &gtfs_data,
        &route_remap,
        &valid_trip_indices,
        (min_lat, max_lat),
    );

    eprintln!("\n--- Writing binary output ---");
    let prepared = binary::PreparedData {
        nodes: osm_graph.nodes,
        edges: osm_graph.edges,
        stops: gtfs_data.stops,
        stop_to_node,
        patterns,
        route_names,
        route_colors,
        leg_shapes,
    };
    binary::write_binary(&prepared, output)?;
    let size = std::fs::metadata(output)?.len();
    eprintln!(
        "Wrote {} ({:.2} MB)",
        output.display(),
        size as f64 / 1_048_576.0
    );

    eprintln!("\n=== Done ===");
    Ok(())
}

fn build_leg_shapes(
    gtfs_data: &gtfs::GtfsData,
    route_remap: &HashMap<u32, u32>,
    valid_trip_indices: &HashSet<u32>,
    lat_range: (f64, f64),
) -> Vec<((u32, u32, u32), Vec<(f64, f64)>)> {
    use rayon::prelude::*;

    let route_id_to_old_idx: HashMap<&str, u32> = gtfs_data
        .routes
        .iter()
        .map(|r| (r.id.as_str(), r.index))
        .collect();

    let mut stop_times_by_trip: HashMap<u32, Vec<&gtfs::StopTime>> = HashMap::new();
    for st in &gtfs_data.stop_times {
        stop_times_by_trip
            .entry(st.trip_index)
            .or_default()
            .push(st);
    }
    for times in stop_times_by_trip.values_mut() {
        times.sort_by_key(|st| st.stop_sequence);
    }

    let (min_lat, max_lat) = lat_range;
    let center_lat = (min_lat + max_lat) / 2.0;
    let cos_lat = center_lat.to_radians().cos();

    struct TripShapeResult {
        had_shape: bool,
        legs: Option<Vec<((u32, u32, u32), (f64, Vec<(f64, f64)>))>>,
    }

    type LegMap = HashMap<(u32, u32, u32), (f64, Vec<(f64, f64)>)>;

    fn merge_leg_maps(mut a: LegMap, b: LegMap) -> LegMap {
        for (key, (quality, leg_points)) in b {
            match a.get(&key) {
                Some((best_q, _)) if quality >= *best_q => {}
                _ => {
                    a.insert(key, (quality, leg_points));
                }
            }
        }
        a
    }

    let trip_results: Vec<TripShapeResult> = gtfs_data
        .trips
        .par_iter()
        .enumerate()
        .map(|(trip_idx, trip)| {
            let trip_idx = trip_idx as u32;
            if !valid_trip_indices.contains(&trip_idx) {
                return TripShapeResult {
                    had_shape: false,
                    legs: None,
                };
            }
            let shape_id = match &trip.shape_id {
                Some(id) => id.as_str(),
                None => {
                    return TripShapeResult {
                        had_shape: false,
                        legs: None,
                    };
                }
            };
            let shape = match gtfs_data.shapes.get(shape_id) {
                Some(pts) if pts.len() >= 2 => pts,
                _ => {
                    return TripShapeResult {
                        had_shape: false,
                        legs: None,
                    };
                }
            };
            let times = match stop_times_by_trip.get(&trip_idx) {
                Some(t) if t.len() >= 2 => t,
                _ => {
                    return TripShapeResult {
                        had_shape: false,
                        legs: None,
                    };
                }
            };
            let old_route_idx = match route_id_to_old_idx.get(trip.route_id.as_str()) {
                Some(&idx) => idx,
                None => {
                    return TripShapeResult {
                        had_shape: false,
                        legs: None,
                    };
                }
            };
            let new_route_idx = match route_remap.get(&old_route_idx) {
                Some(&idx) => idx,
                None => {
                    return TripShapeResult {
                        had_shape: false,
                        legs: None,
                    };
                }
            };

            let stop_coords: Vec<(f64, f64)> = times
                .iter()
                .map(|st| {
                    let stop = &gtfs_data.stops[st.stop_index as usize];
                    (stop.lat, stop.lon)
                })
                .collect();

            let shape_matches = match match_stops_to_shape(&stop_coords, shape, cos_lat) {
                Some(m) => m,
                None => {
                    return TripShapeResult {
                        had_shape: true,
                        legs: None,
                    };
                }
            };

            let mut legs = Vec::new();
            for w in 0..times.len() - 1 {
                let from_stop = times[w].stop_index;
                let to_stop = times[w + 1].stop_index;
                let key = (new_route_idx, from_stop, to_stop);

                let mf = shape_matches[w];
                let mt = shape_matches[w + 1];

                let quality = mf.dist_sq.max(mt.dist_sq);

                let forward = (mf.seg_idx, mf.t) <= (mt.seg_idx, mt.t);
                let span = mf.seg_idx.abs_diff(mt.seg_idx);
                let mut leg_points = Vec::with_capacity(span + 2);
                leg_points.push(mf.proj);
                if forward {
                    if mf.seg_idx + 1 <= mt.seg_idx {
                        leg_points.extend_from_slice(&shape[mf.seg_idx + 1..=mt.seg_idx]);
                    }
                } else if mt.seg_idx + 1 <= mf.seg_idx {
                    leg_points.extend(shape[mt.seg_idx + 1..=mf.seg_idx].iter().rev().copied());
                }
                leg_points.push(mt.proj);

                legs.push((key, (quality, leg_points)));
            }
            TripShapeResult {
                had_shape: true,
                legs: Some(legs),
            }
        })
        .collect();

    let trips_with_shape = trip_results.iter().filter(|r| r.had_shape).count() as u32;
    let trips_matched = trip_results.iter().filter(|r| r.legs.is_some()).count() as u32;

    let best_legs: LegMap = trip_results
        .into_par_iter()
        .filter_map(|r| r.legs)
        .fold(LegMap::new, |mut acc, legs| {
            for (key, entry) in legs {
                acc.entry(key)
                    .and_modify(|(best_q, best_pts)| {
                        if entry.0 < *best_q {
                            *best_q = entry.0;
                            *best_pts = entry.1.clone();
                        }
                    })
                    .or_insert(entry);
            }
            acc
        })
        .reduce(LegMap::new, merge_leg_maps);

    eprintln!(
        "  {} trips with shapes, {} matched successfully, {} leg shapes",
        trips_with_shape,
        trips_matched,
        best_legs.len()
    );

    let mut leg_shapes: Vec<((u32, u32, u32), Vec<(f64, f64)>)> = best_legs
        .into_iter()
        .map(|(k, (_, pts))| (k, pts))
        .collect();
    leg_shapes.sort_by_key(|&(k, _)| k);
    leg_shapes
}
