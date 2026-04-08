mod binary;
mod download;
mod graph;
mod gtfs;
mod osm;
mod transitland;

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
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Process a city config into binary transit data
    Prep {
        /// Path to city JSON file (e.g. cities/chicago.jsonc)
        #[arg(long)]
        city_file: PathBuf,

        /// Output binary file path
        #[arg(long, default_value = "city.bin")]
        output: PathBuf,

        /// Cache directory
        #[arg(long, default_value = "cache")]
        cache_dir: PathBuf,
    },
    /// Check if any Transitland feeds have newer versions upstream.
    /// Exits with code 0 if all feeds are up to date, 1 if any have changed.
    Check {
        /// Path to city JSON file
        #[arg(long)]
        city_file: PathBuf,

        /// Cache directory
        #[arg(long, default_value = "cache")]
        cache_dir: PathBuf,
    },
    /// Generate a city config file by querying Transitland for feeds in a geographic area
    Generate {
        /// Output JSONC file path (e.g. cities/portland.jsonc)
        #[arg(long)]
        output: PathBuf,

        /// City ID (used for naming)
        #[arg(long)]
        id: String,

        /// BBBike city name (e.g. "Portland")
        #[arg(long)]
        bbbike_name: Option<String>,

        /// Direct OSM PBF URL
        #[arg(long)]
        osm_url: Option<String>,

        /// Cache directory
        #[arg(long, default_value = "cache")]
        cache_dir: PathBuf,
    },
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

/// Extract Transitland onestop_id and api_key from a Transitland download URL, if it is one.
fn parse_transitland_url(url: &str) -> Option<(&str, &str)> {
    let prefix = "https://api.transit.land/api/v2/rest/feeds/";
    let suffix = "/download_latest_feed_version?apikey=";
    if let Some(rest) = url.strip_prefix(prefix) {
        if let Some(pos) = rest.find(suffix) {
            let onestop_id = &rest[..pos];
            let api_key = &rest[pos + suffix.len()..];
            return Some((onestop_id, api_key));
        }
    }
    None
}

/// FNV-1a hash of a URL.
fn url_hash(url: &str) -> u64 {
    url.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        h.wrapping_mul(0x100000001b3) ^ b as u64
    })
}

/// Cache path for the GTFS zip download.
/// Transitland feeds use the onestop ID as filename; direct URLs use a hash.
fn gtfs_cache_path(url: &str, cache_dir: &Path) -> PathBuf {
    if let Some((onestop_id, _)) = parse_transitland_url(url) {
        cache_dir.join(format!("{}.gtfs.zip", onestop_id))
    } else {
        cache_dir.join(format!("url_{:016x}.gtfs.zip", url_hash(url)))
    }
}

/// Cache path for the sha1 sidecar (stored in cache_dir/sha1/ so it can be
/// cached separately from the large download files in CI).
fn gtfs_sha1_path(url: &str, cache_dir: &Path) -> PathBuf {
    if let Some((onestop_id, _)) = parse_transitland_url(url) {
        cache_dir.join("sha1").join(format!("{}.sha1", onestop_id))
    } else {
        cache_dir
            .join("sha1")
            .join(format!("url_{:016x}.sha1", url_hash(url)))
    }
}

fn fetch_gtfs_url(url: &str, cache_dir: &Path) -> Result<PathBuf> {
    let cache_path = gtfs_cache_path(url, cache_dir);
    let sha1_path = gtfs_sha1_path(url, cache_dir);

    // If cached, check if Transitland has a newer version
    if cache_path.exists() {
        if let Some((onestop_id, api_key)) = parse_transitland_url(url) {
            let local_sha1 = std::fs::read_to_string(&sha1_path).unwrap_or_default();
            match transitland::latest_feed_sha1(api_key, onestop_id) {
                Ok(Some(remote_sha1)) if !local_sha1.is_empty() && local_sha1 == remote_sha1 => {
                    eprintln!("Using cached GTFS (up to date): {:?}", cache_path);
                    return Ok(cache_path);
                }
                Ok(Some(remote_sha1)) => {
                    eprintln!(
                        "Transitland feed '{}' has new version (sha1: {}), re-downloading...",
                        onestop_id,
                        &remote_sha1[..12.min(remote_sha1.len())]
                    );
                    // Fall through to download
                }
                Ok(None) => {
                    eprintln!(
                        "Using cached GTFS (no remote sha1 to compare): {:?}",
                        cache_path
                    );
                    return Ok(cache_path);
                }
                Err(e) => {
                    eprintln!("WARNING: could not check Transitland for updates: {}", e);
                    eprintln!("Using cached GTFS: {:?}", cache_path);
                    return Ok(cache_path);
                }
            }
        } else {
            eprintln!("Using cached GTFS: {:?}", cache_path);
            return Ok(cache_path);
        }
    }

    let tl_info = parse_transitland_url(url).map(|(id, key)| (id.to_string(), key.to_string()));

    download::with_download_lock(&cache_path, |path| {
        // Re-check after acquiring lock (parallel process may have downloaded)
        if path.exists() && tl_info.is_none() {
            eprintln!(
                "Using cached GTFS (downloaded by parallel process): {:?}",
                path
            );
            return Ok(path.to_path_buf());
        }
        eprintln!("Downloading GTFS from: {}", url);
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .user_agent("Mozilla/5.0 (compatible; transit-prep/1.0)")
            .build()?;
        let bytes = client.get(url).send()?.error_for_status()?.bytes()?;
        let tmp = path.with_extension("zip.tmp");
        std::fs::File::create(&tmp)?.write_all(&bytes)?;
        std::fs::rename(&tmp, path)?;

        // Save the sha1 for future staleness checks
        if let Some((onestop_id, api_key)) = &tl_info {
            match transitland::latest_feed_sha1(api_key, onestop_id) {
                Ok(Some(sha1)) => {
                    let _ = std::fs::write(&sha1_path, &sha1);
                }
                _ => {}
            }
        }

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

/// Resolve a feed ID to a download URL.
/// Direct URLs pass through; Transitland onestop IDs (starting with "f-") are resolved
/// to Transitland's download endpoint (which serves a hosted copy of the GTFS zip).
fn resolve_feed_id(feed_id: &str, api_key: Option<&str>) -> Result<String> {
    if feed_id.starts_with("http://") || feed_id.starts_with("https://") {
        return Ok(feed_id.to_string());
    }
    if feed_id.starts_with("f-") {
        let key = api_key.with_context(|| {
            format!(
                "Feed '{}' is a Transitland ID but TRANSITLAND_API_KEY is not set",
                feed_id
            )
        })?;
        let url = transitland::download_url(key, feed_id);
        eprintln!("Resolved '{}' -> Transitland download", feed_id);
        return Ok(url);
    }
    anyhow::bail!(
        "Unknown feed_id format: '{}' (expected URL or Transitland onestop ID starting with 'f-')",
        feed_id
    )
}

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();

    match cli.command {
        Commands::Prep {
            city_file,
            output,
            cache_dir,
        } => cmd_prep(&city_file, &output, &cache_dir),
        Commands::Check {
            city_file,
            cache_dir,
        } => {
            let stale = cmd_check(&city_file, &cache_dir)?;
            if stale {
                std::process::exit(1);
            }
            Ok(())
        }
        Commands::Generate {
            output,
            id,
            bbbike_name,
            osm_url,
            cache_dir,
        } => cmd_generate(
            &output,
            &id,
            bbbike_name.as_deref(),
            osm_url.as_deref(),
            &cache_dir,
        ),
    }
}

/// Check if any Transitland feeds have newer versions. Returns true if stale.
fn cmd_check(city_file: &Path, cache_dir: &Path) -> Result<bool> {
    let city_json = std::fs::read_to_string(city_file)
        .with_context(|| format!("Failed to read city file: {:?}", city_file))?;
    let city: CityConfig = parse_to_serde_value(&city_json, &Default::default())
        .with_context(|| format!("Failed to parse city file: {:?}", city_file))?;

    let api_key = transitland::get_api_key().ok();

    for feed_id in &city.feed_ids {
        let url = resolve_feed_id(feed_id, api_key.as_deref())?;
        let Some((onestop_id, api_key)) = parse_transitland_url(&url) else {
            continue; // direct URLs — no remote check available
        };

        let sha1_path = gtfs_sha1_path(&url, cache_dir);
        let local_sha1 = std::fs::read_to_string(&sha1_path).unwrap_or_default();

        if local_sha1.is_empty() {
            eprintln!("Feed '{}': no local sha1 — needs download", onestop_id);
            return Ok(true);
        }

        match transitland::latest_feed_sha1(api_key, onestop_id) {
            Ok(Some(remote_sha1)) if remote_sha1 != local_sha1 => {
                eprintln!(
                    "Feed '{}': stale (local: {}..., remote: {}...)",
                    onestop_id,
                    &local_sha1[..12.min(local_sha1.len())],
                    &remote_sha1[..12.min(remote_sha1.len())]
                );
                return Ok(true);
            }
            Ok(Some(_)) => {
                eprintln!("Feed '{}': up to date", onestop_id);
            }
            Ok(None) => {
                eprintln!("Feed '{}': no remote sha1 available", onestop_id);
            }
            Err(e) => {
                eprintln!("WARNING: could not check '{}': {}", onestop_id, e);
            }
        }
    }

    eprintln!("All feeds up to date");
    Ok(false)
}

fn cmd_prep(city_file: &Path, output: &Path, cache_dir: &Path) -> Result<()> {
    let city_json = std::fs::read_to_string(city_file)
        .with_context(|| format!("Failed to read city file: {:?}", city_file))?;
    let city: CityConfig = parse_to_serde_value(&city_json, &Default::default())
        .with_context(|| format!("Failed to parse city file: {:?}", city_file))?;

    anyhow::ensure!(
        !city.feed_ids.is_empty(),
        "feed_ids must not be empty in {:?}",
        city_file
    );

    let bbox = parse_bbox(&city.bbox)?;

    std::fs::create_dir_all(cache_dir.join("sha1"))?;

    // Resolve Transitland IDs to URLs
    let api_key = transitland::get_api_key().ok();
    let has_transitland_ids = city.feed_ids.iter().any(|f| f.starts_with("f-"));
    if has_transitland_ids && api_key.is_none() {
        anyhow::bail!("Config contains Transitland feed IDs but TRANSITLAND_API_KEY is not set");
    }
    let resolved_feeds: Vec<String> = city
        .feed_ids
        .iter()
        .map(|fid| resolve_feed_id(fid, api_key.as_deref()))
        .collect::<Result<Vec<_>>>()?;

    run_prep(
        &city.id,
        &resolved_feeds,
        city.bbbike_name.as_deref(),
        city.osm_url.as_deref(),
        bbox,
        output,
        cache_dir,
    )
}

fn cmd_generate(
    output: &Path,
    id: &str,
    bbbike_name: Option<&str>,
    osm_url: Option<&str>,
    cache_dir: &Path,
) -> Result<()> {
    anyhow::ensure!(
        bbbike_name.is_some() || osm_url.is_some(),
        "Either --bbbike-name or --osm-url must be provided"
    );

    let api_key = transitland::get_api_key()?;
    std::fs::create_dir_all(cache_dir)?;

    // Step 1: Download OSM PBF
    let pbf_path = if let Some(name) = bbbike_name {
        let cache_path = cache_dir.join(format!("{}.osm.pbf", osm::sanitize(id)));
        if cache_path.exists() {
            eprintln!("Using cached PBF: {:?}", cache_path);
            cache_path
        } else {
            osm::try_bbbike_download(name, &cache_path)?
        }
    } else if let Some(url) = osm_url {
        let cache_path = cache_dir.join(format!("{}.osm.pbf", osm::sanitize(id)));
        if cache_path.exists() {
            eprintln!("Using cached PBF: {:?}", cache_path);
            cache_path
        } else {
            eprintln!("Downloading OSM from: {}", url);
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .user_agent("Mozilla/5.0 (compatible; transit-prep/1.0)")
                .build()?;
            let bytes = client.get(url).send()?.error_for_status()?.bytes()?;
            eprintln!("Downloaded PBF: {:.1} MB", bytes.len() as f64 / 1_048_576.0);
            let tmp = cache_path.with_extension("tmp");
            std::fs::File::create(&tmp)?.write_all(&bytes)?;
            std::fs::rename(&tmp, &cache_path)?;
            cache_path
        }
    } else {
        unreachable!()
    };

    // Step 2: Extract bbox from PBF header
    eprintln!("\n--- Extracting bounding box from PBF ---");
    let (min_lon, min_lat, max_lon, max_lat) = graph::extract_pbf_bbox(&pbf_path)?;
    eprintln!(
        "Bounding box: {:.4},{:.4},{:.4},{:.4}",
        min_lon, min_lat, max_lon, max_lat
    );

    // Step 3: Query Transitland
    eprintln!("\n--- Querying Transitland for feeds ---");
    let bbox = (min_lon, min_lat, max_lon, max_lat);
    let feeds = transitland::query_feeds_in_bbox(&api_key, bbox)?;

    eprintln!("\n--- Querying Transitland for operators ---");
    let op_pairs = transitland::query_operators_in_bbox(&api_key, bbox)?;
    let op_map = transitland::build_feed_operator_map(&op_pairs);

    // Filter to feeds with a download URL
    let feeds: Vec<_> = feeds
        .into_iter()
        .filter(|f| {
            f.urls
                .static_current
                .as_ref()
                .map(|u| !u.is_empty())
                .unwrap_or(false)
        })
        .collect();

    eprintln!("\n{} feeds with download URLs found", feeds.len());

    // Step 4: Compute center
    let center_lat = (min_lat + max_lat) / 2.0;
    let center_lon = (min_lon + max_lon) / 2.0;

    // Step 5: Write JSONC config
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("    \"id\": \"{}\",\n", id));
    if let Some(name) = bbbike_name {
        out.push_str(&format!("    \"bbbike_name\": \"{}\",\n", name));
    } else if let Some(url) = osm_url {
        out.push_str(&format!("    \"osm_url\": \"{}\",\n", url));
    }
    out.push_str("    \"feed_ids\": [\n");
    for (i, feed) in feeds.iter().enumerate() {
        let comma = if i + 1 < feeds.len() { "," } else { "" };
        let comment = op_map
            .get(&feed.onestop_id)
            .map(|name| format!(" // {}", name))
            .unwrap_or_default();
        out.push_str(&format!(
            "        \"{}\"{}{}\n",
            feed.onestop_id, comma, comment
        ));
    }
    out.push_str("    ],\n");
    out.push_str("    \"name\": \"TODO\",\n");
    out.push_str(&format!("    \"file\": \"{}.bin\",\n", id));
    out.push_str(&format!(
        "    \"bbox\": \"{:.4},{:.4},{:.4},{:.4}\",\n",
        min_lon, min_lat, max_lon, max_lat
    ));
    out.push_str(&format!(
        "    \"center\": [{:.3}, {:.3}],\n",
        center_lat, center_lon
    ));
    out.push_str("    \"zoom\": 12,\n");
    out.push_str("    \"detail\": \"TODO\"\n");
    out.push_str("}\n");

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, &out)?;
    eprintln!("\nWrote config to {:?}", output);

    Ok(())
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
