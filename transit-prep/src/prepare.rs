//! End-to-end build: GTFS + OSM → `city.bin`.
//!
//! [`prepare`] is the library entry point. It expects local file paths only —
//! downloading feeds and OSM extracts is the responsibility of an external
//! orchestrator (see the `city-builder` crate).

use anyhow::Result;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

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

    let mut parsed = parsed.into_iter().enumerate();
    let Some((_, mut gtfs_data)) = parsed.next() else {
        anyhow::bail!("no GTFS feeds given for '{city_id}'");
    };
    for (ordinal, data) in parsed {
        gtfs_data.merge(data, ordinal);
    }

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
    // A pattern with no events and no frequency entries (every service in
    // its group only has trips outside the bbox, or none at all) still costs
    // two `(num_stops + 1)` u32 offset arrays in the browser: Amsterdam had
    // 1745 of 3310 patterns empty, 125 MB of offsets; Berlin 254 MB. And
    // `Index::new` scans `num_stops` per active pattern per query.
    let built = patterns.len();
    patterns.retain(|p| !p.events.is_empty() || !p.frequency_routes.is_empty());
    for (i, p) in patterns.iter_mut().enumerate() {
        p.pattern_id = i as u32;
    }
    eprintln!(
        "Built {} service patterns ({} empty dropped)",
        patterns.len(),
        built - patterns.len()
    );

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
    let leg_shapes = build_leg_shapes(&gtfs_data, &route_remap, (min_lat, max_lat));

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
    lat_range: (f64, f64),
) -> Vec<binary::LegShape> {
    use rayon::prelude::*;

    let route_id_to_old_idx: HashMap<&str, u32> = gtfs_data
        .routes
        .iter()
        .map(|r| (r.id.as_str(), r.index))
        .collect();

    // The DP result depends only on the shape and the stop sequence, so run
    // it once per distinct `(shape_id, stops)` and reuse the legs for every
    // trip (and route) with that key. GO Transit has 146,514 trips over 460
    // shapes; per-trip matching did the same work hundreds of times over.
    struct Group {
        routes: BTreeSet<u32>, // new route indices
        trips: usize,
    }
    type GroupKey<'a> = (&'a str, Vec<u32>);
    let mut groups: HashMap<GroupKey, Group> = HashMap::new();
    let mut trips_with_shape = 0usize;
    for (trip_idx, trip) in gtfs_data.trips.iter().enumerate() {
        let Some(shape_id) = trip.shape_id.as_deref() else {
            continue;
        };
        if gtfs_data
            .shapes
            .get(shape_id)
            .is_none_or(|pts| pts.len() < 2)
        {
            continue;
        }
        let times = gtfs::trip_stop_times(&gtfs_data.stop_times, trip_idx as u32);
        if times.len() < 2 {
            continue;
        }
        let Some(new_route_idx) = route_id_to_old_idx
            .get(trip.route_id.as_str())
            .and_then(|old| route_remap.get(old))
        else {
            continue;
        };
        trips_with_shape += 1;
        let stops: Vec<u32> = times.iter().map(|st| st.stop_index).collect();
        let group = groups.entry((shape_id, stops)).or_insert_with(|| Group {
            routes: BTreeSet::new(),
            trips: 0,
        });
        group.routes.insert(*new_route_idx);
        group.trips += 1;
    }
    let groups: Vec<(GroupKey, Group)> = groups.into_iter().collect();

    let (min_lat, max_lat) = lat_range;
    let center_lat = (min_lat + max_lat) / 2.0;
    let cos_lat = center_lat.to_radians().cos();

    type LegEntry = (f64, Vec<(f64, f64)>);
    type LegMap = HashMap<(u32, u32, u32), LegEntry>;
    type KeyedLegs = Vec<((u32, u32, u32), LegEntry)>;

    /// Keep the better-quality (lower max projection distance) leg per key.
    fn insert_best(map: &mut LegMap, key: (u32, u32, u32), entry: LegEntry) {
        use std::collections::hash_map::Entry;
        match map.entry(key) {
            Entry::Occupied(mut o) => {
                // Strictly better quality wins; on a tie the lexicographically
                // smaller polyline does, so the parallel fold/reduce order
                // cannot change the output.
                let cur = o.get();
                if entry.0 < cur.0 || (entry.0 == cur.0 && entry.1 < cur.1) {
                    o.insert(entry);
                }
            }
            Entry::Vacant(v) => {
                v.insert(entry);
            }
        }
    }

    // (trips matched, legs) per group
    let group_results: Vec<(usize, KeyedLegs)> = groups
        .par_iter()
        .map(|((shape_id, stops), group)| {
            let shape = &gtfs_data.shapes[*shape_id];
            let stop_coords: Vec<(f64, f64)> = stops
                .iter()
                .map(|&s| {
                    let stop = &gtfs_data.stops[s as usize];
                    (stop.lat, stop.lon)
                })
                .collect();
            let Some(shape_matches) = match_stops_to_shape(&stop_coords, shape, cos_lat) else {
                return (0, Vec::new());
            };

            let mut legs: Vec<((u32, u32), LegEntry)> = Vec::with_capacity(stops.len() - 1);
            for w in 0..stops.len() - 1 {
                let (from_stop, to_stop) = (stops[w], stops[w + 1]);
                if from_stop == to_stop {
                    continue;
                }
                let mf = shape_matches[w];
                let mt = shape_matches[w + 1];
                let quality = mf.dist_sq.max(mt.dist_sq);

                let forward = (mf.seg_idx, mf.t) <= (mt.seg_idx, mt.t);
                let span = mf.seg_idx.abs_diff(mt.seg_idx);
                let mut leg_points = Vec::with_capacity(span + 2);
                leg_points.push(mf.proj);
                if forward {
                    if mf.seg_idx < mt.seg_idx {
                        leg_points.extend_from_slice(&shape[mf.seg_idx + 1..=mt.seg_idx]);
                    }
                } else if mt.seg_idx < mf.seg_idx {
                    leg_points.extend(shape[mt.seg_idx + 1..=mf.seg_idx].iter().rev().copied());
                }
                leg_points.push(mt.proj);
                legs.push(((from_stop, to_stop), (quality, leg_points)));
            }

            // Expand to every route that uses this key; only the last route
            // takes the points by move.
            let mut out = Vec::with_capacity(legs.len() * group.routes.len());
            let last = group.routes.len().saturating_sub(1);
            for (ri, &route) in group.routes.iter().enumerate() {
                if ri == last {
                    out.extend(legs.drain(..).map(|((f, t), e)| ((route, f, t), e)));
                } else {
                    out.extend(legs.iter().map(|((f, t), e)| ((route, *f, *t), e.clone())));
                }
            }
            (group.trips, out)
        })
        .collect();

    let trips_matched: usize = group_results.iter().map(|(n, _)| n).sum();

    let best_legs: LegMap = group_results
        .into_par_iter()
        .map(|(_, legs)| legs)
        .fold(LegMap::new, |mut acc, legs| {
            for (key, entry) in legs {
                insert_best(&mut acc, key, entry);
            }
            acc
        })
        .reduce(LegMap::new, |mut a, b| {
            for (key, entry) in b {
                insert_best(&mut a, key, entry);
            }
            a
        });

    eprintln!(
        "  {} trips with shapes, {} matched successfully ({} distinct shape/stop sequences), {} leg shapes",
        trips_with_shape,
        trips_matched,
        groups.len(),
        best_legs.len()
    );

    let mut leg_shapes: Vec<binary::LegShape> = best_legs
        .into_iter()
        .map(|(k, (_, pts))| (k, pts))
        .collect();
    leg_shapes.sort_by_key(|&(k, _)| k);
    leg_shapes
}
