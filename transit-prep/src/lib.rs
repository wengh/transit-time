//! Build a `city.bin` from local GTFS + OSM inputs.
//!
//! This crate is a pure library + thin CLI: it accepts already-downloaded
//! files and writes a binary suitable for `transit-router`/`transit-data`.
//! All networking, feed staleness checks, and JSONC config plumbing live in
//! the separate `city-builder` crate.

pub mod binary;
pub mod graph;
pub mod gtfs;
pub mod prepare;
pub mod shape_match;
pub mod stale;

pub use prepare::prepare;

/// Parse a `"min_lon,min_lat,max_lon,max_lat"` decimal-degree bbox string.
pub fn parse_bbox(s: &str) -> anyhow::Result<(f64, f64, f64, f64)> {
    let parts: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        parts.len() == 4,
        "bbox must have 4 values: min_lon,min_lat,max_lon,max_lat"
    );
    Ok((parts[0], parts[1], parts[2], parts[3]))
}
