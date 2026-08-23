//! GTFS-zip download and cache layout. Handles two feed-id forms:
//!   * Transitland onestop IDs (`f-...`) — header-auth, with a SHA1 sidecar
//!     in `cache_dir/sha1/` for fresh-enough caching.
//!   * Direct URLs — cached by URL hash and validated against the origin's
//!     ETag on every run (see [`crate::http_cache`]), so an unchanged feed
//!     costs one `HEAD` instead of a download.
//!
//! Feeds are re-downloaded only when their content actually changed; the age
//! rule in [`crate::cache`] is a fallback for sources that stop answering with
//! validators.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::cache;
use crate::http_cache;
use crate::osm_fetch::url_hash;
use crate::transitland;

/// How long a Transitland sha1 sidecar is considered fresh enough to skip
/// the upstream feed-version check. Bounds the worst-case staleness of a
/// city build between two pipeline runs.
const SHA1_CACHE_FRESHNESS: std::time::Duration = std::time::Duration::from_secs(2 * 24 * 3600);

/// GTFS zips are small next to OSM extracts; keep the original tighter budget.
const GTFS_DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

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
                // No hash to compare against — fall back to the age rule so an
                // unverifiable feed doesn't stay pinned to the cache forever.
                Ok(None) if cache::is_usable(&cache_path) => {
                    eprintln!(
                        "Using cached GTFS (no remote sha1 to compare): {:?}",
                        cache_path
                    );
                    return Ok(cache_path);
                }
                Ok(None) => {
                    eprintln!(
                        "Feed '{}': no remote sha1 and cache is {} day(s) old — re-downloading",
                        feed_id,
                        cache::age_days(&cache_path)
                    );
                }
                Err(e) if cache::is_usable(&cache_path) => {
                    eprintln!("WARNING: could not check Transitland for updates: {}", e);
                    eprintln!("Using cached GTFS: {:?}", cache_path);
                    return Ok(cache_path);
                }
                Err(e) => {
                    eprintln!("WARNING: could not check Transitland for updates: {}", e);
                    eprintln!(
                        "Cached GTFS is {} day(s) old — re-downloading: {:?}",
                        cache::age_days(&cache_path),
                        cache_path
                    );
                }
            }
        }

        eprintln!("Downloading GTFS from Transitland: {}", feed_id);
        let bytes = transitland::download_feed(key, feed_id)
            .with_context(|| format!("Failed to fetch GTFS feed '{}'", feed_id))?;
        http_cache::write_atomic(&cache_path, &bytes)?;

        if let Ok(Some(sha1)) = transitland::latest_feed_sha1(key, feed_id) {
            let _ = std::fs::write(&sha1_path, &sha1);
        }

        Ok(cache_path)
    } else {
        // Direct URL: validated against the origin's ETag on every run, so the
        // age rule only decides things when validators are unavailable.
        let client = http_cache::client(http_cache::CHECK_TIMEOUT)?;
        if http_cache::check(
            &client,
            feed_id,
            &cache_path,
            cache::MAX_CACHE_AGE,
            &format!("Feed '{}'", feed_id),
        ) == http_cache::CacheState::Current
        {
            eprintln!("Using cached GTFS: {:?}", cache_path);
            return Ok(cache_path);
        }

        eprintln!("Downloading GTFS from: {}", feed_id);
        let client = http_cache::client(GTFS_DOWNLOAD_TIMEOUT)?;
        http_cache::download_or_cached(
            &client,
            feed_id,
            feed_id,
            &cache_path,
            cache::MAX_CACHE_AGE,
            "GTFS",
        )
    }
}

pub fn sha1_recently_checked(sha1_path: &Path) -> bool {
    cache::is_fresh(sha1_path, SHA1_CACHE_FRESHNESS)
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

#[cfg(test)]
mod tests {
    /// Full round trip against a live origin: download, record validators,
    /// then confirm the second call validates instead of re-downloading.
    /// Uses a 12 KB feed. Run with `cargo test -p city-builder -- --ignored`.
    #[test]
    #[ignore = "requires network"]
    fn direct_url_feed_downloads_then_validates() {
        let dir = std::env::temp_dir().join("city-builder-gtfs-net-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let url =
            "https://api.gtfs-data.jp/v2/organizations/arakawacity/feeds/sakura/files/feed.zip";

        let path = super::fetch_gtfs(url, None, &dir).unwrap();
        let downloaded = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 0);
        assert!(
            !crate::http_cache::load(&path).is_empty(),
            "download must record the origin's validators"
        );

        let again = super::fetch_gtfs(url, None, &dir).unwrap();
        assert_eq!(
            std::fs::metadata(&again).unwrap().modified().unwrap(),
            downloaded,
            "unchanged feed must not be re-downloaded"
        );
    }
}
