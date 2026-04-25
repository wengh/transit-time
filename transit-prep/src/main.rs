mod binary;
mod graph;
mod gtfs;
mod osm;
mod transitland;

use anyhow::{Context, Result};
use chrono::NaiveDate;
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
    allow_stale: Option<bool>,
    enabled: Option<bool>,
}

fn unix_epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
}

/// Days since Unix epoch (UTC).
fn unix_days_now() -> u32 {
    (chrono::Utc::now().date_naive() - unix_epoch()).num_days() as u32
}

/// YYYYMMDD → days since Unix epoch.
fn yyyymmdd_to_days(date: u32) -> u32 {
    let y = (date / 10000) as i32;
    let m = (date / 100) % 100;
    let d = date % 100;
    let nd = NaiveDate::from_ymd_opt(y, m, d).expect("invalid YYYYMMDD date");
    (nd - unix_epoch()).num_days() as u32
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

/// Adjust per-feed service calendars for stale or not-yet-started feeds so the
/// data remains useful for isochrone queries on today's date.
fn apply_stale_policy(data: &mut gtfs::GtfsData, allow_stale: Option<bool>, today_days: u32) {
    const THRESHOLD_DAYS: u32 = 7;

    match allow_stale {
        Some(false) => return,
        Some(true) => {
            for s in &mut data.services {
                s.start_date = 0;
                s.end_date = 0;
            }
            return;
        }
        None => {}
    }

    // Gate on the publisher's authoritative dates from feed_info.txt only.
    // If a date isn't specified, we can't tell whether the feed covers today,
    // so be conservative and apply the corresponding extension.
    let do_stale = data
        .feed_end_date
        .filter(|&d| d != 0)
        .map(yyyymmdd_to_days)
        .map_or(true, |m| today_days + THRESHOLD_DAYS > m);

    let do_new = data
        .feed_start_date
        .filter(|&d| d != 0)
        .map(yyyymmdd_to_days)
        .map_or(true, |m| m + THRESHOLD_DAYS > today_days);

    if !do_stale && !do_new {
        return;
    }

    eprintln!(
        "Applying stale policy: feed date from {:?} to {:?} → do_stale={}, do_new={}",
        data.feed_start_date, data.feed_end_date, do_stale, do_new,
    );

    // Precompute (start_days, end_days) per service with sentinel values so
    // unbounded endpoints compare correctly. u32::MIN for "no start", u32::MAX
    // for "no end" (i.e. the service runs forever already).
    let service_info: Vec<(u32, u32)> = data
        .services
        .iter()
        .map(|s| {
            (
                if s.start_date != 0 {
                    yyyymmdd_to_days(s.start_date)
                } else {
                    u32::MIN
                },
                if s.end_date != 0 {
                    yyyymmdd_to_days(s.end_date)
                } else {
                    u32::MAX
                },
            )
        })
        .collect();

    // Services superseded by a successor (stale path): leave these alone so
    // the router drops them on today's date naturally.
    let has_successor: Vec<bool> = service_info
        .iter()
        .enumerate()
        .map(|(i, &(_, a_end))| {
            // Already-unbounded services have nothing to extend and no
            // meaningful "end" to compare against.
            if a_end == u32::MAX {
                return false;
            }
            let handoff = a_end as i64 + 1;
            service_info
                .iter()
                .enumerate()
                .any(|(j, &(b_start, b_end))| {
                    if i == j {
                        return false;
                    }
                    if b_start == u32::MIN {
                        return false;
                    }
                    if b_end <= a_end {
                        return false;
                    }
                    (b_start as i64 - handoff).abs() <= THRESHOLD_DAYS as i64
                })
        })
        .collect();

    // Services that have a predecessor (too-new path): leave these alone so
    // the predecessor covers today and this service activates at its natural date.
    let has_predecessor: Vec<bool> = service_info
        .iter()
        .enumerate()
        .map(|(i, &(a_start, _))| {
            // Unbounded-start services have no meaningful start to compare against.
            if a_start == u32::MIN {
                return false;
            }
            let handoff = a_start as i64 - 1;
            service_info
                .iter()
                .enumerate()
                .any(|(j, &(b_start, b_end))| {
                    if i == j {
                        return false;
                    }
                    if b_end == u32::MAX {
                        return false;
                    }
                    if b_start >= a_start {
                        return false;
                    }
                    (b_end as i64 - handoff).abs() <= THRESHOLD_DAYS as i64
                })
        })
        .collect();

    for (i, s) in data.services.iter_mut().enumerate() {
        let (a_start, a_end) = service_info[i];

        if do_stale && a_end != u32::MAX && !has_successor[i] {
            eprintln!(
                "  stale: extending service '{}' ({}-{}) end → unbounded",
                s.id, s.start_date, s.end_date
            );
            s.end_date = 0;
        }

        if do_new && a_start != u32::MIN && !has_predecessor[i] {
            eprintln!(
                "  too-new: extending service '{}' ({}-{}) start → unbounded",
                s.id, s.start_date, s.end_date
            );
            s.start_date = 0;
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

        eprintln!("Downloading GTFS from Transitland: {}", feed_id);
        let bytes = transitland::download_feed(key, feed_id)
            .with_context(|| format!("Failed to fetch GTFS feed '{}'", feed_id))?;
        let tmp = cache_path.with_extension("zip.tmp");
        std::fs::File::create(&tmp)?.write_all(&bytes)?;
        std::fs::rename(&tmp, &cache_path)?;

        // Save sha1 for future staleness checks
        if let Ok(Some(sha1)) = transitland::latest_feed_sha1(key, feed_id) {
            let _ = std::fs::write(&sha1_path, &sha1);
        }

        Ok(cache_path)
    } else {
        eprintln!("Downloading GTFS from: {}", feed_id);
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .user_agent("Mozilla/5.0 (compatible; transit-prep/1.0)")
            .build()?;
        let bytes = client
            .get(feed_id)
            .send()
            .with_context(|| format!("Failed to request GTFS URL '{}'", feed_id))?
            .error_for_status()
            .with_context(|| format!("GTFS URL returned error status '{}'", feed_id))?
            .bytes()
            .with_context(|| format!("Failed to read GTFS response body from '{}'", feed_id))?;
        let tmp = cache_path.with_extension("zip.tmp");
        std::fs::File::create(&tmp)?.write_all(&bytes)?;
        std::fs::rename(&tmp, &cache_path)?;
        Ok(cache_path)
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

#[derive(Clone, Copy)]
struct ShapeMatch {
    seg_idx: usize,
    t: f64,
    proj: (f64, f64),
    dist_sq: f64,
}

/// DP subsequence matching: find the minimum-cost monotone assignment of stops
/// to shape *segments*, recording the projection of each stop onto its segment.
fn match_stops_to_shape(
    stop_coords: &[(f64, f64)],
    shape: &[(f64, f64)],
    cos_lat: f64,
) -> Option<Vec<ShapeMatch>> {
    let (cost, assignment) = match_stops_to_shape_impl(stop_coords, shape, cos_lat)?;

    // Try reverse direction if the cost is abnormally high.
    // This happens in Mexico City metro line 9 for example
    let avg_cost = cost / stop_coords.len() as f64;
    const THRESHOLD: f64 = 0.0005; // ~50m
    if avg_cost > THRESHOLD * THRESHOLD {
        if let Some((rev_cost, rev_assignment)) = match_stops_to_shape_impl(
            &stop_coords.iter().rev().cloned().collect::<Vec<_>>(),
            shape,
            cos_lat,
        ) {
            // Only accept if the reverse is much better
            if rev_cost * 5.0 < cost {
                return Some(rev_assignment.into_iter().rev().collect());
            }
        }
    }
    Some(assignment)
}

fn match_stops_to_shape_impl(
    stop_coords: &[(f64, f64)],
    shape: &[(f64, f64)],
    cos_lat: f64,
) -> Option<(f64, Vec<ShapeMatch>)> {
    let n = stop_coords.len();
    let m = shape.len();
    // Need at least one segment, i.e. m >= 2.
    if n == 0 || m < 2 || n > m {
        return None;
    }
    let segs = m - 1;

    // Precompute the projection of every stop onto every segment once.
    // matches[i * segs + j] = projection of stop i onto segment j.
    let mut matches: Vec<ShapeMatch> = Vec::with_capacity(n * segs);
    for i in 0..n {
        for j in 0..segs {
            let (t, proj, d) =
                graph::project_on_segment(stop_coords[i], shape[j], shape[j + 1], cos_lat);
            matches.push(ShapeMatch {
                seg_idx: j,
                t,
                proj,
                dist_sq: d,
            });
        }
    }
    let at = |i: usize, j: usize| matches[i * segs + j];

    let mut dp = vec![f64::MAX; segs];
    let mut backtrack = vec![vec![0usize; segs]; n];

    // Base case: stop 0
    for j in 0..segs {
        dp[j] = at(0, j).dist_sq;
    }

    // Fill remaining stops
    for i in 1..n {
        let mut new_dp = vec![f64::MAX; segs];
        let mut min_prev = f64::MAX;
        let mut argmin_prev = 0;

        for j in 0..segs {
            if min_prev < f64::MAX {
                new_dp[j] = at(i, j).dist_sq + min_prev;
                backtrack[i][j] = argmin_prev;
            }
            if dp[j] < min_prev {
                min_prev = dp[j];
                argmin_prev = j;
            }
        }
        dp = new_dp;
    }

    // Find best final assignment
    let mut best_j = 0;
    let mut best_cost = f64::MAX;
    for (j, &cost) in dp.iter().enumerate() {
        if cost < best_cost {
            best_cost = cost;
            best_j = j;
        }
    }
    if best_cost == f64::MAX {
        return None;
    }

    // Backtrack
    let mut picks = vec![0usize; n];
    picks[n - 1] = best_j;
    for i in (1..n).rev() {
        picks[i - 1] = backtrack[i][picks[i]];
    }
    let result = (0..n).map(|i| at(i, picks[i])).collect();
    Some((best_cost, result))
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

    let mut cities: Vec<(String, CityConfig, PathBuf)> = Vec::new();
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

    for (id, config, city_path) in &cities {
        let bin_path = output_dir.join(format!("{}.bin", id));
        let has_stale_feed = config.feed_ids.iter().any(|f| stale_feeds.contains(f));
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

    // ── Stage 4: Download stale GTFS feeds + OSM data ──
    eprintln!("\n=== Stage 4: Download data ===");

    use rayon::prelude::*;

    // Only download feeds needed by cities we're rebuilding
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

    // Download OSM data only if not already cached (OSM data is stable),
    // then build city .bin files. Combined into one parallel pass so that
    // fetch_osm returns the actual cache path (which varies by source).
    eprintln!("\n=== Stage 5: Build city .bin files ===");

    std::fs::create_dir_all(output_dir)?;

    cities
        .par_iter()
        .filter(|(id, _, _)| cities_to_rebuild.contains(id))
        .try_for_each(|(id, config, _)| -> Result<()> {
            let bbox = parse_bbox(&config.bbox)?;

            // fetch_osm handles caching internally and returns the actual path
            let osm_path = osm::fetch_osm(
                bbox,
                cache_dir,
                id,
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
            run_prep(
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

    // Collect expected cache filenames
    let mut expected_files: HashSet<PathBuf> = HashSet::new();

    // GTFS zips and sha1 sidecars for all active feeds
    for feed_id in feed_to_cities.keys() {
        expected_files.insert(gtfs_cache_path(feed_id, cache_dir));
        expected_files.insert(gtfs_sha1_path(feed_id, cache_dir));
    }

    // OSM files for all active cities — include all possible naming patterns
    // since the actual filename depends on which download path was used
    for (id, config, _) in &cities {
        let sanitized = osm::sanitize(id);
        expected_files.insert(cache_dir.join(format!("{}.osm.pbf", sanitized)));
        expected_files.insert(cache_dir.join(format!("{}.osm.xml", sanitized)));
        if let Ok((min_lon, min_lat, max_lon, max_lat)) = parse_bbox(&config.bbox) {
            expected_files.insert(cache_dir.join(format!(
                "osm_{:.4}_{:.4}_{:.4}_{:.4}.xml",
                min_lon, min_lat, max_lon, max_lat
            )));
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

fn cmd_prep(city_file: &Path, output: &Path, cache_dir: &Path) -> Result<()> {
    let city_json = std::fs::read_to_string(city_file)
        .with_context(|| format!("Failed to read city file: {:?}", city_file))?;
    let city: CityConfig = parse_to_serde_value(&city_json, &Default::default())
        .with_context(|| format!("Failed to parse city file: {:?}", city_file))?;
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

    run_prep(
        &city.id,
        &gtfs_paths,
        &osm_path,
        bbox,
        output,
        city.allow_stale,
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
    allow_stale: Option<bool>,
) -> Result<()> {
    eprintln!("=== Transit Prep for '{}' ===", city);
    eprintln!("Bounding box: {:?}", bbox);

    // Step 1: Parse GTFS data — parse each feed in parallel, then merge sequentially.
    // Merge order must match gtfs_paths order (the ID prefix is derived from self.stops.len()
    // at merge time), and par_iter on a slice collects in input order.
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

    // Filter stops to bbox and re-index sequentially so out-of-bbox stops occupy no ids.
    // Also remap stop_times to the new stop indices (dropping entries for out-of-bbox stops).
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    gtfs_data
        .stops
        .retain(|s| s.lat >= min_lat && s.lat <= max_lat && s.lon >= min_lon && s.lon <= max_lon);
    // Build remap from old parse-time index to new compact index before overwriting stop.index.
    let stop_index_remap: HashMap<u32, u32> = gtfs_data
        .stops
        .iter()
        .enumerate()
        .map(|(new_idx, stop)| (stop.index, new_idx as u32))
        .collect();
    for (i, stop) in gtfs_data.stops.iter_mut().enumerate() {
        stop.index = i as u32;
    }
    // Filter stop_times to only in-bbox stops and update their stop_index to the new compact value.
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

    // Identify trips with ≥2 in-bbox stops; trips with 0-1 are useless (no edges).
    // stop_times are already filtered to in-bbox stops, so no extra check needed.
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

    // Step 3b: Prune nodes unreachable from any transit stop
    let stop_to_node = graph::prune_unreachable_nodes(&mut osm_graph, stop_to_node);

    // Step 3c: Drop stop_times rows for stops that failed to snap/prune.
    // Patterns built from the remaining rows will naturally skip these stops;
    // through travel times stay correct because GTFS stores per-stop arrival/
    // departure, so a trip A → B* → C with B* dropped becomes A → C with the
    // unchanged (arrival_C − departure_A) gap. This enforces the runtime
    // invariant that every stop referenced by a pattern has a node mapping
    // (see stop_to_node check in transit-router::profile::node_for_stop).
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

    // Step 4: Build service patterns and event arrays
    // Sort stop_times by (trip_index, stop_sequence) so build_service_patterns
    // can use binary-search slices instead of a HashMap, avoiding 2× peak memory.
    gtfs_data
        .stop_times
        .sort_unstable_by_key(|st| (st.trip_index, st.stop_sequence));
    eprintln!("\n--- Building service patterns ---");
    let mut patterns = gtfs::build_service_patterns(&gtfs_data);
    eprintln!("Built {} service patterns", patterns.len());

    // Compact route ids: collect used route indices, remap to 0..N, drop unused routes
    let mut used_route_indices: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for pattern in &patterns {
        for (_, event) in &pattern.events {
            used_route_indices.insert(event.route_index);
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

    // Compact stop ids: collect used stop indices, remap to 0..N, drop orphaned stops
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
    let stop_remap: HashMap<u32, u32> = used_stop_indices
        .iter()
        .enumerate()
        .map(|(new_idx, &old_idx)| (old_idx, new_idx as u32))
        .collect();
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
    // Remap stop_times to the new compacted indices so step 6b can index gtfs_data.stops
    // directly. Drop stop_times for stops not referenced in any pattern event.
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

    // Build compacted route arrays
    let mut route_names: Vec<String> = Vec::new();
    let mut route_colors: Vec<Option<gtfs::Color>> = Vec::new();
    for &old_idx in &used_route_indices {
        let route = &gtfs_data.routes[old_idx as usize];
        route_names.push(route.short_name.clone());
        route_colors.push(route.color);
    }

    // Step 6b: Build per-leg shape slices using DP subsequence matching
    eprintln!("\n--- Building leg shapes ---");
    let leg_shapes = {
        // Build route_id -> old_route_index mapping
        let route_id_to_old_idx: HashMap<&str, u32> = gtfs_data
            .routes
            .iter()
            .map(|r| (r.id.as_str(), r.index))
            .collect();

        // Build stop_times_by_trip keyed by trip_index, sorted by stop_sequence.
        // stop_times are already filtered to in-bbox stops by the earlier remap step.
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

        // Compute cos(lat) for the dataset center
        let center_lat = (min_lat + max_lat) / 2.0;
        let cos_lat = center_lat.to_radians().cos();

        // Per-trip result: the legs this trip contributes to best_legs.
        struct TripShapeResult {
            had_shape: bool,
            legs: Option<Vec<((u32, u32, u32), (f64, Vec<(f64, f64)>))>>,
        }

        type LegMap = HashMap<(u32, u32, u32), (f64, Vec<(f64, f64)>)>;

        // Merge two LegMaps, keeping the better (lower quality score) entry per key.
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

        // Process trips in parallel — match_stops_to_shape is a pure function.
        // All captured data is immutable (gtfs_data, stop_times_by_trip, etc. are all read-only).
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
                        }
                    }
                };
                let shape = match gtfs_data.shapes.get(shape_id) {
                    Some(pts) if pts.len() >= 2 => pts,
                    _ => {
                        return TripShapeResult {
                            had_shape: false,
                            legs: None,
                        }
                    }
                };
                let times = match stop_times_by_trip.get(&trip_idx) {
                    Some(t) if t.len() >= 2 => t,
                    _ => {
                        return TripShapeResult {
                            had_shape: false,
                            legs: None,
                        }
                    }
                };
                let old_route_idx = match route_id_to_old_idx.get(trip.route_id.as_str()) {
                    Some(&idx) => idx,
                    None => {
                        return TripShapeResult {
                            had_shape: false,
                            legs: None,
                        }
                    }
                };
                let new_route_idx = match route_remap.get(&old_route_idx) {
                    Some(&idx) => idx,
                    None => {
                        return TripShapeResult {
                            had_shape: false,
                            legs: None,
                        }
                    }
                };

                // Collect stop coords; stop_index is already the compacted index.
                let stop_coords: Vec<(f64, f64)> = times
                    .iter()
                    .map(|st| {
                        let stop = &gtfs_data.stops[st.stop_index as usize];
                        (stop.lat, stop.lon)
                    })
                    .collect();

                // DP subsequence matching (point-to-segment)
                let shape_matches = match match_stops_to_shape(&stop_coords, shape, cos_lat) {
                    Some(m) => m,
                    None => {
                        return TripShapeResult {
                            had_shape: true,
                            legs: None,
                        }
                    }
                };

                // Extract leg slices for each consecutive stop pair
                let mut legs = Vec::new();
                for w in 0..times.len() - 1 {
                    let from_stop = times[w].stop_index;
                    let to_stop = times[w + 1].stop_index;
                    let key = (new_route_idx, from_stop, to_stop);

                    let mf = shape_matches[w];
                    let mt = shape_matches[w + 1];

                    // Quality = max squared distance of the two stops to their
                    // projected points on the shape.
                    let quality = mf.dist_sq.max(mt.dist_sq);

                    // Build the leg polyline by splitting the shape at each
                    // stop's projected point: [proj_from] + intermediate shape
                    // vertices strictly between the two projections + [proj_to].
                    let forward = (mf.seg_idx, mf.t) <= (mt.seg_idx, mt.t);
                    let span = mf.seg_idx.abs_diff(mt.seg_idx);
                    let mut leg_points = Vec::with_capacity(span + 2);
                    leg_points.push(mf.proj);
                    if forward {
                        // Vertices shape[mf.seg_idx + 1 ..= mt.seg_idx] lie between
                        // the projection on segment mf.seg_idx (ends at shape[mf.seg_idx + 1])
                        // and the projection on segment mt.seg_idx (starts at shape[mt.seg_idx]).
                        if mf.seg_idx + 1 <= mt.seg_idx {
                            leg_points.extend_from_slice(&shape[mf.seg_idx + 1..=mt.seg_idx]);
                        }
                    } else {
                        // Reverse: walk shape backwards from mf.seg_idx down to mt.seg_idx + 1.
                        if mt.seg_idx + 1 <= mf.seg_idx {
                            leg_points
                                .extend(shape[mt.seg_idx + 1..=mf.seg_idx].iter().rev().copied());
                        }
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

        // Merge per-trip leg contributions into best_legs using fold+reduce —
        // each thread builds its own LegMap then they are merged pairwise.
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

        // Sort by key for binary search at query time
        let mut leg_shapes: Vec<((u32, u32, u32), Vec<(f64, f64)>)> = best_legs
            .into_iter()
            .map(|(k, (_, pts))| (k, pts))
            .collect();
        leg_shapes.sort_by_key(|&(k, _)| k);
        leg_shapes
    };

    // Step 7: Serialize to binary
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
