use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

const MDB_TOKEN_URL: &str = "https://api.mobilitydatabase.org/v1/tokens";
const MDB_FEEDS_URL: &str = "https://api.mobilitydatabase.org/v1/gtfs_feeds";

/// Fetch a GTFS zip for the given feed ID, caching the result.
pub fn fetch_gtfs(
    feed_id: &str,
    cache_dir: &Path,
    refresh_token: &str,
) -> Result<PathBuf> {
    let cache_path = cache_dir.join(format!("{}.gtfs.zip", sanitize_filename(feed_id)));
    if cache_path.exists() {
        eprintln!("Using cached GTFS: {:?}", cache_path);
        return Ok(cache_path);
    }

    let access_token = get_access_token(refresh_token)?;
    let feed_url = find_feed_url(feed_id, &access_token)?;
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

fn find_feed_url(feed_id: &str, access_token: &str) -> Result<String> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(format!("{}/{}", MDB_FEEDS_URL, feed_id))
        .header("Authorization", format!("Bearer {}", access_token))
        .send()?
        .error_for_status()
        .with_context(|| format!("Failed to fetch feed with ID {}", feed_id))?;

    let feed: serde_json::Value = resp.json()?;
    extract_download_url(&feed)
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
