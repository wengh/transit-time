use anyhow::{Result, bail};
use std::io::Write;
use std::path::{Path, PathBuf};

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
    let (min_lon, min_lat, max_lon, max_lat) = bbox;

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
        let ext = if url.contains(".pbf") {
            "osm.pbf"
        } else {
            "osm.xml"
        };
        let cache_path = pbf_cache_path(cache_dir, city, url, ext);
        if cache_path.exists() {
            eprintln!("Using cached OSM: {:?}", cache_path);
            return Ok(cache_path);
        }
        eprintln!("Downloading OSM from: {}", url);
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .user_agent("Mozilla/5.0 (compatible; transit-prep/1.0)")
            .build()?;
        let bytes = client.get(url).send()?.error_for_status()?.bytes()?;
        eprintln!("Downloaded OSM: {:.1} MB", bytes.len() as f64 / 1_048_576.0);
        let tmp = cache_path.with_extension("tmp");
        std::fs::File::create(&tmp)?.write_all(&bytes)?;
        std::fs::rename(&tmp, &cache_path)?;
        return Ok(cache_path);
    }

    if let Some(extract_id) = interline_extract {
        let cache_path = pbf_cache_path(
            cache_dir,
            city,
            &interline_source_url(extract_id),
            "osm.pbf",
        );
        if cache_path.exists() {
            eprintln!("Using cached PBF: {:?}", cache_path);
            return Ok(cache_path);
        }
        let key = interline_api_key().ok_or_else(|| {
            anyhow::anyhow!(
                "city '{}' uses interline_extract but INTERLINE_OSM_EXTRACTS_API_KEY is not set",
                city
            )
        })?;
        return try_interline_download(extract_id, &key, &cache_path);
    }

    if let Some(name) = bbbike_name {
        let cache_path = pbf_cache_path(cache_dir, city, &bbbike_source_url(name), "osm.pbf");
        if cache_path.exists() {
            eprintln!("Using cached PBF: {:?}", cache_path);
            return Ok(cache_path);
        }
        return try_bbbike_download(name, &cache_path);
    }

    // No source configured — use Overpass for the bbox.
    let xml_cache = cache_dir.join(format!(
        "osm_{:.4}_{:.4}_{:.4}_{:.4}.xml",
        min_lon, min_lat, max_lon, max_lat
    ));
    if xml_cache.exists() {
        eprintln!("Using cached OSM XML: {:?}", xml_cache);
        return Ok(xml_cache);
    }
    fetch_overpass(bbox, &xml_cache)
}

/// Try to download a PBF extract from Interline OSM Extracts.
pub fn try_interline_download(
    extract_id: &str,
    api_key: &str,
    cache_path: &Path,
) -> Result<PathBuf> {
    let url = format!(
        "{}?string_id={}&data_format=pbf&api_token={}",
        INTERLINE_BASE,
        urlencoded(extract_id),
        urlencoded(api_key),
    );

    eprintln!(
        "Trying Interline extract: {}?string_id={}&data_format=pbf&api_token=…",
        INTERLINE_BASE, extract_id,
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .user_agent("Mozilla/5.0 (compatible; transit-prep/1.0)")
        .build()?;

    let resp = client.get(&url).send()?;

    if !resp.status().is_success() {
        bail!("Interline returned {}", resp.status());
    }

    let bytes = resp.bytes()?;
    eprintln!("Downloaded PBF: {:.1} MB", bytes.len() as f64 / 1_048_576.0);

    let tmp = cache_path.with_extension("tmp");
    std::fs::File::create(&tmp)?.write_all(&bytes)?;
    std::fs::rename(&tmp, cache_path)?;

    Ok(cache_path.to_path_buf())
}

/// Try to download a city PBF extract from BBBike.
pub fn try_bbbike_download(bbbike_name: &str, cache_path: &Path) -> Result<PathBuf> {
    let url = format!("{}/{}/{}.osm.pbf", BBBIKE_BASE, bbbike_name, bbbike_name);

    eprintln!("Trying BBBike extract: {} ...", url);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;

    let resp = client.get(&url).send()?;

    if !resp.status().is_success() {
        bail!("BBBike returned {}", resp.status());
    }

    let bytes = resp.bytes()?;
    eprintln!("Downloaded PBF: {:.1} MB", bytes.len() as f64 / 1_048_576.0);

    let mut file = std::fs::File::create(cache_path)?;
    file.write_all(&bytes)?;

    Ok(cache_path.to_path_buf())
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
                    let mut file = std::fs::File::create(cache_path)?;
                    file.write_all(text.as_bytes())?;
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

pub fn sanitize(s: &str) -> String {
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
