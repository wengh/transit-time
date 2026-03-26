mod mdb;
mod osm;
mod gtfs;
mod graph;
mod binary;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "transit-prep")]
#[command(about = "Download and preprocess transit data for a city")]
struct Cli {
    /// City name (used for GTFS feed lookup)
    #[arg(long)]
    city: String,

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
    let parts: Vec<f64> = s.split(',').map(|p| p.trim().parse()).collect::<std::result::Result<Vec<_>, _>>()?;
    anyhow::ensure!(parts.len() == 4, "bbox must have 4 values: min_lon,min_lat,max_lon,max_lat");
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

    run_prep(&cli.city, bbox, &cli.output, &cli.cache_dir, &refresh_token)
}

pub fn run_prep(
    city: &str,
    bbox: (f64, f64, f64, f64),
    output: &Path,
    cache_dir: &Path,
    refresh_token: &str,
) -> Result<()> {
    eprintln!("=== Transit Prep for '{}' ===", city);
    eprintln!("Bounding box: {:?}", bbox);

    // Step 1: Download GTFS data
    eprintln!("\n--- Fetching GTFS data ---");
    let gtfs_path = mdb::fetch_gtfs(city, cache_dir, refresh_token)?;
    eprintln!("GTFS data cached at: {:?}", gtfs_path);

    // Step 2: Download OSM data
    eprintln!("\n--- Fetching OSM pedestrian data ---");
    let osm_path = osm::fetch_osm(bbox, cache_dir)?;
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
    let osm_graph = graph::build_graph(&osm_path)?;
    eprintln!(
        "Graph: {} nodes, {} edges",
        osm_graph.nodes.len(),
        osm_graph.edges.len(),
    );

    // Step 5: Snap stops to OSM nodes
    eprintln!("\n--- Snapping stops to OSM nodes ---");
    let stop_to_node = graph::snap_stops_to_nodes(&gtfs_data.stops, &osm_graph);
    eprintln!("Snapped {} stops", stop_to_node.len());

    // Step 6: Build service patterns and event arrays
    eprintln!("\n--- Building service patterns ---");
    let patterns = gtfs::build_service_patterns(&gtfs_data);
    eprintln!("Built {} service patterns", patterns.len());

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
    };
    binary::write_binary(&prepared, output)?;
    let size = std::fs::metadata(output)?.len();
    eprintln!("Wrote {} ({:.2} MB)", output.display(), size as f64 / 1_048_576.0);

    eprintln!("\n=== Done ===");
    Ok(())
}
