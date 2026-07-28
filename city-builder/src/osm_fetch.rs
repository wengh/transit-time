//! Download OSM pedestrian-walkable extracts from BBBike, Interline, or
//! Overpass; cache by source-URL hash.
//!
//! BBBike, Interline and direct `osm_url` sources all expose ETags, so cached
//! extracts are validated rather than re-downloaded — but only once they've
//! gone [`cache::OSM_MAX_STALENESS`] without a check, since base map geometry
//! moves slowly and these files are large. Overpass is a POST query with no
//! validators, so it stays on the age rule.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::cache;
use crate::http_cache;

// Try multiple Overpass servers
const OVERPASS_URLS: &[&str] = &[
    "https://overpass-api.de/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
];

// Known city PBF extract URLs (BBBike)
const BBBIKE_BASE: &str = "https://download.bbbike.org/osm/bbbike";

// Interline OSM Extracts download endpoint
const INTERLINE_BASE: &str = "https://app.interline.io/osm_extracts/download_latest";

/// FNV-1a hash of a URL — used to key cache filenames against their source.
pub fn url_hash(url: &str) -> u64 {
    url.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        h.wrapping_mul(0x100000001b3) ^ b as u64
    })
}

pub fn interline_api_key() -> Option<String> {
    std::env::var("INTERLINE_OSM_EXTRACTS_API_KEY")
        .ok()
        .filter(|v| !v.is_empty())
}

/// URL used for cache-key hashing (token deliberately omitted so the cache name
/// is stable across users / token rotations).
pub fn interline_source_url(extract_id: &str) -> String {
    format!(
        "{}?string_id={}&data_format=pbf",
        INTERLINE_BASE, extract_id
    )
}

pub fn bbbike_source_url(bbbike_name: &str) -> String {
    format!("{}/{}/{}.osm.pbf", BBBIKE_BASE, bbbike_name, bbbike_name)
}

/// Pick the canonical OSM source URL from the configured sources.
/// Returns `None` only when all three are absent (Overpass fallback).
pub fn pick_source_url(
    interline_extract: Option<&str>,
    bbbike_name: Option<&str>,
    osm_url: Option<&str>,
) -> Option<String> {
    if let Some(url) = osm_url {
        Some(url.to_string())
    } else if let Some(extract_id) = interline_extract {
        Some(interline_source_url(extract_id))
    } else {
        bbbike_name.map(bbbike_source_url)
    }
}

/// Cache file path: `<sanitized_id>_<16hex hash of source url>.<ext>`.
pub fn pbf_cache_path(cache_dir: &Path, city: &str, source_url: &str, ext: &str) -> PathBuf {
    cache_dir.join(format!(
        "{}_{:016x}.{}",
        sanitize(city),
        url_hash(source_url),
        ext
    ))
}

/// Cache file path for the Overpass fallback (keyed by bbox, not by source).
pub fn overpass_cache_path(cache_dir: &Path, bbox: (f64, f64, f64, f64)) -> PathBuf {
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    cache_dir.join(format!(
        "osm_{:.4}_{:.4}_{:.4}_{:.4}.xml",
        min_lon, min_lat, max_lon, max_lat
    ))
}

/// Extension for a directly-configured `osm_url`; anything else is a PBF.
fn source_ext(osm_url: Option<&str>) -> &'static str {
    match osm_url {
        Some(url) if !url.contains(".pbf") => "osm.xml",
        _ => "osm.pbf",
    }
}

/// The URL to actually request for a city's OSM source, including the
/// Interline token. `None` when there's no HTTP source (Overpass fallback) or
/// when an Interline city is configured without a key.
pub fn osm_request_url(
    interline_extract: Option<&str>,
    bbbike_name: Option<&str>,
    osm_url: Option<&str>,
) -> Option<String> {
    if let Some(url) = osm_url {
        Some(url.to_string())
    } else if let Some(extract_id) = interline_extract {
        interline_api_key().map(|key| interline_download_url(extract_id, &key))
    } else {
        bbbike_name.map(bbbike_source_url)
    }
}

/// Reuse-or-refetch an OSM extract from an HTTP source.
///
/// Within [`cache::OSM_MAX_STALENESS`] of the last confirmation this doesn't
/// touch the network at all; past that it compares validators and downloads
/// only on an actual change.
fn fetch_http_osm(cache_path: &Path, url: &str, display_url: &str, label: &str) -> Result<PathBuf> {
    if cache_path.exists() && http_cache::checked_within(cache_path, cache::OSM_MAX_STALENESS) {
        eprintln!(
            "Using cached {}: {:?} (verified {} day(s) ago)",
            label,
            cache_path,
            http_cache::checked_days_ago(cache_path)
        );
        return Ok(cache_path.to_path_buf());
    }

    let client = http_cache::client(http_cache::CHECK_TIMEOUT)?;
    if http_cache::check(
        &client,
        url,
        cache_path,
        cache::OSM_MAX_STALENESS,
        &format!("{} {:?}", label, cache_path),
    ) == http_cache::CacheState::Current
    {
        return Ok(cache_path.to_path_buf());
    }

    eprintln!("Downloading {} from: {}", label, display_url);
    let client = http_cache::client(http_cache::DOWNLOAD_TIMEOUT)?;
    let (bytes, validators) = http_cache::download(&client, url)
        .with_context(|| format!("failed to download {} from {}", label, display_url))?;
    eprintln!(
        "Downloaded {}: {:.1} MB",
        label,
        bytes.len() as f64 / 1_048_576.0
    );
    http_cache::save(cache_path, &bytes, &validators)?;
    Ok(cache_path.to_path_buf())
}

/// Fetch pedestrian-walkable OSM data for a bounding box, caching the result.
///
/// Exactly one of `interline_extract`, `bbbike_name`, or `osm_url` may be set;
/// providing more than one is a configuration error. If none is set, falls back
/// to an Overpass query over `bbox`.
pub fn fetch_osm(
    bbox: (f64, f64, f64, f64),
    cache_dir: &Path,
    city: &str,
    interline_extract: Option<&str>,
    bbbike_name: Option<&str>,
    osm_url: Option<&str>,
) -> Result<PathBuf> {
    let configured = interline_extract.is_some() as usize
        + bbbike_name.is_some() as usize
        + osm_url.is_some() as usize;
    if configured > 1 {
        bail!(
            "at most one of `interline_extract`, `bbbike_name`, `osm_url` may be set for city '{}'",
            city
        );
    }

    if let Some(url) = osm_url {
        let cache_path = pbf_cache_path(cache_dir, city, url, source_ext(osm_url));
        return fetch_http_osm(&cache_path, url, url, "OSM");
    }

    if let Some(extract_id) = interline_extract {
        let cache_path = pbf_cache_path(
            cache_dir,
            city,
            &interline_source_url(extract_id),
            "osm.pbf",
        );
        let key = interline_api_key().ok_or_else(|| {
            anyhow::anyhow!(
                "city '{}' uses interline_extract but INTERLINE_OSM_EXTRACTS_API_KEY is not set",
                city
            )
        })?;
        // Log the token-free form of the URL.
        let display = format!(
            "{}?string_id={}&data_format=pbf&api_token=…",
            INTERLINE_BASE, extract_id
        );
        return fetch_http_osm(
            &cache_path,
            &interline_download_url(extract_id, &key),
            &display,
            "PBF",
        );
    }

    if let Some(name) = bbbike_name {
        let url = bbbike_source_url(name);
        let cache_path = pbf_cache_path(cache_dir, city, &url, "osm.pbf");
        return fetch_http_osm(&cache_path, &url, &url, "PBF");
    }

    // No source configured — use Overpass for the bbox. Overpass is a POST
    // query with no validators, so this one stays on the age rule.
    let xml_cache = overpass_cache_path(cache_dir, bbox);
    if xml_cache.exists() && cache::is_fresh(&xml_cache, cache::OSM_MAX_STALENESS) {
        eprintln!("Using cached OSM XML: {:?}", xml_cache);
        return Ok(xml_cache);
    }
    fetch_overpass(bbox, &xml_cache)
}

/// Interline download URL including the API token — never log this directly.
fn interline_download_url(extract_id: &str, api_key: &str) -> String {
    format!(
        "{}?string_id={}&data_format=pbf&api_token={}",
        INTERLINE_BASE,
        urlencoded(extract_id),
        urlencoded(api_key),
    )
}

fn fetch_overpass(bbox: (f64, f64, f64, f64), cache_path: &Path) -> Result<PathBuf> {
    let (min_lon, min_lat, max_lon, max_lat) = bbox;

    let query = format!(
        r#"[out:xml][timeout:300];
(
  way["highway"~"^(footway|pedestrian|path|steps|residential|living_street|tertiary|secondary|primary|trunk|service|unclassified|crossing|cycleway|track|corridor)$"]({0},{1},{2},{3});
);
(._;>;);
out body;"#,
        min_lat, min_lon, max_lat, max_lon
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;

    let body = format!("data={}", urlencoded(&query));

    for (i, url) in OVERPASS_URLS.iter().enumerate() {
        eprintln!("Trying Overpass server: {} ...", url);
        match client
            .post(*url)
            .body(body.clone())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    let text = resp.text()?;
                    http_cache::write_atomic(cache_path, text.as_bytes())?;
                    eprintln!("OSM data: {} bytes", text.len());
                    return Ok(cache_path.to_path_buf());
                }
                eprintln!("Server {} returned {}", url, resp.status());
            }
            Err(e) => {
                eprintln!("Server {} failed: {}", url, e);
            }
        }
        if i < OVERPASS_URLS.len() - 1 {
            eprintln!("Retrying with next server...");
        }
    }

    bail!("All Overpass servers failed")
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn urlencoded(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            ' ' => out.push('+'),
            _ => {
                for byte in c.to_string().as_bytes() {
                    out.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    out
}
