mod binary;
mod download;
mod graph;
mod gtfs;
mod osm;
mod transitland;

use anyhow::{Context, Result};
use clap::Parser;
use jsonc_parser::parse_to_serde_value;
use std::collections::{HashMap, HashSet};
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
    /// Build all cities: check feeds, download stale ones, rebuild affected .bin files
    Pipeline {
        /// Directory containing city .jsonc config files
        #[arg(long, default_value = "cities")]
        cities_dir: PathBuf,

        /// Output directory for .bin files
        #[arg(long, default_value = "transit-viz/public/data")]
        output_dir: PathBuf,

        /// Cache directory
        #[arg(long, default_value = "cache")]
        cache_dir: PathBuf,

        /// Only check what needs rebuilding (stages 1-3), don't download or build.
        /// Exit 0 if nothing to rebuild, exit 1 if rebuild needed.
        #[arg(long)]
        check_only: bool,
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

/// Check if a feed ID is a Transitland onestop ID (starts with "f-").
fn is_transitland_id(feed_id: &str) -> bool {
    feed_id.starts_with("f-")
}

/// FNV-1a hash of a URL.
fn url_hash(url: &str) -> u64 {
    url.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        h.wrapping_mul(0x100000001b3) ^ b as u64
    })
}

/// Cache path for the GTFS zip download.
/// Transitland feeds use the onestop ID as filename; direct URLs use a hash.
fn gtfs_cache_path(feed_id: &str, cache_dir: &Path) -> PathBuf {
    if is_transitland_id(feed_id) {
        cache_dir.join(format!("{}.gtfs.zip", feed_id))
    } else {
        cache_dir.join(format!("url_{:016x}.gtfs.zip", url_hash(feed_id)))
    }
}

/// Cache path for the sha1 sidecar (stored in cache_dir/sha1/ so it can be
/// cached separately from the large download files in CI).
fn gtfs_sha1_path(feed_id: &str, cache_dir: &Path) -> PathBuf {
    if is_transitland_id(feed_id) {
        cache_dir.join("sha1").join(format!("{}.sha1", feed_id))
    } else {
        cache_dir
            .join("sha1")
            .join(format!("url_{:016x}.sha1", url_hash(feed_id)))
    }
}

/// Download a GTFS feed (Transitland or direct URL) into the cache directory.
/// For Transitland feeds, uses header-based auth and saves the SHA1 sidecar.
fn fetch_gtfs(feed_id: &str, api_key: Option<&str>, cache_dir: &Path) -> Result<PathBuf> {
    let cache_path = gtfs_cache_path(feed_id, cache_dir);
    let sha1_path = gtfs_sha1_path(feed_id, cache_dir);

    if cache_path.exists() && !is_transitland_id(feed_id) {
        eprintln!("Using cached GTFS: {:?}", cache_path);
        return Ok(cache_path);
    }

    if is_transitland_id(feed_id) {
        let key =
            api_key.with_context(|| format!("Feed '{}' requires TRANSITLAND_API_KEY", feed_id))?;

        // If cached, check staleness via SHA1 (skip if checked recently)
        if cache_path.exists() {
            if sha1_recently_checked(&sha1_path) {
                eprintln!("Using cached GTFS (checked recently): {:?}", cache_path);
                return Ok(cache_path);
            }
            let local_sha1 = std::fs::read_to_string(&sha1_path).unwrap_or_default();
            match transitland::latest_feed_sha1(key, feed_id) {
                Ok(Some(remote_sha1)) if !local_sha1.is_empty() && local_sha1 == remote_sha1 => {
                    // Touch the sha1 file to record that we just checked
                    let _ = std::fs::write(&sha1_path, &remote_sha1);
                    eprintln!("Using cached GTFS (up to date): {:?}", cache_path);
                    return Ok(cache_path);
                }
                Ok(Some(remote_sha1)) => {
                    eprintln!(
                        "Transitland feed '{}' has new version (sha1: {}), re-downloading...",
                        feed_id,
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
        }

        // Download via Transitland with header auth
        download::with_download_lock(&cache_path, |path| {
            if path.exists() {
                // Check again — parallel process may have just downloaded
                if sha1_recently_checked(&sha1_path) {
                    return Ok(path.to_path_buf());
                }
            }
            eprintln!("Downloading GTFS from Transitland: {}", feed_id);
            let bytes = transitland::download_feed(key, feed_id)?;
            let tmp = path.with_extension("zip.tmp");
            std::fs::File::create(&tmp)?.write_all(&bytes)?;
            std::fs::rename(&tmp, path)?;

            // Save sha1 for future staleness checks
            if let Ok(Some(sha1)) = transitland::latest_feed_sha1(key, feed_id) {
                let _ = std::fs::write(&sha1_path, &sha1);
            }

            Ok(path.to_path_buf())
        })
    } else {
        // Direct URL download
        download::with_download_lock(&cache_path, |path| {
            if path.exists() {
                eprintln!(
                    "Using cached GTFS (downloaded by parallel process): {:?}",
                    path
                );
                return Ok(path.to_path_buf());
            }
            eprintln!("Downloading GTFS from: {}", feed_id);
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .user_agent("Mozilla/5.0 (compatible; transit-prep/1.0)")
                .build()?;
            let bytes = client.get(feed_id).send()?.error_for_status()?.bytes()?;
            let tmp = path.with_extension("zip.tmp");
            std::fs::File::create(&tmp)?.write_all(&bytes)?;
            std::fs::rename(&tmp, path)?;
            Ok(path.to_path_buf())
        })
    }
}

/// Check if a sha1 sidecar was written/updated less than 2 days ago.
fn sha1_recently_checked(sha1_path: &Path) -> bool {
    std::fs::metadata(sha1_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|mtime| {
            mtime.elapsed().unwrap_or_default() < std::time::Duration::from_secs(2 * 24 * 3600)
        })
        .unwrap_or(false)
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
fn validate_feed_id(feed_id: &str, api_key: Option<&str>) -> Result<()> {
    if feed_id.starts_with("http://") || feed_id.starts_with("https://") {
        return Ok(());
    }
    if feed_id.starts_with("f-") {
        anyhow::ensure!(
            api_key.is_some(),
            "Feed '{}' is a Transitland ID but TRANSITLAND_API_KEY is not set",
            feed_id
        );
        return Ok(());
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
        Commands::Pipeline {
            cities_dir,
            output_dir,
            cache_dir,
            check_only,
        } => {
            let needs_rebuild = cmd_pipeline(&cities_dir, &output_dir, &cache_dir, check_only)?;
            if check_only && needs_rebuild {
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
        if !is_transitland_id(feed_id) {
            continue; // direct URLs — no remote check available
        }
        let key = api_key
            .as_deref()
            .with_context(|| format!("Feed '{}' requires TRANSITLAND_API_KEY", feed_id))?;

        let sha1_path = gtfs_sha1_path(feed_id, cache_dir);

        if sha1_recently_checked(&sha1_path) {
            eprintln!("Feed '{}': fresh (checked recently)", feed_id);
            continue;
        }

        let local_sha1 = std::fs::read_to_string(&sha1_path).unwrap_or_default();

        if local_sha1.is_empty() {
            eprintln!("Feed '{}': no local sha1 — needs download", feed_id);
            return Ok(true);
        }

        match transitland::latest_feed_sha1(key, feed_id) {
            Ok(Some(remote_sha1)) if remote_sha1 != local_sha1 => {
                eprintln!(
                    "Feed '{}': stale (local: {}..., remote: {}...)",
                    feed_id,
                    &local_sha1[..12.min(local_sha1.len())],
                    &remote_sha1[..12.min(remote_sha1.len())]
                );
                return Ok(true);
            }
            Ok(Some(_)) => {
                eprintln!("Feed '{}': up to date", feed_id);
            }
            Ok(None) => {
                eprintln!("Feed '{}': no remote sha1 available", feed_id);
            }
            Err(e) => {
                eprintln!("WARNING: could not check '{}': {}", feed_id, e);
            }
        }
    }

    eprintln!("All feeds up to date");
    Ok(false)
}

/// Build pipeline: check all cities, download stale feeds, rebuild affected .bin files.
/// Returns true if any city needed rebuilding.
fn cmd_pipeline(
    cities_dir: &Path,
    output_dir: &Path,
    cache_dir: &Path,
    check_only: bool,
) -> Result<bool> {
    std::fs::create_dir_all(cache_dir.join("sha1"))?;

    let api_key = transitland::get_api_key().ok();

    // ── Stage 1: Extract feeds from city configs ──
    eprintln!("=== Stage 1: Extract feeds from city configs ===");

    let mut cities: Vec<(String, CityConfig)> = Vec::new();
    let mut feed_to_cities: HashMap<String, Vec<String>> = HashMap::new(); // feed_id → city_ids

    let mut entries: Vec<_> = std::fs::read_dir(cities_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "jsonc" || ext == "json")
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by_key(|e| e.path());

    for entry in &entries {
        let path = entry.path();
        let city_json =
            std::fs::read_to_string(&path).with_context(|| format!("Failed to read {:?}", path))?;
        let config: CityConfig = parse_to_serde_value(&city_json, &Default::default())
            .with_context(|| format!("Failed to parse {:?}", path))?;

        for fid in &config.feed_ids {
            validate_feed_id(fid, api_key.as_deref())?;
            feed_to_cities
                .entry(fid.clone())
                .or_default()
                .push(config.id.clone());
        }

        cities.push((config.id.clone(), config));
    }

    let tl_feeds: Vec<_> = feed_to_cities
        .keys()
        .filter(|f| is_transitland_id(f))
        .cloned()
        .collect();

    eprintln!(
        "  {} cities, {} unique feeds ({} Transitland)",
        cities.len(),
        feed_to_cities.len(),
        tl_feeds.len()
    );

    // ── Stage 2: Check Transitland feed hashes ──
    eprintln!("\n=== Stage 2: Check Transitland feed hashes ===");

    let mut stale_feeds: HashSet<String> = HashSet::new();

    for feed_id in &tl_feeds {
        let sha1_path = gtfs_sha1_path(feed_id, cache_dir);

        if sha1_recently_checked(&sha1_path) {
            eprintln!("  {}: fresh (checked recently)", feed_id);
            continue;
        }

        let local_sha1 = std::fs::read_to_string(&sha1_path).unwrap_or_default();

        if local_sha1.is_empty() {
            eprintln!("  {}: no local sha1 → stale", feed_id);
            stale_feeds.insert(feed_id.clone());
            continue;
        }

        let key = api_key.as_deref().unwrap(); // validated in stage 1
        match transitland::latest_feed_sha1(key, feed_id) {
            Ok(Some(remote_sha1)) if remote_sha1 != local_sha1 => {
                eprintln!("  {}: sha1 changed → stale", feed_id);
                stale_feeds.insert(feed_id.clone());
            }
            Ok(Some(remote_sha1)) => {
                // Touch sha1 file to record successful check
                let _ = std::fs::write(&sha1_path, &remote_sha1);
                eprintln!("  {}: up to date", feed_id);
            }
            Ok(None) => {
                eprintln!("  {}: no remote sha1 available", feed_id);
            }
            Err(e) => {
                eprintln!("  WARNING: {}: {}", feed_id, e);
            }
        }
    }

    // Also check for uncached direct URL feeds
    for feed_id in feed_to_cities.keys() {
        if !is_transitland_id(feed_id) && !gtfs_cache_path(feed_id, cache_dir).exists() {
            stale_feeds.insert(feed_id.clone());
        }
    }

    // ── Stage 3: Determine what needs rebuilding ──
    eprintln!("\n=== Stage 3: Determine what needs rebuilding ===");

    // Check if the binary itself is newer than any .bin (i.e. code changed)
    let exe_mtime = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(&p).ok())
        .and_then(|m| m.modified().ok());

    let mut cities_to_rebuild: Vec<String> = Vec::new();

    for (id, config) in &cities {
        let bin_path = output_dir.join(format!("{}.bin", id));
        let has_stale_feed = config.feed_ids.iter().any(|f| stale_feeds.contains(f));
        let bin_missing = !bin_path.exists();
        let code_changed = exe_mtime
            .and_then(|exe_t| {
                std::fs::metadata(&bin_path)
                    .ok()?
                    .modified()
                    .ok()
                    .map(|bin_t| exe_t > bin_t)
            })
            .unwrap_or(false);

        let reason = if bin_missing {
            Some(".bin missing")
        } else if has_stale_feed {
            Some("stale feed")
        } else if code_changed {
            Some("code changed")
        } else {
            None
        };

        if let Some(reason) = reason {
            eprintln!("  {}: needs rebuild ({})", id, reason);
            cities_to_rebuild.push(id.clone());
        } else {
            eprintln!("  {}: up to date", id);
        }
    }

    if cities_to_rebuild.is_empty() {
        eprintln!("\nNothing to rebuild.");
        return Ok(false);
    }

    eprintln!(
        "\n  {} cities to rebuild: {}",
        cities_to_rebuild.len(),
        cities_to_rebuild.join(", ")
    );

    if check_only {
        return Ok(true);
    }

    // ── Stage 4: Download stale GTFS feeds + OSM data ──
    eprintln!("\n=== Stage 4: Download data ===");

    use rayon::prelude::*;

    // Only download feeds needed by cities we're rebuilding
    let feeds_to_download: Vec<&String> = {
        let needed: HashSet<&String> = cities
            .iter()
            .filter(|(id, _)| cities_to_rebuild.contains(id))
            .flat_map(|(_, config)| config.feed_ids.iter())
            .collect();
        needed
            .into_iter()
            .filter(|fid| stale_feeds.contains(*fid) || !gtfs_cache_path(fid, cache_dir).exists())
            .collect()
    };

    feeds_to_download
        .par_iter()
        .try_for_each(|feed_id| -> Result<()> {
            fetch_gtfs(feed_id, api_key.as_deref(), cache_dir)?;
            Ok(())
        })?;

    // Download OSM data only if not already cached (OSM data is stable)
    cities
        .par_iter()
        .filter(|(id, _)| cities_to_rebuild.contains(id))
        .try_for_each(|(id, config)| -> Result<()> {
            let bbox = parse_bbox(&config.bbox)?;
            let osm_path = osm::cache_path(
                cache_dir,
                id,
                bbox,
                config.bbbike_name.as_deref(),
                config.osm_url.as_deref(),
            );
            if !osm_path.exists() {
                osm::fetch_osm(
                    bbox,
                    cache_dir,
                    id,
                    config.bbbike_name.as_deref(),
                    config.osm_url.as_deref(),
                )?;
            } else {
                eprintln!("  OSM for {}: cached", id);
            }
            Ok(())
        })?;

    // ── Stage 5: Build city .bin files ──
    eprintln!("\n=== Stage 5: Build city .bin files ===");

    std::fs::create_dir_all(output_dir)?;

    cities
        .par_iter()
        .filter(|(id, _)| cities_to_rebuild.contains(id))
        .try_for_each(|(id, config)| -> Result<()> {
            let bin_path = output_dir.join(format!("{}.bin", id));
            let bbox = parse_bbox(&config.bbox)?;
            let gtfs_paths: Vec<PathBuf> = config
                .feed_ids
                .iter()
                .map(|fid| gtfs_cache_path(fid, cache_dir))
                .collect();
            let osm_path = osm::cache_path(
                cache_dir,
                id,
                bbox,
                config.bbbike_name.as_deref(),
                config.osm_url.as_deref(),
            );

            eprintln!("\n--- Building {} ---", id);
            run_prep(id, &gtfs_paths, &osm_path, bbox, &bin_path)?;
            Ok(())
        })?;

    // ── Stage 6: Clean up orphaned cache files ──
    eprintln!("\n=== Stage 6: Clean up orphaned cache files ===");

    // Collect expected cache filenames
    let mut expected_files: HashSet<PathBuf> = HashSet::new();

    // GTFS zips and sha1 sidecars for all active feeds
    for feed_id in feed_to_cities.keys() {
        expected_files.insert(gtfs_cache_path(feed_id, cache_dir));
        expected_files.insert(gtfs_sha1_path(feed_id, cache_dir));
    }

    // OSM files for all active cities
    for (id, config) in &cities {
        if let Ok(bbox) = parse_bbox(&config.bbox) {
            expected_files.insert(osm::cache_path(
                cache_dir,
                id,
                bbox,
                config.bbbike_name.as_deref(),
                config.osm_url.as_deref(),
            ));
        }
    }

    // Scan cache dir for orphaned files
    let mut removed = 0usize;
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if (name.ends_with(".gtfs.zip")
                || name.ends_with(".osm.pbf")
                || name.ends_with(".osm.xml"))
                && !expected_files.contains(&path)
            {
                eprintln!("  removing orphaned: {}", name);
                let _ = std::fs::remove_file(&path);
                removed += 1;
            }
        }
    }

    // Scan sha1 dir for orphaned sidecars
    let sha1_dir = cache_dir.join("sha1");
    if let Ok(entries) = std::fs::read_dir(&sha1_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file()
                && path.extension().map_or(false, |e| e == "sha1")
                && !expected_files.contains(&path)
            {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                eprintln!("  removing orphaned: sha1/{}", name);
                let _ = std::fs::remove_file(&path);
                removed += 1;
            }
        }
    }

    // Scan output dir for orphaned .bin files
    let active_city_ids: HashSet<&str> = cities.iter().map(|(id, _)| id.as_str()).collect();
    if let Ok(entries) = std::fs::read_dir(output_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "bin") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if !active_city_ids.contains(stem) {
                        eprintln!("  removing orphaned: {}.bin", stem);
                        let _ = std::fs::remove_file(&path);
                        removed += 1;
                    }
                }
            }
        }
    }

    if removed == 0 {
        eprintln!("  no orphaned files");
    } else {
        eprintln!("  removed {} orphaned file(s)", removed);
    }

    eprintln!("\n=== Pipeline complete ===");
    Ok(true)
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

    let api_key = transitland::get_api_key().ok();
    for fid in &city.feed_ids {
        validate_feed_id(fid, api_key.as_deref())?;
    }

    // Download GTFS feeds
    let gtfs_paths: Vec<PathBuf> = city
        .feed_ids
        .iter()
        .map(|fid| fetch_gtfs(fid, api_key.as_deref(), cache_dir))
        .collect::<Result<Vec<_>>>()?;

    // Download OSM data
    let osm_path = osm::fetch_osm(
        bbox,
        cache_dir,
        &city.id,
        city.bbbike_name.as_deref(),
        city.osm_url.as_deref(),
    )?;

    run_prep(&city.id, &gtfs_paths, &osm_path, bbox, output)
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
    gtfs_paths: &[PathBuf],
    osm_path: &Path,
    bbox: (f64, f64, f64, f64),
    output: &Path,
) -> Result<()> {
    eprintln!("=== Transit Prep for '{}' ===", city);
    eprintln!("Bounding box: {:?}", bbox);

    // Step 1: Parse GTFS data
    eprintln!("\n--- Parsing GTFS data ---");

    let mut merged: Option<gtfs::GtfsData> = None;
    for path in gtfs_paths {
        let data = gtfs::parse_gtfs(path)?;
        eprintln!(
            "  {:?}: {} stops, {} routes, {} trips",
            path.file_name().unwrap_or_default(),
            data.stops.len(),
            data.routes.len(),
            data.trips.len()
        );
        warn_if_expired(&path.to_string_lossy(), &data);
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

    // Filter stops to bbox and re-index sequentially so out-of-bbox stops occupy no ids
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    gtfs_data
        .stops
        .retain(|s| s.lat >= min_lat && s.lat <= max_lat && s.lon >= min_lon && s.lon <= max_lon);
    for (i, stop) in gtfs_data.stops.iter_mut().enumerate() {
        stop.index = i as u32;
    }
    eprintln!("  {} stops within bbox", gtfs_data.stops.len());

    // Identify trips with ≥2 in-bbox stops; trips with 0-1 are useless (no edges)
    let in_bbox_stop_ids: HashSet<&str> = gtfs_data.stops.iter().map(|s| s.id.as_str()).collect();
    let mut stops_per_trip: HashMap<&str, usize> = HashMap::new();
    for st in &gtfs_data.stop_times {
        if in_bbox_stop_ids.contains(st.stop_id.as_str()) {
            *stops_per_trip.entry(st.trip_id.as_str()).or_default() += 1;
        }
    }
    let valid_trip_ids: HashSet<&str> = stops_per_trip
        .iter()
        .filter(|(_, &count)| count >= 2)
        .map(|(&id, _)| id)
        .collect();
    eprintln!(
        "  {} trips with ≥2 in-bbox stops (of {} total)",
        valid_trip_ids.len(),
        gtfs_data.trips.len()
    );

    // Step 2: Build OSM graph
    eprintln!("\n--- Building OSM graph ---");
    let mut osm_graph = graph::build_graph(osm_path, bbox)?;
    eprintln!(
        "Graph: {} nodes, {} edges",
        osm_graph.nodes.len(),
        osm_graph.edges.len(),
    );

    // Step 3: Snap stops to OSM edges (inserting virtual nodes)
    eprintln!("\n--- Snapping stops to OSM edges ---");
    let stop_to_node = graph::snap_stops_to_nodes(&gtfs_data.stops, &mut osm_graph);
    eprintln!("Snapped {} stops", stop_to_node.len());

    // Step 4: Build service patterns and event arrays
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
    // Build route_id -> shape_ids from trips with ≥2 in-bbox stops only
    let mut route_id_to_shapes: HashMap<&str, HashSet<&str>> = HashMap::new();
    for trip in &gtfs_data.trips {
        if !valid_trip_ids.contains(trip.id.as_str()) {
            continue;
        }
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

    // Prune shapes: keep only those referenced by routes, then trim to bbox
    let referenced_shape_ids: HashSet<&str> = route_shapes
        .iter()
        .flat_map(|shapes| shapes.iter().map(|s| s.as_str()))
        .collect();
    let total_shapes = gtfs_data.shapes.len();
    gtfs_data
        .shapes
        .retain(|id, _| referenced_shape_ids.contains(id.as_str()));
    for points in gtfs_data.shapes.values_mut() {
        let first_in = points.iter().position(|&(lat, lon)| {
            lat >= min_lat && lat <= max_lat && lon >= min_lon && lon <= max_lon
        });
        let last_in = points.iter().rposition(|&(lat, lon)| {
            lat >= min_lat && lat <= max_lat && lon >= min_lon && lon <= max_lon
        });
        if let (Some(f), Some(l)) = (first_in, last_in) {
            *points = points[f..=l].to_vec();
        } else {
            points.clear();
        }
    }
    gtfs_data.shapes.retain(|_, pts| !pts.is_empty());
    eprintln!(
        "  {} shapes after pruning and trimming (of {} total)",
        gtfs_data.shapes.len(),
        total_shapes
    );

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
