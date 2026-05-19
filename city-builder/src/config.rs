//! City `.jsonc` config schema.

#[derive(serde::Deserialize)]
pub struct CityConfig {
    pub id: String,
    pub feed_ids: Vec<String>,
    pub bbox: String,
    pub bbbike_name: Option<String>,
    pub interline_extract: Option<String>,
    pub osm_url: Option<String>,
    pub allow_stale: Option<bool>,
    pub enabled: Option<bool>,
}
