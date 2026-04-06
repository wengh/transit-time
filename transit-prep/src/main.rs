mod binary;
mod download;
mod graph;
mod gtfs;
mod osm;

use anyhow::{Context, Result};
use clap::Parser;
use jsonc_parser::parse_to_serde_value;
use std::collections::HashMap;
use std::io::Write;
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
}

#[derive(serde::Deserialize)]
struct CityConfig {
    id: String,
    feed_ids: Vec<String>,
    bbox: String,
    bbbike_name: Option<String>,
    osm_url: Option<String>,
}

/// Days since Unix epoch (no external deps).
fn unix_days_now() -> u32 {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400) as u32
}

/// YYYYMMDD → days since Unix epoch (Hinnant's civil_from_days inverse).
fn yyyymmdd_to_days(date: u32) -> u32 {
    let y = (date / 10000) as i64;
    let m = (date / 100 % 100) as u32;
    let d = (date % 100) as u32;
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let m0 = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m0 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe as i64 - 719468) as u32
}

/// Warn if the last service date in `data` is more than 1 day before today.
fn warn_if_expired(feed_id: &str, data: &gtfs::GtfsData) {
    let last = data
        .services
        .iter()
        .flat_map(|s| {
            s.added_dates.iter().copied().chain(if s.end_date != 0 {
                Some(s.end_date)
            } else {
                None
            })
        })
        .max();
    if let Some(last_date) = last {
        let today = unix_days_now();
        let last_days = yyyymmdd_to_days(last_date);
        if last_days + 1 < today {
            eprintln!(
                "WARNING: feed '{}' last service date is {} — {} day(s) ago",
                feed_id,
                last_date,
                today - last_days,
            );
        }
    }
}

fn fetch_gtfs_url(url: &str, cache_dir: &Path) -> Result<PathBuf> {
    let hash = url.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        h.wrapping_mul(0x100000001b3) ^ b as u64
    });
    let cache_path = cache_dir.join(format!("url_{:016x}.gtfs.zip", hash));
    if cache_path.exists() {
        eprintln!("Using cached GTFS: {:?}", cache_path);
        return Ok(cache_path);
    }
    download::with_download_lock(&cache_path, |path| {
        if path.exists() {
            eprintln!(
                "Using cached GTFS (downloaded by parallel process): {:?}",
                path
            );
            return Ok(path.to_path_buf());
        }
        eprintln!("Downloading GTFS from: {}", url);
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .user_agent("Mozilla/5.0 (compatible; transit-prep/1.0)")
            .build()?;
        let bytes = client.get(url).send()?.error_for_status()?.bytes()?;
        let tmp = path.with_extension("zip.tmp");
        std::fs::File::create(&tmp)?.write_all(&bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(path.to_path_buf())
    })
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

    run_prep(
        &city.id,
        &city.feed_ids,
        city.bbbike_name.as_deref(),
        city.osm_url.as_deref(),
        bbox,
        &cli.output,
        &cli.cache_dir,
    )
}

pub fn run_prep(
    city: &str,
    feed_ids: &[String],
    bbbike_name: Option<&str>,
    osm_url: Option<&str>,
    bbox: (f64, f64, f64, f64),
    output: &Path,
    cache_dir: &Path,
) -> Result<()> {
    eprintln!("=== Transit Prep for '{}' ===", city);
    eprintln!("Bounding box: {:?}", bbox);

    // Step 1: Download GTFS data
    eprintln!("\n--- Fetching GTFS data ---");

    let mut merged: Option<gtfs::GtfsData> = None;
    for fid in feed_ids {
        let path = fetch_gtfs_url(fid, cache_dir)?;
        eprintln!("GTFS feed {} cached at: {:?}", fid, path);
        let data = gtfs::parse_gtfs(&path)?;
        eprintln!(
            "  {} stops, {} routes, {} trips",
            data.stops.len(),
            data.routes.len(),
            data.trips.len()
        );
        warn_if_expired(fid, &data);
        match merged {
            Some(ref mut m) => m.merge(data),
            None => merged = Some(data),
        }
    }
    let mut gtfs_data = merged.unwrap();

    // Step 2: Download OSM data
    eprintln!("\n--- Fetching OSM pedestrian data ---");
    let osm_path = osm::fetch_osm(bbox, cache_dir, city, bbbike_name, osm_url)?;
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
