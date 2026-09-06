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
mod metadata;
mod osm_fetch;
mod transitland;

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::CityConfig;
use crate::gtfs_fetch::{
    fetch_gtfs, gtfs_cache_path, gtfs_sha1_path, is_transitland_id, sha1_file,
    sha1_recently_checked, validate_feed_id,
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

/// Identity of the GTFS zip currently in the cache for `feed_id`: the sha1
/// sidecar for Transitland feeds, the ETag sidecar for direct URLs.
fn on_disk_feed_identity(feed_id: &str, cache_dir: &Path) -> metadata::SourceId {
    if is_transitland_id(feed_id) {
        std::fs::read_to_string(gtfs_sha1_path(feed_id, cache_dir))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(metadata::SourceId::from_sha1)
            .unwrap_or_default()
    } else {
        metadata::SourceId::from(&http_cache::load(&gtfs_cache_path(feed_id, cache_dir)))
    }
}

/// A city config as loaded by the pipeline, with the hash of its file bytes
/// that the build record compares against.
struct City {
    id: String,
    config: CityConfig,
    config_hash: String,
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
        .map(|fid| fetch_gtfs(fid, api_key.as_deref(), cache_dir, false).map(|f| f.path))
        .collect::<Result<Vec<_>>>()?;

    let osm_path = osm_fetch::fetch_osm(
        bbox,
        cache_dir,
        &city.id,
        city.interline_extract.as_deref(),
        city.bbbike_name.as_deref(),
        city.osm_url.as_deref(),
        false,
    )?
    .path;

    transit_prep::prepare(
        &city.id,
        &gtfs_paths,
        &osm_path,
        bbox,
        output,
        city.allow_stale,
    )
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

    let mut cities: Vec<City> = Vec::new();
    let mut feed_ids: BTreeSet<String> = BTreeSet::new();

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
            feed_ids.insert(fid.clone());
        }

        let config_hash = sha1_file(&path)?;
        cities.push(City {
            id: config.id.clone(),
            config,
            config_hash,
        });
    }

    let tl_feeds: Vec<_> = feed_ids
        .iter()
        .filter(|f| is_transitland_id(f))
        .cloned()
        .collect();

    eprintln!(
        "  {} cities, {} unique feeds ({} Transitland)",
        cities.len(),
        feed_ids.len(),
        tl_feeds.len()
    );

    // ── Stage 2: Probe upstream source identities ──
    //
    // One probe per feed, answering only "what does the origin call this right
    // now?". Two independent decisions are derived from it:
    //   * `remote_feeds` vs. `metadata.json` → does a `.bin` need rebuilding
    //     (stage 3). Payload-independent, so it works from a cold cache.
    //   * `remote_feeds` vs. the local sidecar → does the *download* need
    //     refetching (`stale_feeds`, stage 4).
    // Conflating the two is what made this undecidable in CI, where the outputs
    // are restored but `cache/` is not.
    eprintln!("\n=== Stage 2: Probe upstream source identities ===");

    let mut remote_feeds: HashMap<String, metadata::SourceId> = HashMap::new();
    let mut stale_feeds: HashSet<String> = HashSet::new();

    // One shared TLS client and a small pool: 166 sequential probes, each on
    // a fresh connection, took minutes; the API is happy with a handful of
    // concurrent requests. Results are collected and merged afterwards.
    let tl_probes: Vec<(&String, Option<String>, bool)> = {
        let client = http_cache::client(transitland::API_TIMEOUT)?;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .context("failed to build probe thread pool")?;
        pool.install(|| {
            tl_feeds
                .par_iter()
                .map(|feed_id| {
                    let sha1_path = gtfs_sha1_path(feed_id, cache_dir);
                    let local_sha1 = std::fs::read_to_string(&sha1_path).unwrap_or_default();

                    // The sidecar holds the last sha1 the API reported; inside the
                    // freshness window reuse it rather than re-querying. It is still a
                    // usable probe result — it *is* what upstream last told us.
                    if sha1_recently_checked(&sha1_path) && !local_sha1.is_empty() {
                        eprintln!("  {}: fresh (checked recently)", feed_id);
                        return (feed_id, Some(local_sha1), false);
                    }
                    let key = api_key.as_deref().unwrap(); // validated in stage 1
                    match transitland::latest_feed_sha1(&client, key, feed_id) {
                        Ok(Some(remote)) => {
                            let stale = if local_sha1.is_empty() {
                                eprintln!("  {}: no local sha1 → cache stale", feed_id);
                                true
                            } else if remote != local_sha1 {
                                eprintln!("  {}: sha1 changed → cache stale", feed_id);
                                true
                            } else {
                                eprintln!("  {}: up to date", feed_id);
                                // Refresh the sidecar's mtime so the freshness window
                                // restarts. Only when it matches: the sidecar must
                                // always describe the zip on disk. Writing the remote
                                // hash for a *stale* feed made stage 4's fetch take the
                                // "checked recently" shortcut and keep the old zip,
                                // which is how cities shipped months-old feeds while
                                // the build record claimed they were current.
                                let _ = std::fs::write(&sha1_path, &remote);
                                false
                            };
                            (feed_id, Some(remote), stale)
                        }
                        // Unverifiable. Deliberately *not* falling back to `local_sha1`
                        // as the probe result: that would compare equal to whatever
                        // built the .bin and silently pin it forever. Leaving it empty
                        // routes the city to the built_at age rule in stage 3 instead.
                        Ok(None) => {
                            eprintln!("  {}: no remote sha1 available", feed_id);
                            (feed_id, None, false)
                        }
                        Err(e) => {
                            eprintln!("  WARNING: {}: {}", feed_id, e);
                            (feed_id, None, false)
                        }
                    }
                })
                .collect()
        })
    };
    for (feed_id, remote_sha1, stale) in tl_probes {
        if stale {
            stale_feeds.insert(feed_id.clone());
        }
        remote_feeds.insert(
            feed_id.clone(),
            remote_sha1
                .map(metadata::SourceId::from_sha1)
                .unwrap_or_default(),
        );
    }

    // Direct-URL feeds: one HEAD each, in parallel — header-only round trips.
    let url_feeds: Vec<&String> = feed_ids.iter().filter(|f| !is_transitland_id(f)).collect();

    if !url_feeds.is_empty() {
        let client = http_cache::client(http_cache::CHECK_TIMEOUT)?;
        let probed: Vec<(&String, metadata::SourceId, bool)> = url_feeds
            .par_iter()
            .map(|feed_id| {
                let remote = match http_cache::head(&client, feed_id) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("  WARNING: {}: could not probe upstream: {}", feed_id, e);
                        http_cache::Validators::default()
                    }
                };
                let remote = metadata::SourceId::from(&remote);

                // Does the *downloaded file* still match? Absent payload counts
                // as stale here but says nothing about the .bin.
                let path = gtfs_cache_path(feed_id, cache_dir);
                let local = metadata::SourceId::from(&http_cache::load(&path));
                let cache_current = path.exists() && local.same_as(&remote) == Some(true);
                (*feed_id, remote, cache_current)
            })
            .collect();

        for (feed_id, remote, cache_current) in probed {
            if remote.is_empty() {
                eprintln!("  {}: origin reports no validators", feed_id);
            }
            if !cache_current {
                stale_feeds.insert(feed_id.clone());
            }
            remote_feeds.insert(feed_id.clone(), remote);
        }
    }

    // ── Stage 3: Determine what needs rebuilding ──
    eprintln!("\n=== Stage 3: Determine what needs rebuilding ===");

    // One HEAD per city's OSM source, in parallel. This used to be skipped
    // whenever the cached extract had been verified within OSM_MAX_STALENESS,
    // which made it unrunnable without the extract on disk. Comparing against
    // the build record needs no payload, and 23 header-only requests are
    // cheap enough that the staleness window buys nothing here — it still
    // governs the download cache in `osm_fetch`.
    let remote_osm: HashMap<&str, metadata::SourceId> = {
        let client = http_cache::client(http_cache::CHECK_TIMEOUT)?;
        cities
            .par_iter()
            .map(|City { id, config, .. }| {
                let source_url = osm_fetch::osm_request_url(
                    config.interline_extract.as_deref(),
                    config.bbbike_name.as_deref(),
                    config.osm_url.as_deref(),
                );
                // `None` = Overpass fallback (a POST query with no validators),
                // or an Interline city with no API key configured.
                let sid = match source_url {
                    Some(url) => match http_cache::head(&client, &url) {
                        Ok(v) => metadata::SourceId::from(&v),
                        Err(e) => {
                            // Already URL-redacted by `http_cache` — the
                            // Interline URL carries the API token.
                            eprintln!("  WARNING: {} osm: could not probe upstream: {}", id, e);
                            metadata::SourceId::default()
                        }
                    },
                    None => metadata::SourceId::default(),
                };
                (id.as_str(), sid)
            })
            .collect()
    };

    let recorded = metadata::Metadata::load(output_dir);

    let mut cities_to_rebuild: Vec<String> = Vec::new();

    for City {
        id,
        config,
        config_hash,
        ..
    } in &cities
    {
        let bin_path = output_dir.join(format!("{}.bin", id));
        let bin_missing = !bin_path.exists();

        let prior = recorded.cities.get(id);
        // Recorded content, not mtimes: in CI `actions/checkout` writes every
        // source and config file at checkout time while the restored `.bin`
        // keeps its original mtime, so an mtime comparison reported "config
        // changed" for every city on every scheduled run.
        let code_changed = prior.is_some_and(|p| p.code_fingerprint != metadata::CODE_FINGERPRINT);
        let config_changed = prior.is_some_and(|p| &p.config_hash != config_hash);

        // Compare every input against the identity that produced this .bin.
        // `same_as` returning None means "not comparable" — treated as
        // unverifiable, never as unchanged.
        let mut changed_input: Option<String> = None;
        let mut unverifiable = false;
        let mut feed_set_changed = false;

        if let Some(prior) = prior {
            feed_set_changed = prior.feeds.len() != config.feed_ids.len()
                || config.feed_ids.iter().any(|f| !prior.feeds.contains_key(f));

            let osm_pair = (
                "OSM extract".to_string(),
                prior.osm.clone().unwrap_or_default(),
                remote_osm.get(id.as_str()).cloned().unwrap_or_default(),
            );
            let feed_pairs = config.feed_ids.iter().map(|fid| {
                (
                    format!("feed {}", fid),
                    prior.feeds.get(fid).cloned().unwrap_or_default(),
                    remote_feeds.get(fid).cloned().unwrap_or_default(),
                )
            });

            for (what, then, now) in feed_pairs.chain(std::iter::once(osm_pair)) {
                match then.same_as(&now) {
                    Some(true) => {}
                    Some(false) => {
                        changed_input = Some(format!("{} changed upstream", what));
                        break;
                    }
                    None => unverifiable = true,
                }
            }
        }

        // Nothing upstream could be compared, so bound how long we coast on it.
        // `built_at` is recorded content, not an mtime, so it survives the
        // archive round trips that CI's cache layers put it through.
        let stale_unverifiable = unverifiable
            && prior
                .and_then(|p| p.age())
                .is_none_or(|age| age >= cache::MAX_CACHE_AGE);

        let reason: Option<String> = if bin_missing {
            Some(".bin missing".into())
        } else if prior.is_none() {
            Some("no build metadata".into())
        } else if feed_set_changed {
            Some("feed list changed".into())
        } else if let Some(what) = changed_input {
            Some(what)
        } else if stale_unverifiable {
            Some(format!(
                "unverifiable input and build is {} day(s) old",
                prior.map(|p| p.age_days()).unwrap_or(0)
            ))
        } else if code_changed {
            Some("code changed".into())
        } else if config_changed {
            Some("config changed".into())
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
            .filter(|c| cities_to_rebuild.contains(&c.id))
            .flat_map(|c| c.config.feed_ids.iter())
            .collect();
        needed
            .into_iter()
            .filter(|fid| stale_feeds.contains(*fid) || !gtfs_cache_path(fid, cache_dir).exists())
            .collect()
    };

    // Feeds whose fetch fell back to an unverified cached copy. A city built
    // from one keeps its prior build record so the next run tries again.
    let fallback_feeds: HashSet<&String> = feeds_to_download
        .par_iter()
        .map(|feed_id| -> Result<Option<&String>> {
            // Stage 2 already decided whether this feed is stale, so skip the
            // re-probe: a sidecar refreshed within the freshness window would
            // otherwise make `fetch_gtfs` hand back the old zip.
            let fetched = fetch_gtfs(
                feed_id,
                api_key.as_deref(),
                cache_dir,
                stale_feeds.contains(*feed_id),
            )?;
            Ok(fetched.fell_back.then_some(*feed_id))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();

    // ── Stage 5: Build city .bin files (downloads OSM on demand) ──
    eprintln!("\n=== Stage 5: Build city .bin files ===");

    std::fs::create_dir_all(output_dir)?;

    // Every city runs to completion and the outcomes are collected: the
    // records of the cities that built are saved before the first error is
    // returned, so one failing city no longer discards the others' work.
    let outcomes: Vec<Result<Option<(String, metadata::CityMetadata)>>> = cities
        .par_iter()
        .filter(|c| cities_to_rebuild.contains(&c.id))
        .map(
            |City {
                 id,
                 config,
                 config_hash,
                 ..
             }|
             -> Result<Option<(String, metadata::CityMetadata)>> {
                let bbox = parse_bbox(&config.bbox)?;

                // Stage 3 compared the recorded OSM identity against the
                // origin; if either that or the local sidecar disagrees with
                // the origin, the cached extract is behind and `fetch_osm`
                // must not take its 30-day no-network shortcut. Otherwise a
                // rebuild triggered by a new ETag would build from the old
                // extract and then record the new identity, pinning the city
                // to stale data until the sidecar aged out.
                let remote = remote_osm.get(id.as_str()).cloned().unwrap_or_default();
                let osm_cache_path = osm_fetch::pick_source_url(
                    config.interline_extract.as_deref(),
                    config.bbbike_name.as_deref(),
                    config.osm_url.as_deref(),
                )
                .map(|url| {
                    osm_fetch::pbf_cache_path(
                        cache_dir,
                        id,
                        &url,
                        osm_fetch::source_ext(config.osm_url.as_deref()),
                    )
                });
                let osm_changed = recorded
                    .cities
                    .get(id)
                    .and_then(|p| p.osm.as_ref())
                    .is_some_and(|then| then.same_as(&remote) == Some(false))
                    || osm_cache_path.as_deref().is_some_and(|path| {
                        metadata::SourceId::from(&http_cache::load(path)).same_as(&remote)
                            == Some(false)
                    });

                let osm = osm_fetch::fetch_osm(
                    bbox,
                    cache_dir,
                    id,
                    config.interline_extract.as_deref(),
                    config.bbbike_name.as_deref(),
                    config.osm_url.as_deref(),
                    osm_changed,
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
                    &osm.path,
                    bbox,
                    &bin_path,
                    config.allow_stale,
                )?;

                // A fetch that fell back to an unverified cached copy may have
                // built from stale data. Keep the prior record (or none) so
                // stage 3 rebuilds the city next run instead of pinning it.
                let stale_inputs: Vec<&str> = config
                    .feed_ids
                    .iter()
                    .filter(|fid| fallback_feeds.contains(fid))
                    .map(String::as_str)
                    .chain(osm.fell_back.then_some("OSM extract"))
                    .collect();
                if !stale_inputs.is_empty() {
                    eprintln!(
                        "  {}: built from unverified cached input(s) ({}); keeping the prior build record",
                        id,
                        stale_inputs.join(", ")
                    );
                    return Ok(None);
                }

                // Record the identity of what is on disk — the files this .bin
                // was actually built from — not what stage 2/3 probed: the two
                // differ whenever a fetch kept a cached copy. Stamped only on
                // success, so a failed build leaves the old record in place.
                let feeds = config
                    .feed_ids
                    .iter()
                    .map(|fid| (fid.clone(), on_disk_feed_identity(fid, cache_dir)))
                    .collect();
                let osm_id = metadata::SourceId::from(&http_cache::load(&osm.path));
                Ok(Some((
                    id.clone(),
                    metadata::CityMetadata {
                        built_at: chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        feeds,
                        osm: Some(osm_id).filter(|s| !s.is_empty()),
                        config_hash: config_hash.clone(),
                        code_fingerprint: metadata::CODE_FINGERPRINT.to_string(),
                    },
                )))
            },
        )
        .collect();
    let mut built: Vec<(String, metadata::CityMetadata)> = Vec::new();
    let mut first_error: Option<anyhow::Error> = None;
    for outcome in outcomes {
        match outcome {
            Ok(Some(record)) => built.push(record),
            Ok(None) => {}
            Err(e) => {
                eprintln!("ERROR: {:#}", e);
                first_error.get_or_insert(e);
            }
        }
    }

    // Merge over the prior record so cities that didn't rebuild keep theirs,
    // then drop any city that no longer has a config.
    let mut updated = recorded;
    updated.cities.extend(built);
    let active: HashSet<&str> = cities.iter().map(|c| c.id.as_str()).collect();
    updated.cities.retain(|id, _| active.contains(id.as_str()));
    updated.save(output_dir)?;
    eprintln!(
        "\nRecorded build metadata for {} cities",
        updated.cities.len()
    );
    if let Some(e) = first_error {
        return Err(e.context("one or more cities failed to build"));
    }

    // ── Cleanup: Remove orphaned cache files ──
    eprintln!("\n=== Cleanup: Remove orphaned cache files ===");

    let mut expected_files: HashSet<PathBuf> = HashSet::new();

    for feed_id in &feed_ids {
        expected_files.insert(gtfs_cache_path(feed_id, cache_dir));
        expected_files.insert(gtfs_sha1_path(feed_id, cache_dir));
    }

    for City { id, config, .. } in &cities {
        if let Some(url) = osm_fetch::pick_source_url(
            config.interline_extract.as_deref(),
            config.bbbike_name.as_deref(),
            config.osm_url.as_deref(),
        ) {
            expected_files.insert(osm_fetch::pbf_cache_path(
                cache_dir,
                id,
                &url,
                osm_fetch::source_ext(config.osm_url.as_deref()),
            ));
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

    let active_city_ids: HashSet<&str> = cities.iter().map(|c| c.id.as_str()).collect();
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
        false,
    )?
    .path;

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
    let client = http_cache::client(transitland::API_TIMEOUT)?;
    let feeds = transitland::query_feeds_in_bbox(&client, &api_key, bbox)?;

    eprintln!("\n--- Querying Transitland for operators ---");
    let op_map = match transitland::query_operators_in_bbox(&client, &api_key, bbox) {
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
