mod binary;
mod graph;
mod gtfs;
mod mdb;
mod osm;

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "transit-prep")]
#[command(about = "Download and preprocess transit data for a city")]
struct Cli {
    /// Path to city JSON file (e.g. cities/chicago.json)
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
    let city: CityConfig = serde_json::from_str(&city_json)
        .with_context(|| format!("Failed to parse city file: {:?}", cli.city_file))?;

    anyhow::ensure!(!city.feed_ids.is_empty(), "feed_ids must not be empty in {:?}", cli.city_file);

    let bbox = parse_bbox(&city.bbox)?;

    std::fs::create_dir_all(&cli.cache_dir)?;

    let refresh_token = std::fs::read_to_string(&cli.token_file)
        .context("Failed to read MDB refresh token file")?
        .trim()
        .to_string();

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
        let path = mdb::fetch_gtfs(fid, cache_dir, refresh_token)?;
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
    let gtfs_data = merged.unwrap();

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
    let patterns = gtfs::build_service_patterns(&gtfs_data, bbox);
    eprintln!("Built {} service patterns", patterns.len());

    // Build route_index -> shape_id mapping
    let mut route_id_to_index: HashMap<String, u32> = HashMap::new();
    for route in &gtfs_data.routes {
        route_id_to_index.insert(route.id.clone(), route.index);
    }
    let mut route_shapes_set: Vec<std::collections::HashSet<String>> = vec![std::collections::HashSet::new(); gtfs_data.routes.len()];
    for trip in &gtfs_data.trips {
        if let Some(ref shape_id) = trip.shape_id {
            if let Some(&ridx) = route_id_to_index.get(&trip.route_id) {
                route_shapes_set[ridx as usize].insert(shape_id.clone());
            }
        }
    }
    let route_shapes: Vec<Vec<String>> = route_shapes_set.into_iter()
        .map(|s| s.into_iter().collect())
        .collect();

    // Step 7: Serialize to binary
    eprintln!("\n--- Writing binary output ---");
    let prepared = binary::PreparedData {
        nodes: osm_graph.nodes,
        edges: osm_graph.edges,
        stops: gtfs_data.stops,
        stop_to_node,
        patterns,
        shapes: gtfs_data.shapes,
        route_names: gtfs_data.routes.iter().map(|r| r.short_name.clone()).collect(),
        route_colors: gtfs_data.routes.into_iter().map(|r| r.color).collect(),
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
