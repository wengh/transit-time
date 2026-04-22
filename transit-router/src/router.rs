//! Shared helpers used by [`crate::profile`]: spatial snapping, calendar
//! filtering, and day-of-week arithmetic. The former time-dependent Dijkstra
//! (`run_tdd_multi` et al.) lived here and has been removed — profile routing
//! is the sole routing path now.

use crate::data::PreparedData;

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

/// Convert a YYYYMMDD date to day of week (0=Mon..6=Sun).
fn date_to_day_of_week(date: u32) -> u8 {
    let y = (date / 10000) as i32;
    let m = ((date / 100) % 100) as i32;
    let d = (date % 100) as i32;
    // Tomohiko Sakamoto's algorithm (returns 0=Sun..6=Sat)
    let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let dow = (y + y / 4 - y / 100 + y / 400 + t[(m - 1) as usize] + d) % 7;
    // Convert from 0=Sun..6=Sat to 0=Mon..6=Sun
    ((dow + 6) % 7) as u8
}

/// Find pattern indices active on a given date (YYYYMMDD).
/// Checks day-of-week mask, start/end date range, and date exceptions.
pub fn patterns_for_date(data: &PreparedData, date: u32) -> Vec<usize> {
    let day_of_week = date_to_day_of_week(date);
    let bit = 1u8 << day_of_week;
    data.patterns
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            if p.stop_index.events_by_stop.is_empty() && p.frequency_routes.is_empty() {
                return false;
            }
            // Explicitly removed on this date
            if p.date_exceptions_remove.contains(&date) {
                return false;
            }
            // Explicitly added on this date
            if p.date_exceptions_add.contains(&date) {
                return true;
            }
            // Check day-of-week mask
            if p.day_mask & bit == 0 {
                return false;
            }
            // Check date range (0 means unbounded)
            if p.start_date != 0 && date < p.start_date {
                return false;
            }
            if p.end_date != 0 && date > p.end_date {
                return false;
            }
            true
        })
        .map(|(i, _)| i)
        .collect()
}
