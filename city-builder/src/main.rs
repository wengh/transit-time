//! Download and orchestrate transit data builds: GTFS + OSM → city `.bin`.
//!
//! Reads city `.jsonc` configs, fetches feeds/extracts (with caching and
//! freshness checks), then delegates the actual binary build to
//! [`transit_prep::prepare`]. The `transit-prep` crate is intentionally
//! network-free; everything that touches HTTP or Transitland lives here.

mod cache;
mod config;
mod gtfs_fetch;
mod http_cache;
mod osm_fetch;
mod transitland;

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::CityConfig;
use crate::gtfs_fetch::{
    fetch_gtfs, gtfs_cache_path, gtfs_sha1_path, is_transitland_id, sha1_recently_checked,
    validate_feed_id,
};
use transit_prep::parse_bbox;

#[derive(Parser)]
#[command(name = "city-builder")]
#[command(about = "Download GTFS + OSM and build per-city transit binaries")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Process a city config into binary transit data (downloads as needed).
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
    Check {
        #[arg(long)]
        city_file: PathBuf,
        #[arg(long, default_value = "cache")]
        cache_dir: PathBuf,
    },
    /// Build all cities: check feeds, download stale ones, rebuild affected .bin files.
    Pipeline {
        #[arg(long, default_value = "cities")]
        cities_dir: PathBuf,
        #[arg(long, default_value = "transit-viz/public/data")]
        output_dir: PathBuf,
        #[arg(long, default_value = "cache")]
        cache_dir: PathBuf,
        /// Only check what needs rebuilding (stages 1-3); exit 1 if rebuild
        /// is needed, 0 otherwise. No downloads, no builds.
        #[arg(long)]
        check_only: bool,
    },
    /// Generate a city config by querying Transitland for feeds in a geographic area.
    Generate {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        bbbike_name: Option<String>,
        #[arg(long)]
        interline_extract: Option<String>,
        #[arg(long)]
        osm_url: Option<String>,
        #[arg(long, default_value = "cache")]
        cache_dir: PathBuf,
    },
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
            interline_extract,
            osm_url,
            cache_dir,
        } => cmd_generate(
            &output,
            &id,
            bbbike_name.as_deref(),
            interline_extract.as_deref(),
            osm_url.as_deref(),
            &cache_dir,
        ),
    }
}

fn load_city_config(city_file: &Path) -> Result<CityConfig> {
    let city_json = std::fs::read_to_string(city_file)
        .with_context(|| format!("Failed to read city file: {:?}", city_file))?;
    jsonc_parser::parse_to_serde_value(&city_json, &Default::default())
        .with_context(|| format!("Failed to parse city file: {:?}", city_file))
}

fn cmd_prep(city_file: &Path, output: &Path, cache_dir: &Path) -> Result<()> {
    let city: CityConfig = load_city_config(city_file)?;
    if city.enabled == Some(false) {
        anyhow::bail!("City '{}' is disabled", city.id);
    }

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

    let gtfs_paths: Vec<PathBuf> = city
        .feed_ids
        .iter()
        .map(|fid| fetch_gtfs(fid, api_key.as_deref(), cache_dir))
        .collect::<Result<Vec<_>>>()?;

    let osm_path = osm_fetch::fetch_osm(
        bbox,
        cache_dir,
        &city.id,
        city.interline_extract.as_deref(),
        city.bbbike_name.as_deref(),
        city.osm_url.as_deref(),
    )?;

    transit_prep::prepare(
        &city.id,
        &gtfs_paths,
        &osm_path,
        bbox,
        output,
        city.allow_stale,
    )
}

fn cmd_check(city_file: &Path, cache_dir: &Path) -> Result<bool> {
    let city: CityConfig = load_city_config(city_file)?;
    if city.enabled == Some(false) {
        eprintln!("City '{}' is disabled, skipping check", city.id);
        return Ok(false);
    }

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
            Ok(Some(remote_sha1)) => {
                let _ = std::fs::write(&sha1_path, &remote_sha1);
                eprintln!("Feed '{}': up to date", feed_id);
            }
            Ok(None) => eprintln!("Feed '{}': no remote sha1 available", feed_id),
            Err(e) => eprintln!("WARNING: could not check '{}': {}", feed_id, e),
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
    use rayon::prelude::*;

    std::fs::create_dir_all(cache_dir.join("sha1"))?;

    let api_key = transitland::get_api_key().ok();

    // ── Stage 1: Extract feeds from city configs ──
    eprintln!("=== Stage 1: Extract feeds from city configs ===");

    let mut cities: Vec<(String, CityConfig, PathBuf)> = Vec::new();
    let mut feed_to_cities: HashMap<String, Vec<String>> = HashMap::new();

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
        let config: CityConfig = load_city_config(&path)?;

        if config.enabled == Some(false) {
            eprintln!("Skipping {} (disabled)", config.id);
            continue;
        }

        for fid in &config.feed_ids {
            validate_feed_id(fid, api_key.as_deref())?;
            feed_to_cities
                .entry(fid.clone())
                .or_default()
                .push(config.id.clone());
        }

        cities.push((config.id.clone(), config, path));
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
        let unverifiable = match transitland::latest_feed_sha1(key, feed_id) {
            Ok(Some(remote_sha1)) if remote_sha1 != local_sha1 => {
                eprintln!("  {}: sha1 changed → stale", feed_id);
                stale_feeds.insert(feed_id.clone());
                false
            }
            Ok(Some(remote_sha1)) => {
                let _ = std::fs::write(&sha1_path, &remote_sha1);
                eprintln!("  {}: up to date", feed_id);
                false
            }
            Ok(None) => {
                eprintln!("  {}: no remote sha1 available", feed_id);
                true
            }
            Err(e) => {
                eprintln!("  WARNING: {}: {}", feed_id, e);
                true
            }
        };

        // Couldn't verify by hash — fall back to the age rule.
        let zip_path = gtfs_cache_path(feed_id, cache_dir);
        if unverifiable && cache::is_expired(&zip_path) {
            eprintln!(
                "  {}: unverifiable and cache is {} day(s) old → stale",
                feed_id,
                cache::age_days(&zip_path)
            );
            stale_feeds.insert(feed_id.clone());
        }
    }

    // Direct-URL feeds: ask each origin whether its ETag still matches what we
    // recorded. Checked in parallel — these are header-only round trips.
    let url_feeds: Vec<&String> = feed_to_cities
        .keys()
        .filter(|f| !is_transitland_id(f))
        .collect();

    if !url_feeds.is_empty() {
        let client = http_cache::client(http_cache::CHECK_TIMEOUT)?;
        let checked: Vec<(&String, http_cache::CacheState)> = url_feeds
            .par_iter()
            .map(|feed_id| {
                let path = gtfs_cache_path(feed_id, cache_dir);
                let state = http_cache::check(
                    &client,
                    feed_id,
                    &path,
                    cache::MAX_CACHE_AGE,
                    &format!("  {}", feed_id),
                );
                (*feed_id, state)
            })
            .collect();

        for (feed_id, state) in checked {
            match state {
                http_cache::CacheState::Current => {}
                http_cache::CacheState::Missing => {
                    eprintln!("  {}: not cached → stale", feed_id);
                    stale_feeds.insert(feed_id.clone());
                }
                http_cache::CacheState::Stale => {
                    stale_feeds.insert(feed_id.clone());
                }
            }
        }
    }

    // ── Stage 3: Determine what needs rebuilding ──
    eprintln!("\n=== Stage 3: Determine what needs rebuilding ===");

    // OSM extracts are validated only once they've gone OSM_MAX_STALENESS
    // without a check — base maps move slowly and these files are ~100 MB
    // each, so we accept that much staleness rather than re-checking hourly.
    let osm_changed: HashMap<&str, bool> = {
        let client = http_cache::client(http_cache::CHECK_TIMEOUT)?;
        cities
            .par_iter()
            .map(|(id, config, _)| {
                let changed = match (
                    parse_bbox(&config.bbox),
                    osm_fetch::osm_request_url(
                        config.interline_extract.as_deref(),
                        config.bbbike_name.as_deref(),
                        config.osm_url.as_deref(),
                    ),
                ) {
                    (Ok(bbox), Some(url)) => {
                        let path = osm_fetch::osm_cache_path(
                            cache_dir,
                            id,
                            bbox,
                            config.interline_extract.as_deref(),
                            config.bbbike_name.as_deref(),
                            config.osm_url.as_deref(),
                        );
                        // Nothing cached → stage 5 downloads it; that alone is
                        // not a reason to rebuild an otherwise current .bin.
                        if !path.exists()
                            || http_cache::checked_within(&path, cache::OSM_MAX_STALENESS)
                        {
                            false
                        } else {
                            http_cache::check(
                                &client,
                                &url,
                                &path,
                                cache::OSM_MAX_STALENESS,
                                &format!("  {} osm", id),
                            ) == http_cache::CacheState::Stale
                        }
                    }
                    _ => false,
                };
                (id.as_str(), changed)
            })
            .collect()
    };

    let exe_mtime = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(&p).ok())
        .and_then(|m| m.modified().ok());

    let mut cities_to_rebuild: Vec<String> = Vec::new();

    for (id, config, city_path) in &cities {
        let bin_path = output_dir.join(format!("{}.bin", id));
        let has_stale_feed = config.feed_ids.iter().any(|f| stale_feeds.contains(f));
        let osm_stale = osm_changed.get(id.as_str()).copied().unwrap_or(false);
        let bin_missing = !bin_path.exists();
        let bin_mtime = std::fs::metadata(&bin_path)
            .ok()
            .and_then(|m| m.modified().ok());
        let code_changed = exe_mtime
            .and_then(|exe_t| bin_mtime.map(|bin_t| exe_t > bin_t))
            .unwrap_or(false);
        let config_changed = std::fs::metadata(city_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|cfg_t| bin_mtime.map(|bin_t| cfg_t > bin_t))
            .unwrap_or(false);

        let reason = if bin_missing {
            Some(".bin missing")
        } else if has_stale_feed {
            Some("stale feed")
        } else if osm_stale {
            Some("OSM extract changed upstream")
        } else if code_changed {
            Some("code changed")
        } else if config_changed {
            Some("config changed")
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

    // ── Stage 4: Download stale GTFS feeds ──
    eprintln!("\n=== Stage 4: Download data ===");

    let feeds_to_download: Vec<&String> = {
        let needed: HashSet<&String> = cities
            .iter()
            .filter(|(id, _, _)| cities_to_rebuild.contains(id))
            .flat_map(|(_, config, _)| config.feed_ids.iter())
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

    // ── Stage 5: Build city .bin files (downloads OSM on demand) ──
    eprintln!("\n=== Stage 5: Build city .bin files ===");

    std::fs::create_dir_all(output_dir)?;

    cities
        .par_iter()
        .filter(|(id, _, _)| cities_to_rebuild.contains(id))
        .try_for_each(|(id, config, _)| -> Result<()> {
            let bbox = parse_bbox(&config.bbox)?;

            let osm_path = osm_fetch::fetch_osm(
                bbox,
                cache_dir,
                id,
                config.interline_extract.as_deref(),
                config.bbbike_name.as_deref(),
                config.osm_url.as_deref(),
            )?;

            let gtfs_paths: Vec<PathBuf> = config
                .feed_ids
                .iter()
                .map(|fid| gtfs_cache_path(fid, cache_dir))
                .collect();
            let bin_path = output_dir.join(format!("{}.bin", id));

            eprintln!("\n--- Building {} ---", id);
            transit_prep::prepare(
                id,
                &gtfs_paths,
                &osm_path,
                bbox,
                &bin_path,
                config.allow_stale,
            )?;
            Ok(())
        })?;

    // ── Cleanup: Remove orphaned cache files ──
    eprintln!("\n=== Cleanup: Remove orphaned cache files ===");

    let mut expected_files: HashSet<PathBuf> = HashSet::new();

    for feed_id in feed_to_cities.keys() {
        expected_files.insert(gtfs_cache_path(feed_id, cache_dir));
        expected_files.insert(gtfs_sha1_path(feed_id, cache_dir));
    }

    for (id, config, _) in &cities {
        if let Some(url) = osm_fetch::pick_source_url(
            config.interline_extract.as_deref(),
            config.bbbike_name.as_deref(),
            config.osm_url.as_deref(),
        ) {
            expected_files.insert(osm_fetch::pbf_cache_path(cache_dir, id, &url, "osm.pbf"));
            expected_files.insert(osm_fetch::pbf_cache_path(cache_dir, id, &url, "osm.xml"));
        }
        if let Ok(bbox) = parse_bbox(&config.bbox) {
            expected_files.insert(osm_fetch::overpass_cache_path(cache_dir, bbox));
        }
    }

    // An ETag sidecar is expected wherever its cache file is.
    for path in expected_files.clone() {
        expected_files.insert(http_cache::sidecar_path(&path));
    }

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
                || name.ends_with(".osm.xml")
                || (name.starts_with("osm_") && name.ends_with(".xml")))
                && !expected_files.contains(&path)
            {
                eprintln!("  removing orphaned: {}", name);
                let _ = std::fs::remove_file(&path);
                removed += 1;
            }
        }
    }

    for (subdir, ext) in [("sha1", "sha1"), ("etag", "etag")] {
        let dir = cache_dir.join(subdir);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_file()
                    && path.extension().is_some_and(|e| e == ext)
                    && !expected_files.contains(&path)
                {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    eprintln!("  removing orphaned: {}/{}", subdir, name);
                    let _ = std::fs::remove_file(&path);
                    removed += 1;
                }
            }
        }
    }

    let active_city_ids: HashSet<&str> = cities.iter().map(|(id, _, _)| id.as_str()).collect();
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

fn cmd_generate(
    output: &Path,
    id: &str,
    bbbike_name: Option<&str>,
    interline_extract: Option<&str>,
    osm_url: Option<&str>,
    cache_dir: &Path,
) -> Result<()> {
    let provided = bbbike_name.is_some() as usize
        + interline_extract.is_some() as usize
        + osm_url.is_some() as usize;
    anyhow::ensure!(
        provided == 1,
        "Exactly one of --bbbike-name, --interline-extract, or --osm-url must be provided",
    );

    let api_key = transitland::get_api_key()?;
    std::fs::create_dir_all(cache_dir)?;

    // ensure! above guarantees exactly one source is set, so fetch_osm's
    // bbox arg is unused (the Overpass fallback only runs when no source
    // is configured). Pass a sentinel bbox to keep one code path.
    let pbf_path = osm_fetch::fetch_osm(
        (0.0, 0.0, 0.0, 0.0),
        cache_dir,
        id,
        interline_extract,
        bbbike_name,
        osm_url,
    )?;

    // Step 2: Extract bbox from PBF header
    eprintln!("\n--- Extracting bounding box from PBF ---");
    let (min_lon, min_lat, max_lon, max_lat) = transit_prep::graph::extract_pbf_bbox(&pbf_path)?;
    eprintln!(
        "Bounding box: {:.4},{:.4},{:.4},{:.4}",
        min_lon, min_lat, max_lon, max_lat
    );

    // Step 3: Query Transitland
    eprintln!("\n--- Querying Transitland for feeds ---");
    let bbox = (min_lon, min_lat, max_lon, max_lat);
    let feeds = transitland::query_feeds_in_bbox(&api_key, bbox)?;

    eprintln!("\n--- Querying Transitland for operators ---");
    let op_map = match transitland::query_operators_in_bbox(&api_key, bbox) {
        Ok(op_pairs) => transitland::build_feed_operator_map(&op_pairs),
        Err(e) => {
            eprintln!(
                "WARNING: operators query failed ({}), continuing without operator names",
                e
            );
            std::collections::HashMap::new()
        }
    };

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

    let center_lat = (min_lat + max_lat) / 2.0;
    let center_lon = (min_lon + max_lon) / 2.0;

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("    \"id\": \"{}\",\n", id));
    if let Some(extract_id) = interline_extract {
        out.push_str(&format!("    \"interline_extract\": \"{}\",\n", extract_id));
    } else if let Some(name) = bbbike_name {
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
