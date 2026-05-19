//! GTFS-zip download and cache layout. Handles two feed-id forms:
//!   * Transitland onestop IDs (`f-...`) — header-auth, with a SHA1 sidecar
//!     in `cache_dir/sha1/` for fresh-enough caching.
//!   * Direct URLs — cached forever by URL hash.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::osm_fetch::url_hash;
use crate::transitland;

/// How long a Transitland sha1 sidecar is considered fresh enough to skip
/// the upstream feed-version check. Bounds the worst-case staleness of a
/// city build between two pipeline runs.
const SHA1_CACHE_FRESHNESS: std::time::Duration = std::time::Duration::from_secs(2 * 24 * 3600);

pub fn is_transitland_id(feed_id: &str) -> bool {
    feed_id.starts_with("f-")
}

pub fn gtfs_cache_path(feed_id: &str, cache_dir: &Path) -> PathBuf {
    if is_transitland_id(feed_id) {
        cache_dir.join(format!("{}.gtfs.zip", feed_id))
    } else {
        cache_dir.join(format!("url_{:016x}.gtfs.zip", url_hash(feed_id)))
    }
}

pub fn gtfs_sha1_path(feed_id: &str, cache_dir: &Path) -> PathBuf {
    if is_transitland_id(feed_id) {
        cache_dir.join("sha1").join(format!("{}.sha1", feed_id))
    } else {
        cache_dir
            .join("sha1")
            .join(format!("url_{:016x}.sha1", url_hash(feed_id)))
    }
}

/// Download a GTFS feed (Transitland or direct URL) into the cache directory.
pub fn fetch_gtfs(feed_id: &str, api_key: Option<&str>, cache_dir: &Path) -> Result<PathBuf> {
    let cache_path = gtfs_cache_path(feed_id, cache_dir);
    let sha1_path = gtfs_sha1_path(feed_id, cache_dir);

    if cache_path.exists() && !is_transitland_id(feed_id) {
        eprintln!("Using cached GTFS: {:?}", cache_path);
        return Ok(cache_path);
    }

    if is_transitland_id(feed_id) {
        let key =
            api_key.with_context(|| format!("Feed '{}' requires TRANSITLAND_API_KEY", feed_id))?;

        if cache_path.exists() {
            if sha1_recently_checked(&sha1_path) {
                eprintln!("Using cached GTFS (checked recently): {:?}", cache_path);
                return Ok(cache_path);
            }
            let local_sha1 = std::fs::read_to_string(&sha1_path).unwrap_or_default();
            match transitland::latest_feed_sha1(key, feed_id) {
                Ok(Some(remote_sha1)) if !local_sha1.is_empty() && local_sha1 == remote_sha1 => {
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

pub fn sha1_recently_checked(sha1_path: &Path) -> bool {
    std::fs::metadata(sha1_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|mtime| mtime.elapsed().unwrap_or_default() < SHA1_CACHE_FRESHNESS)
        .unwrap_or(false)
}

/// Validate a feed identifier (URL or Transitland onestop ID). Doesn't fetch.
pub fn validate_feed_id(feed_id: &str, api_key: Option<&str>) -> Result<()> {
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
