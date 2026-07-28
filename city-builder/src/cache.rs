//! Age-based cache freshness — the fallback when content checks don't apply.
//!
//! Most downloads can be validated against upstream without transferring the
//! body: Transitland feeds carry a sha1, and every HTTP source we use exposes
//! an ETag (see [`crate::http_cache`]). Age only decides things when no such
//! handle is available, plus it bounds how long OSM extracts may go unchecked.

use std::path::Path;
use std::time::Duration;

/// Cached artifacts that can't be validated against upstream are re-downloaded
/// once they reach this age.
pub const MAX_CACHE_AGE: Duration = Duration::from_secs(15 * 24 * 3600);

/// How stale an OSM extract may get before we go back to the network. Base map
/// geometry moves slowly and the extracts are huge (1.7 GB across our cities),
/// so within this window a cached extract is used without even a `HEAD`; past
/// it we check validators and re-download only on an actual change.
pub const OSM_MAX_STALENESS: Duration = Duration::from_secs(30 * 24 * 3600);

/// Time since `path` was last modified, or `None` if it doesn't exist or its
/// mtime can't be read.
pub fn age(path: &Path) -> Option<Duration> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(mtime.elapsed().unwrap_or_default())
}

/// True when `path` exists and is younger than `max_age`.
pub fn is_fresh(path: &Path, max_age: Duration) -> bool {
    age(path).is_some_and(|a| a < max_age)
}

/// True when `path` exists but has aged past [`MAX_CACHE_AGE`]. A missing file
/// is *not* expired — callers distinguish "never downloaded" from "too old".
pub fn is_expired(path: &Path) -> bool {
    age(path).is_some_and(|a| a >= MAX_CACHE_AGE)
}

/// True when `path` can be used as-is: present and not aged out.
pub fn is_usable(path: &Path) -> bool {
    is_fresh(path, MAX_CACHE_AGE)
}

/// Whole days since `path` was last modified, for log messages.
pub fn age_days(path: &Path) -> u64 {
    age(path).map(|a| a.as_secs() / 86_400).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// Write `name` into a temp dir and backdate its mtime by `days`.
    fn aged_file(dir: &Path, name: &str, days: u64) -> std::path::PathBuf {
        let path = dir.join(name);
        let f = std::fs::File::create(&path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(days * 86_400))
            .unwrap();
        path
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("city-builder-cache-test-{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_is_neither_fresh_nor_expired() {
        let dir = temp_dir("missing");
        let path = dir.join("nope.gtfs.zip");
        assert!(age(&path).is_none());
        assert!(!is_expired(&path));
        assert!(!is_usable(&path));
    }

    #[test]
    fn young_file_is_usable() {
        let dir = temp_dir("young");
        let path = aged_file(&dir, "fresh.osm.pbf", 14);
        assert!(is_usable(&path));
        assert!(!is_expired(&path));
        assert_eq!(age_days(&path), 14);
    }

    #[test]
    fn file_past_max_age_is_expired() {
        let dir = temp_dir("old");
        let path = aged_file(&dir, "stale.osm.pbf", 16);
        assert!(is_expired(&path));
        assert!(!is_usable(&path));
        assert_eq!(age_days(&path), 16);
    }

    #[test]
    fn is_fresh_honours_a_custom_window() {
        let dir = temp_dir("window");
        let path = aged_file(&dir, "sidecar.sha1", 3);
        assert!(!is_fresh(&path, Duration::from_secs(2 * 86_400)));
        assert!(is_fresh(&path, Duration::from_secs(4 * 86_400)));
    }
}
