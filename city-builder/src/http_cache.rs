//! Conditional-request caching for downloads that expose HTTP validators.
//!
//! Every OSM and direct-URL GTFS source we use answers a `HEAD` with an
//! `ETag` (and usually `Last-Modified`), and honours conditional requests
//! through redirects. We record whatever the origin reported in a sidecar next
//! to the cached file — `cache/etag/<file name>.etag` — so deciding whether a
//! 200 MB extract changed costs one `HEAD` instead of a full download.
//!
//! Validator formats vary wildly across our sources: plain MD5, S3 multipart
//! `<md5>-<parts>`, Azure `0x8DE…`, Apache inode triples, BBBike decimals.
//! None of them is recomputable from bytes on disk in the general case, so a
//! validator is treated as an **opaque token** — we only ever compare what we
//! stored against what the origin now reports.
//!
//! The sidecar's mtime doubles as "when we last confirmed this entry", which
//! is what lets OSM extracts skip the network entirely for
//! [`crate::cache::OSM_MAX_STALENESS`].

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{ETAG, HeaderMap, LAST_MODIFIED};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cache;

const USER_AGENT: &str = "Mozilla/5.0 (compatible; transit-prep/1.0)";

/// Timeout for validator checks — these are header-only round trips.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// Timeout for full downloads (some OSM extracts are hundreds of MB).
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

pub fn client(timeout: Duration) -> Result<Client> {
    Client::builder()
        .timeout(timeout)
        .user_agent(USER_AGENT)
        .build()
        .context("failed to build HTTP client")
}

/// What the origin reports about a resource's identity.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Validators {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let get = |name: reqwest::header::HeaderName| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        Self {
            etag: get(ETAG),
            last_modified: get(LAST_MODIFIED),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }
}

/// Compare a stored validator against a freshly reported one. `None` when
/// there's nothing comparable — no sidecar yet, or the origin switched which
/// validator it exposes. Shares the comparison ladder with the persisted
/// build record so the cache layer and the rebuild decision can't drift apart.
fn same_resource(local: &Validators, remote: &Validators) -> Option<bool> {
    crate::metadata::SourceId::from(local).same_as(&crate::metadata::SourceId::from(remote))
}

/// `<cache dir>/etag/<cache file name>.etag`.
pub fn sidecar_path(cache_path: &Path) -> PathBuf {
    let dir = cache_path.parent().unwrap_or_else(|| Path::new("."));
    let name = cache_path.file_name().unwrap_or_default().to_string_lossy();
    dir.join("etag").join(format!("{}.etag", name))
}

pub fn load(cache_path: &Path) -> Validators {
    let text = std::fs::read_to_string(sidecar_path(cache_path)).unwrap_or_default();
    let mut v = Validators::default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("etag: ") {
            v.etag = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("last-modified: ") {
            v.last_modified = Some(rest.trim().to_string());
        }
    }
    v
}

/// Record `validators` for `cache_path`. Always rewrites the sidecar, even
/// when nothing changed: its mtime is the "last confirmed current" stamp.
pub fn store(cache_path: &Path, validators: &Validators) {
    let path = sidecar_path(cache_path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut out = String::new();
    if let Some(etag) = &validators.etag {
        out.push_str(&format!("etag: {}\n", etag));
    }
    if let Some(lm) = &validators.last_modified {
        out.push_str(&format!("last-modified: {}\n", lm));
    }
    let _ = std::fs::write(path, out);
}

/// Time since we last confirmed this entry against the origin — the sidecar's
/// mtime, which [`store`] refreshes on every successful check. Entries that
/// predate sidecars fall back to the cached file's own mtime (its download
/// time). `None` if nothing is cached.
pub fn checked_age(cache_path: &Path) -> Option<Duration> {
    cache::age(&sidecar_path(cache_path)).or_else(|| cache::age(cache_path))
}

/// True when we confirmed this entry was current within `window`.
pub fn checked_within(cache_path: &Path, window: Duration) -> bool {
    checked_age(cache_path).is_some_and(|a| a < window)
}

/// Days since we last confirmed this entry, for log messages.
pub fn checked_days_ago(cache_path: &Path) -> u64 {
    checked_age(cache_path)
        .map(|a| a.as_secs() / 86_400)
        .unwrap_or(0)
}

/// Drop the request URL from a `reqwest` error before it can reach a log.
///
/// `reqwest`'s `Display` appends `for url (<the full url>)`, and the Interline
/// OSM extract URL carries the API token as a query parameter — so an ordinary
/// connect/timeout/4xx failure would otherwise print the secret. Every error
/// leaving this module goes through here, rather than relying on each call site
/// to remember; callers add their own redacted URL via `.context(…)`.
fn redact(e: reqwest::Error) -> anyhow::Error {
    anyhow::Error::new(e.without_url())
}

/// Ask the origin for a resource's current validators.
pub fn head(client: &Client, url: &str) -> Result<Validators> {
    let resp = client
        .head(url)
        .send()
        .map_err(redact)?
        .error_for_status()
        .map_err(redact)?;
    Ok(Validators::from_headers(resp.headers()))
}

/// Fetch `url` in full, returning the body and the validators to record.
pub fn download(client: &Client, url: &str) -> Result<(Vec<u8>, Validators)> {
    let resp = client
        .get(url)
        .send()
        .map_err(redact)?
        .error_for_status()
        .map_err(redact)?;
    let validators = Validators::from_headers(resp.headers());
    let bytes = resp.bytes().map_err(redact)?.to_vec();
    Ok((bytes, validators))
}

/// Write `bytes` to `cache_path` atomically, so an interrupted re-download
/// can't leave a truncated file where a valid cached one used to be.
pub fn write_atomic(cache_path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = cache_path.with_extension("tmp");
    std::fs::File::create(&tmp)?.write_all(bytes)?;
    std::fs::rename(&tmp, cache_path)?;
    Ok(())
}

/// Store a downloaded body and its validators together.
pub fn save(cache_path: &Path, bytes: &[u8], validators: &Validators) -> Result<()> {
    write_atomic(cache_path, bytes)?;
    store(cache_path, validators);
    Ok(())
}

/// Download `url` into `cache_path`, or — when the download fails but a cached
/// copy was confirmed current within `max_age` — keep the cached copy instead
/// of failing the build.
///
/// This is what keeps an upstream outage (Interline once pointed
/// `download_latest` at a build whose blobs were never uploaded) from taking
/// the whole pipeline down for a file we already have. The bound is on the
/// *last successful check*, not the download date, so a long-lived extract
/// that the origin kept confirming stays usable; one we haven't been able to
/// verify for `max_age` is no longer trusted and the error propagates.
///
/// `display_url` is what goes into logs and errors — the Interline URL
/// carries the API token, so callers pass a redacted form.
pub fn download_or_cached(
    client: &Client,
    url: &str,
    display_url: &str,
    cache_path: &Path,
    max_age: Duration,
    label: &str,
) -> Result<PathBuf> {
    let err = match download(client, url) {
        Ok((bytes, validators)) => {
            eprintln!(
                "Downloaded {}: {:.1} MB",
                label,
                bytes.len() as f64 / 1_048_576.0
            );
            save(cache_path, &bytes, &validators)?;
            return Ok(cache_path.to_path_buf());
        }
        Err(e) => e.context(format!("failed to download {} from {}", label, display_url)),
    };

    if cache_path.exists() && checked_within(cache_path, max_age) {
        eprintln!(
            "WARNING: {:#}
  falling back to cached {}: {:?} (last confirmed {} day(s) ago)",
            err,
            label,
            cache_path,
            checked_days_ago(cache_path)
        );
        return Ok(cache_path.to_path_buf());
    }
    if cache_path.exists() {
        return Err(err.context(format!(
            "cached {} {:?} was last confirmed {} day(s) ago, past the {}-day limit",
            label,
            cache_path,
            checked_days_ago(cache_path),
            max_age.as_secs() / 86_400
        )));
    }
    Err(err)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    /// Nothing cached — must download.
    Missing,
    /// Cache matches the origin (or is young enough to trust).
    Current,
    /// Origin changed, or the entry aged out without a usable validator.
    Stale,
}

/// Decide whether `cache_path` still matches the origin, spending at most one
/// `HEAD`. `max_age` bounds how long an entry we *can't* validate is trusted.
///
/// Logs its own reasoning prefixed with `label` (callers supply any indent).
pub fn check(
    client: &Client,
    url: &str,
    cache_path: &Path,
    max_age: Duration,
    label: &str,
) -> CacheState {
    if !cache_path.exists() {
        return CacheState::Missing;
    }

    let local = load(cache_path);

    let remote = match head(client, url) {
        Ok(remote) if !remote.is_empty() => remote,
        Ok(_) => return age_fallback(cache_path, max_age, label, "origin reports no validators"),
        Err(e) => {
            eprintln!("{}: WARNING: could not check upstream: {}", label, e);
            return age_fallback(cache_path, max_age, label, "upstream check failed");
        }
    };

    match same_resource(&local, &remote) {
        Some(true) => {
            store(cache_path, &remote);
            eprintln!("{}: unchanged upstream", label);
            CacheState::Current
        }
        Some(false) => {
            eprintln!("{}: changed upstream → stale", label);
            CacheState::Stale
        }
        // First time we've seen this entry, so there's nothing to compare.
        // Adopting the origin's *current* validator would be wrong for a copy
        // that's already behind — it would match forever after. Instead prove
        // it: a file written after the origin's Last-Modified is the current
        // version, because any later upstream edit would have moved that date.
        None => match (
            parse_http_date(remote.last_modified.as_deref()),
            modified(cache_path),
        ) {
            (Some(upstream), Some(local)) if local >= upstream => {
                store(cache_path, &remote);
                eprintln!(
                    "{}: no recorded validator, but cache postdates upstream ({}) → adopting upstream validator",
                    label, upstream
                );
                CacheState::Current
            }
            (Some(upstream), _) => {
                eprintln!(
                    "{}: no recorded validator and cache predates upstream ({}) → stale",
                    label, upstream
                );
                CacheState::Stale
            }
            _ => age_fallback(
                cache_path,
                max_age,
                label,
                "no recorded validator and no upstream date",
            ),
        },
    }
}

/// Parse an HTTP date header (`Fri, 17 Jul 2026 08:26:43 GMT`).
fn parse_http_date(value: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc2822(value?)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

fn modified(path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from)
}

/// Decision for entries we couldn't validate: trust them until `max_age`,
/// measured from the last time the origin confirmed them (see [`checked_age`]).
fn age_fallback(cache_path: &Path, max_age: Duration, label: &str, why: &str) -> CacheState {
    let days = checked_days_ago(cache_path);
    if checked_within(cache_path, max_age) {
        eprintln!("{}: {} — keeping {} day(s) old cache", label, why, days);
        CacheState::Current
    } else {
        eprintln!(
            "{}: {} and cache is {} day(s) old → stale",
            label, why, days
        );
        CacheState::Stale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(etag: Option<&str>, lm: Option<&str>) -> Validators {
        Validators {
            etag: etag.map(str::to_string),
            last_modified: lm.map(str::to_string),
        }
    }

    #[test]
    fn etag_wins_over_last_modified() {
        // Same ETag but a rewritten Last-Modified is still the same resource.
        let local = v(Some("\"abc\""), Some("Mon, 01 Jan 2026 00:00:00 GMT"));
        let remote = v(Some("\"abc\""), Some("Tue, 02 Jan 2026 00:00:00 GMT"));
        assert_eq!(same_resource(&local, &remote), Some(true));
    }

    #[test]
    fn falls_back_to_last_modified() {
        let local = v(None, Some("Mon, 01 Jan 2026 00:00:00 GMT"));
        assert_eq!(
            same_resource(&local, &v(None, Some("Mon, 01 Jan 2026 00:00:00 GMT"))),
            Some(true)
        );
        assert_eq!(
            same_resource(&local, &v(None, Some("Tue, 02 Jan 2026 00:00:00 GMT"))),
            Some(false)
        );
    }

    #[test]
    fn nothing_comparable_is_unknown() {
        assert_eq!(
            same_resource(&Validators::default(), &v(Some("\"a\""), None)),
            None
        );
        assert_eq!(
            same_resource(&v(Some("\"a\""), None), &v(None, Some("x"))),
            None
        );
    }

    #[test]
    fn sidecar_round_trips_through_the_cache_dir() {
        let dir = std::env::temp_dir().join("city-builder-etag-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache_path = dir.join("chicago_1234.osm.pbf");

        let validators = v(
            Some("\"32f4b0ef-6\""),
            Some("Fri, 17 Jul 2026 08:26:43 GMT"),
        );
        store(&cache_path, &validators);

        assert_eq!(
            sidecar_path(&cache_path),
            dir.join("etag/chicago_1234.osm.pbf.etag")
        );
        assert_eq!(load(&cache_path), validators);
        assert!(checked_within(&cache_path, Duration::from_secs(60)));
    }

    #[test]
    fn parses_the_http_date_formats_our_origins_send() {
        // Every origin we use sends the obsolete `GMT` zone name.
        let parsed = parse_http_date(Some("Fri, 17 Jul 2026 08:26:43 GMT")).unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-07-17T08:26:43+00:00");
        assert!(parse_http_date(Some("Sat, 25 Jul 2026 16:49:54 GMT")).is_some());
        assert!(parse_http_date(Some("not a date")).is_none());
        assert!(parse_http_date(None).is_none());
    }

    /// Write `path` and backdate its mtime by `days`.
    fn backdate(path: &Path, days: u64) {
        let f = std::fs::File::create(path).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(days * 86_400))
            .unwrap();
    }

    fn fresh_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("city-builder-etag-test-{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("etag")).unwrap();
        dir
    }

    #[test]
    fn checked_age_prefers_last_confirmation_over_download_date() {
        let dir = fresh_dir("checked-age");
        let cache_path = dir.join("old.osm.pbf");
        // Downloaded 40 days ago, but the origin confirmed it 3 days ago.
        backdate(&cache_path, 40);
        backdate(&sidecar_path(&cache_path), 3);
        assert_eq!(checked_days_ago(&cache_path), 3);
        assert!(checked_within(&cache_path, cache::OSM_MAX_STALENESS));

        // Without a sidecar, the download date is all we have.
        std::fs::remove_file(sidecar_path(&cache_path)).unwrap();
        assert_eq!(checked_days_ago(&cache_path), 40);
        assert!(!checked_within(&cache_path, cache::OSM_MAX_STALENESS));
    }

    /// A download from a dead origin should fall back to the cached file only
    /// while the origin confirmed it within `max_age`.
    #[test]
    fn failed_download_falls_back_to_recently_confirmed_cache() {
        let dir = fresh_dir("fallback");
        let cache_path = dir.join("boston.osm.pbf");
        let client = client(Duration::from_secs(5)).unwrap();
        // Discard port on loopback: connection refused immediately, no server needed.
        let url = "http://127.0.0.1:9/boston.osm.pbf";
        let month = cache::OSM_MAX_STALENESS;

        // Nothing cached → the download error propagates.
        let err = download_or_cached(&client, url, url, &cache_path, month, "PBF")
            .unwrap_err()
            .to_string();
        assert!(err.contains("failed to download PBF from"), "{err}");

        // Confirmed last week → use it.
        backdate(&cache_path, 40);
        backdate(&sidecar_path(&cache_path), 7);
        let got = download_or_cached(&client, url, url, &cache_path, month, "PBF").unwrap();
        assert_eq!(got, cache_path);

        // Not confirmed in over a month → refuse.
        backdate(&sidecar_path(&cache_path), 31);
        let err = format!(
            "{:#}",
            download_or_cached(&client, url, url, &cache_path, month, "PBF").unwrap_err()
        );
        assert!(err.contains("last confirmed 31 day(s) ago"), "{err}");
        assert!(err.contains("failed to download PBF"), "{err}");
    }

    #[test]
    fn missing_sidecar_loads_as_empty() {
        let dir = std::env::temp_dir().join("city-builder-etag-test-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache_path = dir.join("nothing.gtfs.zip");
        assert!(load(&cache_path).is_empty());
        assert!(!checked_within(&cache_path, Duration::from_secs(60)));
    }
}
