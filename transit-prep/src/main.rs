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
    /// City name (used for GTFS feed lookup and output naming)
    #[arg(long)]
    city: String,

    /// MDB Feed ID (optional, bypasses city search)
    #[arg(long)]
    feed_id: Option<String>,

    /// Bounding box: min_lon,min_lat,max_lon,max_lat
    #[arg(long)]
    bbox: String,

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
    let bbox = parse_bbox(&cli.bbox)?;

    std::fs::create_dir_all(&cli.cache_dir)?;

    // Read MDB refresh token
    let refresh_token = std::fs::read_to_string(&cli.token_file)
        .context("Failed to read MDB refresh token file")?
        .trim()
        .to_string();

    run_prep(
        &cli.city,
        cli.feed_id.as_deref(),
        bbox,
        &cli.output,
        &cli.cache_dir,
        &refresh_token,
    )
}

pub fn run_prep(
    city: &str,
    feed_id: Option<&str>,
    bbox: (f64, f64, f64, f64),
    output: &Path,
    cache_dir: &Path,
    refresh_token: &str,
) -> Result<()> {
    eprintln!("=== Transit Prep for '{}' ===", city);
    eprintln!("Bounding box: {:?}", bbox);

    // Step 1: Download GTFS data
    eprintln!("\n--- Fetching GTFS data ---");
    let gtfs_path = mdb::fetch_gtfs(city, feed_id, cache_dir, refresh_token)?;
    eprintln!("GTFS data cached at: {:?}", gtfs_path);

    // Step 2: Download OSM data
    eprintln!("\n--- Fetching OSM pedestrian data ---");
    let osm_path = osm::fetch_osm(bbox, cache_dir, city)?;
    eprintln!("OSM data cached at: {:?}", osm_path);

    // Step 3: Parse GTFS
    eprintln!("\n--- Parsing GTFS ---");
    let gtfs_data = gtfs::parse_gtfs(&gtfs_path)?;
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
    let patterns = gtfs::build_service_patterns(&gtfs_data);
    eprintln!("Built {} service patterns", patterns.len());

    // Build route_index -> shape_id mapping (pick first shape per route from trips)
    let mut route_id_to_index: HashMap<String, u32> = HashMap::new();
    for route in &gtfs_data.routes {
        route_id_to_index.insert(route.id.clone(), route.index);
    }
    let mut route_shapes: Vec<String> = vec![String::new(); gtfs_data.routes.len()];
    for trip in &gtfs_data.trips {
        if let Some(ref shape_id) = trip.shape_id {
            if let Some(&ridx) = route_id_to_index.get(&trip.route_id) {
                if route_shapes[ridx as usize].is_empty() {
                    route_shapes[ridx as usize] = shape_id.clone();
                }
            }
        }
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
        route_names: gtfs_data.routes.into_iter().map(|r| r.short_name).collect(),
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
