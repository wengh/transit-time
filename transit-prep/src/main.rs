mod binary;
mod graph;
mod gtfs;
mod mdb;
mod osm;

use anyhow::{Context, Result};
use clap::Parser;
use jsonc_parser::parse_to_serde_value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "transit-prep")]
#[command(about = "Download and preprocess transit data for a city")]
struct Cli {
    /// Path to city JSON file (e.g. cities/chicago.jsonc)
    #[arg(long)]
    city_file: PathBuf,

    /// Output binary file path
    #[arg(long, default_value = "city.bin")]
    output: PathBuf,

    /// Cache directory
    #[arg(long, default_value = "cache")]
    cache_dir: PathBuf,

    /// MDB refresh token file path
    #[arg(long, default_value = ".mdb_refresh_token")]
    token_file: PathBuf,
}

#[derive(serde::Deserialize)]
struct CityConfig {
    id: String,
    feed_ids: Vec<String>,
    bbox: String,
    bbbike_name: Option<String>,
}

fn parse_bbox(s: &str) -> Result<(f64, f64, f64, f64)> {
    let parts: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        parts.len() == 4,
        "bbox must have 4 values: min_lon,min_lat,max_lon,max_lat"
    );
    Ok((parts[0], parts[1], parts[2], parts[3]))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let city_json = std::fs::read_to_string(&cli.city_file)
        .with_context(|| format!("Failed to read city file: {:?}", cli.city_file))?;
    let city: CityConfig = parse_to_serde_value(&city_json, &Default::default())
        .with_context(|| format!("Failed to parse city file: {:?}", cli.city_file))?;

    anyhow::ensure!(
        !city.feed_ids.is_empty(),
        "feed_ids must not be empty in {:?}",
        cli.city_file
    );

    let bbox = parse_bbox(&city.bbox)?;

    std::fs::create_dir_all(&cli.cache_dir)?;

    // Only load the MDB token if at least one feed uses MDB (not a direct URL)
    let needs_mdb = city
        .feed_ids
        .iter()
        .any(|id| !id.starts_with("http://") && !id.starts_with("https://"));
    let refresh_token = if needs_mdb {
        std::fs::read_to_string(&cli.token_file)
            .context("Failed to read MDB refresh token file")?
            .trim()
            .to_string()
    } else {
        String::new()
    };

    run_prep(
        &city.id,
        &city.feed_ids,
        city.bbbike_name.as_deref(),
        bbox,
        &cli.output,
        &cli.cache_dir,
        &refresh_token,
    )
}

pub fn run_prep(
    city: &str,
    feed_ids: &[String],
    bbbike_name: Option<&str>,
    bbox: (f64, f64, f64, f64),
    output: &Path,
    cache_dir: &Path,
    refresh_token: &str,
) -> Result<()> {
    eprintln!("=== Transit Prep for '{}' ===", city);
    eprintln!("Bounding box: {:?}", bbox);

    // Step 1: Download GTFS data
    eprintln!("\n--- Fetching GTFS data ---");

    let mut merged: Option<gtfs::GtfsData> = None;
    for fid in feed_ids {
        let path = if fid.starts_with("http://") || fid.starts_with("https://") {
            mdb::fetch_gtfs_url(fid, cache_dir)?
        } else {
            mdb::fetch_gtfs(fid, cache_dir, refresh_token)?
        };
        eprintln!("GTFS feed {} cached at: {:?}", fid, path);
        let data = gtfs::parse_gtfs(&path)?;
        eprintln!(
            "  {} stops, {} routes, {} trips",
            data.stops.len(),
            data.routes.len(),
            data.trips.len()
        );
        match merged {
            Some(ref mut m) => m.merge(data),
            None => merged = Some(data),
        }
    }
    let mut gtfs_data = merged.unwrap();

    // Step 2: Download OSM data
    eprintln!("\n--- Fetching OSM pedestrian data ---");
    let osm_path = osm::fetch_osm(bbox, cache_dir, city, bbbike_name)?;
    eprintln!("OSM data cached at: {:?}", osm_path);

    // Step 3: Parse GTFS
    eprintln!("\n--- Parsing GTFS ---");
    eprintln!(
        "Parsed {} stops, {} routes, {} trips, {} stop_times, {} services",
        gtfs_data.stops.len(),
        gtfs_data.routes.len(),
        gtfs_data.trips.len(),
        gtfs_data.stop_times.len(),
        gtfs_data.services.len(),
    );

    // Filter stops to bbox and re-index sequentially so out-of-bbox stops occupy no ids
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    gtfs_data
        .stops
        .retain(|s| s.lat >= min_lat && s.lat <= max_lat && s.lon >= min_lon && s.lon <= max_lon);
    for (i, stop) in gtfs_data.stops.iter_mut().enumerate() {
        stop.index = i as u32;
    }
    eprintln!("  {} stops within bbox", gtfs_data.stops.len());

    // Step 4: Build OSM graph
    eprintln!("\n--- Building OSM graph ---");
    let mut osm_graph = graph::build_graph(&osm_path, bbox)?;
    eprintln!(
        "Graph: {} nodes, {} edges",
        osm_graph.nodes.len(),
        osm_graph.edges.len(),
    );

    // Step 5: Snap stops to OSM edges (inserting virtual nodes)
    eprintln!("\n--- Snapping stops to OSM edges ---");
    let stop_to_node = graph::snap_stops_to_nodes(&gtfs_data.stops, &mut osm_graph);
    eprintln!("Snapped {} stops", stop_to_node.len());

    // Step 6: Build service patterns and event arrays
    eprintln!("\n--- Building service patterns ---");
    let mut patterns = gtfs::build_service_patterns(&gtfs_data);
    eprintln!("Built {} service patterns", patterns.len());

    // Compact route ids: collect used route indices, remap to 0..N, drop unused routes
    let mut used_route_indices: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for pattern in &patterns {
        for second_events in &pattern.events {
            for event in second_events {
                used_route_indices.insert(event.route_index);
            }
        }
        for freq in &pattern.frequency_routes {
            used_route_indices.insert(freq.route_index);
        }
    }
    let route_remap: HashMap<u32, u32> = used_route_indices
        .iter()
        .enumerate()
        .map(|(new_idx, &old_idx)| (old_idx, new_idx as u32))
        .collect();
    for pattern in &mut patterns {
        for second_events in &mut pattern.events {
            for event in second_events {
                event.route_index = route_remap[&event.route_index];
            }
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

    // Build compacted route arrays and route_shapes in new-index order
    let mut route_names: Vec<String> = Vec::new();
    let mut route_colors: Vec<Option<gtfs::Color>> = Vec::new();
    let mut route_shapes: Vec<Vec<String>> = Vec::new();
    // Build route_id -> shape_ids from trips (only once, before consuming routes)
    let mut route_id_to_shapes: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
    for trip in &gtfs_data.trips {
        if let Some(ref shape_id) = trip.shape_id {
            route_id_to_shapes
                .entry(&trip.route_id)
                .or_default()
                .insert(shape_id.as_str());
        }
    }
    for &old_idx in &used_route_indices {
        let route = &gtfs_data.routes[old_idx as usize];
        route_names.push(route.short_name.clone());
        route_colors.push(route.color);
        let shapes = route_id_to_shapes
            .get(route.id.as_str())
            .map(|s| s.iter().map(|&id| id.to_string()).collect())
            .unwrap_or_default();
        route_shapes.push(shapes);
    }

    // Step 7: Serialize to binary
    eprintln!("\n--- Writing binary output ---");
    let prepared = binary::PreparedData {
        nodes: osm_graph.nodes,
        edges: osm_graph.edges,
        stops: gtfs_data.stops,
        stop_to_node,
        patterns,
        shapes: gtfs_data.shapes,
        route_names,
        route_colors,
        route_shapes,
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
