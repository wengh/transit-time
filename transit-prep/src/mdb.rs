use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

const MDB_TOKEN_URL: &str = "https://api.mobilitydatabase.org/v1/tokens";
const MDB_FEEDS_URL: &str = "https://api.mobilitydatabase.org/v1/gtfs_feeds";

/// Fetch a GTFS zip for the given city, caching the result.
pub fn fetch_gtfs(
    city: &str,
    feed_id: Option<&str>,
    cache_dir: &Path,
    refresh_token: &str,
) -> Result<PathBuf> {
    let cache_path = cache_dir.join(format!("{}.gtfs.zip", sanitize_filename(city)));
    if cache_path.exists() {
        eprintln!("Using cached GTFS: {:?}", cache_path);
        return Ok(cache_path);
    }

    // Get access token from MDB
    let access_token = get_access_token(refresh_token)?;

    // Search for feeds matching the city or use specific feed ID
    let feed_url = find_feed_url(city, feed_id, &access_token)?;
    eprintln!("Downloading GTFS from: {}", feed_url);

    // Download the GTFS zip
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    let resp = client.get(&feed_url).send()?.error_for_status()?;
    let bytes = resp.bytes()?;

    let mut file = std::fs::File::create(&cache_path)?;
    file.write_all(&bytes)?;

    Ok(cache_path)
}

fn get_access_token(refresh_token: &str) -> Result<String> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(MDB_TOKEN_URL)
        .header("Content-Type", "application/json")
        .body(serde_json::json!({"refresh_token": refresh_token}).to_string())
        .send()?
        .error_for_status()
        .context("Failed to get MDB access token")?;

    let body: serde_json::Value = resp.json()?;
    let token = body["access_token"]
        .as_str()
        .context("No access_token in MDB response")?
        .to_string();
    Ok(token)
}

fn find_feed_url(city: &str, feed_id: Option<&str>, access_token: &str) -> Result<String> {
    let client = reqwest::blocking::Client::new();

    if let Some(id) = feed_id {
        let resp = client
            .get(format!("{}/{}", MDB_FEEDS_URL, id))
            .header("Authorization", format!("Bearer {}", access_token))
            .send()?
            .error_for_status()
            .context(format!("Failed to fetch feed with ID {}", id))?;

        let feed: serde_json::Value = resp.json()?;
        return extract_download_url(&feed);
    }

    // Search feeds by municipality (city name)
    let resp = client
        .get(MDB_FEEDS_URL)
        .query(&[("municipality", city), ("limit", "5")])
        .header("Authorization", format!("Bearer {}", access_token))
        .send()?
        .error_for_status()
        .context("Failed to search MDB feeds")?;

    let feeds: Vec<serde_json::Value> = resp.json()?;

    // Prefer active feeds
    let active_feeds: Vec<&serde_json::Value> = feeds
        .iter()
        .filter(|f| f.get("status").and_then(|s| s.as_str()) == Some("active"))
        .collect();

    if let Some(feed) = active_feeds.first() {
        return extract_download_url(feed);
    }
    if !feeds.is_empty() {
        return extract_download_url(&feeds[0]);
    }

    if feeds.is_empty() {
        // Try with location search
        let resp = client
            .get(MDB_FEEDS_URL)
            .query(&[("country_code", "US"), ("limit", "100")])
            .header("Authorization", format!("Bearer {}", access_token))
            .send()?
            .error_for_status()?;

        let all_feeds: Vec<serde_json::Value> = resp.json()?;
        let city_lower = city.to_lowercase();

        for feed in &all_feeds {
            let name = feed["provider"].as_str().unwrap_or("");
            let municipality = feed
                .get("locations")
                .and_then(|l| l.as_array())
                .and_then(|a| a.first())
                .and_then(|l| l.get("municipality"))
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if name.to_lowercase().contains(&city_lower)
                || municipality.to_lowercase().contains(&city_lower)
            {
                return extract_download_url(feed);
            }
        }
        bail!("No GTFS feed found for city '{}'", city);
    }

    extract_download_url(&feeds[0])
}

fn extract_download_url(feed: &serde_json::Value) -> Result<String> {
    // Try latest_dataset first, then source_info
    if let Some(url) = feed
        .get("latest_dataset")
        .and_then(|d| d.get("hosted_url"))
        .and_then(|u| u.as_str())
    {
        return Ok(url.to_string());
    }
    if let Some(url) = feed
        .get("source_info")
        .and_then(|s| s.get("producer_url"))
        .and_then(|u| u.as_str())
    {
        return Ok(url.to_string());
    }
    bail!("No download URL found in feed: {}", feed)
}

fn sanitize_filename(s: &str) -> String {
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
