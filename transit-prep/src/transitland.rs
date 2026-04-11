use anyhow::{Context, Result};
use std::collections::HashMap;

const API_BASE: &str = "https://api.transit.land/api/v2/rest";

#[derive(serde::Deserialize)]
struct FeedsResponse {
    feeds: Vec<Feed>,
    meta: Option<Meta>,
}

#[derive(serde::Deserialize, Clone)]
pub struct Feed {
    pub onestop_id: String,
    pub urls: FeedUrls,
    #[allow(dead_code)]
    pub authorization: Option<FeedAuth>,
    pub feed_state: Option<FeedState>,
    #[serde(default)]
    pub feed_versions: Vec<FeedVersionEntry>,
}

#[derive(serde::Deserialize, Clone)]
pub struct FeedVersionEntry {
    pub latest_calendar_date: Option<String>,
}

#[derive(serde::Deserialize, Clone)]
pub struct FeedState {
    pub feed_version: Option<FeedVersion>,
}

#[derive(serde::Deserialize, Clone)]
pub struct FeedVersion {
    pub sha1: Option<String>,
}

#[derive(serde::Deserialize, Clone)]
pub struct FeedUrls {
    pub static_current: Option<String>,
}

#[derive(serde::Deserialize, Clone)]
#[allow(dead_code)]
pub struct FeedAuth {
    #[serde(rename = "type")]
    pub auth_type: Option<String>,
}

#[derive(serde::Deserialize)]
struct Meta {
    next: Option<String>,
}

#[derive(serde::Deserialize)]
struct OperatorsResponse {
    operators: Vec<Operator>,
    meta: Option<Meta>,
}

#[derive(serde::Deserialize)]
struct Operator {
    name: Option<String>,
    feeds: Option<Vec<OperatorFeed>>,
}

#[derive(serde::Deserialize)]
struct OperatorFeed {
    onestop_id: Option<String>,
}

pub fn get_api_key() -> Result<String> {
    std::env::var("TRANSITLAND_API_KEY")
        .context("TRANSITLAND_API_KEY not set (check .env or environment)")
}

fn make_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (compatible; transit-prep/1.0)")
        .build()?)
}

/// Query the latest feed version SHA1 for a Transitland feed.
pub fn latest_feed_sha1(api_key: &str, onestop_id: &str) -> Result<Option<String>> {
    let client = make_client()?;
    let url = format!("{}/feeds/{}", API_BASE, onestop_id);
    let resp: FeedsResponse = client
        .get(&url)
        .header("apikey", api_key)
        .send()?
        .error_for_status()
        .with_context(|| format!("Transitland API request failed for feed '{}'", onestop_id))?
        .json()?;

    let sha1 = resp
        .feeds
        .first()
        .and_then(|f| f.feed_state.as_ref())
        .and_then(|fs| fs.feed_version.as_ref())
        .and_then(|fv| fv.sha1.clone());

    Ok(sha1)
}

/// Download the latest GTFS zip for a Transitland feed using header-based auth.
/// Returns the raw bytes of the zip file.
pub fn download_feed(api_key: &str, onestop_id: &str) -> Result<Vec<u8>> {
    let url = format!(
        "{}/feeds/{}/download_latest_feed_version",
        API_BASE, onestop_id
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .user_agent("Mozilla/5.0 (compatible; transit-prep/1.0)")
        .build()?;
    let bytes = client
        .get(&url)
        .header("apikey", api_key)
        .send()
        .with_context(|| format!("Failed to request Transitland feed '{}'", onestop_id))?
        .error_for_status()
        .with_context(|| format!("Failed to download feed '{}'", onestop_id))?
        .bytes()
        .with_context(|| {
            format!(
                "Failed to read Transitland response body for feed '{}'",
                onestop_id
            )
        })?;
    Ok(bytes.to_vec())
}

pub fn query_feeds_in_bbox(api_key: &str, bbox: (f64, f64, f64, f64)) -> Result<Vec<Feed>> {
    let client = make_client()?;
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    let mut all_feeds = Vec::new();
    let mut url = format!(
        "{}/feeds?spec=gtfs&limit=100&bbox={},{},{},{}",
        API_BASE, min_lon, min_lat, max_lon, max_lat
    );

    loop {
        eprintln!(
            "Querying Transitland feeds: {} ...",
            &url[..url.len().min(120)]
        );
        let resp: FeedsResponse = client
            .get(&url)
            .header("apikey", api_key)
            .send()?
            .error_for_status()
            .context("Transitland feeds query failed")?
            .json()?;

        let count = resp.feeds.len();
        all_feeds.extend(resp.feeds);
        eprintln!("  got {} feeds (total: {})", count, all_feeds.len());

        match resp.meta.and_then(|m| m.next) {
            Some(next) => url = next,
            None => break,
        }
    }

    // Filter out feeds whose latest version expired over 1 month ago
    let today = chrono::Utc::now().date_naive();
    let cutoff = today - chrono::Duration::days(30);
    let before_filter = all_feeds.len();
    all_feeds.retain(|feed| {
        let latest_date = feed
            .feed_versions
            .first()
            .and_then(|fv| fv.latest_calendar_date.as_deref())
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        match latest_date {
            Some(date) if date < cutoff => {
                eprintln!(
                    "WARNING: skipping feed '{}' — expired {} (over 1 month ago)",
                    feed.onestop_id, date
                );
                false
            }
            _ => true, // keep feeds with no date info or still valid
        }
    });
    if before_filter != all_feeds.len() {
        eprintln!(
            "  filtered out {} expired feeds",
            before_filter - all_feeds.len()
        );
    }

    Ok(all_feeds)
}

pub fn query_operators_in_bbox(
    api_key: &str,
    bbox: (f64, f64, f64, f64),
) -> Result<Vec<(String, String)>> {
    let client = make_client()?;
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    let mut feed_to_operator: Vec<(String, String)> = Vec::new();
    let mut url = format!(
        "{}/operators?limit=100&bbox={},{},{},{}",
        API_BASE, min_lon, min_lat, max_lon, max_lat
    );

    loop {
        eprintln!(
            "Querying Transitland operators: {} ...",
            &url[..url.len().min(120)]
        );
        let resp: OperatorsResponse = client
            .get(&url)
            .header("apikey", api_key)
            .send()?
            .error_for_status()
            .context("Transitland operators query failed")?
            .json()?;

        let count = resp.operators.len();
        for op in &resp.operators {
            if let Some(name) = &op.name {
                for feed in op.feeds.iter().flatten() {
                    if let Some(fid) = &feed.onestop_id {
                        feed_to_operator.push((fid.clone(), name.clone()));
                    }
                }
            }
        }
        eprintln!("  got {} operators", count);

        match resp.meta.and_then(|m| m.next) {
            Some(next) => url = next,
            None => break,
        }
    }

    Ok(feed_to_operator)
}

pub fn build_feed_operator_map(pairs: &[(String, String)]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (feed_id, op_name) in pairs {
        map.entry(feed_id.clone())
            .and_modify(|existing: &mut String| {
                if !existing.contains(op_name.as_str()) {
                    existing.push_str(", ");
                    existing.push_str(op_name);
                }
            })
            .or_insert_with(|| op_name.clone());
    }
    map
}
