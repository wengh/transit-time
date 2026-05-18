//! Shared helpers used by [`crate::profile`]: spatial snapping, calendar
//! filtering, and day-of-week arithmetic. The former time-dependent Dijkstra
//! (`run_tdd_multi` et al.) lived here and has been removed — profile routing
//! is the sole routing path now.

use crate::data::PreparedData;
use chrono::{Datelike, NaiveDate};

/// Snap lat/lon to nearest OSM node using spatial grid index.
pub fn snap_to_node(data: &PreparedData, lat: f64, lon: f64) -> Option<u32> {
    const CELL_SIZE_LAT: f64 = 0.0045;
    const CELL_SIZE_LON: f64 = 0.006;

    let cell_lat = (lat / CELL_SIZE_LAT).floor() as i32;
    let cell_lon = (lon / CELL_SIZE_LON).floor() as i32;
    let cos_lat = lat.to_radians().cos();

    let mut best: Option<u32> = None;
    let mut best_dist = f64::MAX;

    // Search 3x3 neighborhood of cells
    for dlat in -1..=1 {
        for dlon in -1..=1 {
            if let Some(indices) = data.node_grid.get(&(cell_lat + dlat, cell_lon + dlon)) {
                for &i in indices {
                    let node = &data.nodes[i as usize];
                    let dlat_val = node.lat - lat;
                    let dlon_val = (node.lon - lon) * cos_lat;
                    let dist = dlat_val * dlat_val + dlon_val * dlon_val;
                    if dist < best_dist {
                        best_dist = dist;
                        best = Some(i);
                    }
                }
            }
        }
    }

    best
}

/// Find pattern indices active on a given date.
/// Checks day-of-week mask, start/end date range, and date exceptions.
pub fn patterns_for_date(data: &PreparedData, date: NaiveDate) -> Vec<usize> {
    let bit = 1u8 << date.weekday().num_days_from_monday();
    data.patterns
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            if p.stop_index.events_by_stop.is_empty() && p.frequency_routes.is_empty() {
                return false;
            }
            if p.date_exceptions_remove.contains(&date) {
                return false;
            }
            if p.date_exceptions_add.contains(&date) {
                return true;
            }
            if p.day_mask & bit == 0 {
                return false;
            }
            if let Some(start) = p.start_date
                && date < start
            {
                return false;
            }
            if let Some(end) = p.end_date
                && date > end
            {
                return false;
            }
            true
        })
        .map(|(i, _)| i)
        .collect()
}
