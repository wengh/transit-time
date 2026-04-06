use anyhow::{bail, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

// Try multiple Overpass servers
const OVERPASS_URLS: &[&str] = &[
    "https://overpass-api.de/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
];

// Known city PBF extract URLs (BBBike)
const BBBIKE_BASE: &str = "https://download.bbbike.org/osm/bbbike";

/// Fetch pedestrian-walkable OSM data for a bounding box, caching the result.
/// If `osm_url` is given, downloads directly from that URL.
/// Otherwise, tries a BBBike PBF extract (if `bbbike_name` is set) then falls back to Overpass.
pub fn fetch_osm(
    bbox: (f64, f64, f64, f64),
    cache_dir: &Path,
    city: &str,
    bbbike_name: Option<&str>,
    osm_url: Option<&str>,
) -> Result<PathBuf> {
    let (min_lon, min_lat, max_lon, max_lat) = bbox;

    if let Some(url) = osm_url {
        let ext = if url.contains(".pbf") { "osm.pbf" } else { "osm.xml" };
        let cache_path = cache_dir.join(format!("{}.{}", sanitize(city), ext));
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
        std::fs::File::create(&cache_path)?.write_all(&bytes)?;
        return Ok(cache_path);
    }

    // Check if a PBF cache already exists
    let pbf_cache = cache_dir.join(format!("{}.osm.pbf", sanitize(city)));
    if pbf_cache.exists() {
        eprintln!("Using cached PBF: {:?}", pbf_cache);
        return Ok(pbf_cache);
    }

    // Check if an XML cache already exists
    let xml_cache = cache_dir.join(format!(
        "osm_{:.4}_{:.4}_{:.4}_{:.4}.xml",
        min_lon, min_lat, max_lon, max_lat
    ));
    if xml_cache.exists() {
        eprintln!("Using cached OSM XML: {:?}", xml_cache);
        return Ok(xml_cache);
    }

    if let Some(name) = bbbike_name {
        if let Ok(path) = try_bbbike_download(name, &pbf_cache) {
            return Ok(path);
        }
        eprintln!("BBBike extract not available, falling back to Overpass...");
    }

    // Overpass for smaller areas
    fetch_overpass(bbox, &xml_cache)
}

/// Try to download a city PBF extract from BBBike.
fn try_bbbike_download(bbbike_name: &str, cache_path: &Path) -> Result<PathBuf> {
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
  node["railway"="subway_entrance"]({0},{1},{2},{3});
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
