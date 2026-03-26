use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::{Path, PathBuf};

// Try multiple Overpass servers
const OVERPASS_URLS: &[&str] = &[
    "https://overpass-api.de/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
];

/// Fetch pedestrian-walkable OSM data for a bounding box, caching the result.
pub fn fetch_osm(bbox: (f64, f64, f64, f64), cache_dir: &Path) -> Result<PathBuf> {
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    let cache_name = format!(
        "osm_{:.4}_{:.4}_{:.4}_{:.4}.xml",
        min_lon, min_lat, max_lon, max_lat
    );
    let cache_path = cache_dir.join(cache_name);

    if cache_path.exists() {
        eprintln!("Using cached OSM data: {:?}", cache_path);
        return Ok(cache_path);
    }

    // Overpass query for pedestrian-walkable ways + station entrances/corridors
    // bbox format for Overpass: (south,west,north,east) = (min_lat,min_lon,max_lat,max_lon)
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
                    let mut file = std::fs::File::create(&cache_path)?;
                    file.write_all(text.as_bytes())?;
                    eprintln!("OSM data: {} bytes", text.len());
                    return Ok(cache_path);
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
