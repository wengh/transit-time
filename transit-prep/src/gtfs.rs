use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Stop {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub index: u32, // internal index
}

#[derive(Debug, Clone)]
pub struct Route {
    pub id: String,
    pub short_name: String,
    pub index: u32,
}

#[derive(Debug, Clone)]
pub struct Trip {
    pub id: String,
    pub route_id: String,
    pub service_id: String,
    pub shape_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StopTime {
    pub trip_id: String,
    pub stop_id: String,
    pub arrival_time: u32,   // seconds since midnight
    pub departure_time: u32, // seconds since midnight
    pub stop_sequence: u32,
}

#[derive(Debug, Clone)]
pub struct Service {
    pub id: String,
    pub days: [bool; 7], // mon-sun
    pub start_date: u32, // YYYYMMDD
    pub end_date: u32,
    pub added_dates: Vec<u32>,
    pub removed_dates: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct Frequency {
    pub trip_id: String,
    pub start_time: u32,
    pub end_time: u32,
    pub headway_secs: u32,
}

#[derive(Debug, Clone)]
pub struct ShapePoint {
    pub shape_id: String,
    pub lat: f64,
    pub lon: f64,
    pub sequence: u32,
}

#[derive(Debug)]
pub struct GtfsData {
    pub stops: Vec<Stop>,
    pub routes: Vec<Route>,
    pub trips: Vec<Trip>,
    pub stop_times: Vec<StopTime>,
    pub services: Vec<Service>,
    pub frequencies: Vec<Frequency>,
    pub shapes: HashMap<String, Vec<(f64, f64)>>, // shape_id -> [(lat, lon)]
}

impl GtfsData {
    /// Merge another feed into this one, offsetting numeric indices.
    /// String-based references (trip_id, route_id, stop_id, service_id) are
    /// kept as-is — collisions across feeds are unlikely and harmless for
    /// build_service_patterns which resolves everything by string.
    pub fn merge(&mut self, other: GtfsData) {
        let stop_offset = self.stops.len() as u32;
        let route_offset = self.routes.len() as u32;

        for mut stop in other.stops {
            stop.index += stop_offset;
            self.stops.push(stop);
        }
        for mut route in other.routes {
            route.index += route_offset;
            self.routes.push(route);
        }
        self.trips.extend(other.trips);
        self.stop_times.extend(other.stop_times);

        // Merge services: if same service_id exists, combine date exceptions
        let existing: HashMap<String, usize> = self
            .services
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.clone(), i))
            .collect();
        for svc in other.services {
            if let Some(&idx) = existing.get(&svc.id) {
                self.services[idx].added_dates.extend(svc.added_dates);
                self.services[idx].removed_dates.extend(svc.removed_dates);
                for (i, &d) in svc.days.iter().enumerate() {
                    if d {
                        self.services[idx].days[i] = true;
                    }
                }
            } else {
                self.services.push(svc);
            }
        }

        self.frequencies.extend(other.frequencies);
        self.shapes.extend(other.shapes);
    }
}

/// A service pattern groups service_ids that share the same day-of-week mask.
#[derive(Debug, Clone)]
pub struct ServicePattern {
    pub pattern_id: u32,
    pub day_mask: u8, // bit 0=Mon .. bit 6=Sun
    pub date_exceptions_add: Vec<u32>,
    pub date_exceptions_remove: Vec<u32>,
    pub events: Vec<Vec<Event>>, // indexed by second offset from min_time
    pub min_time: u32,
    pub max_time: u32,
    pub frequency_routes: Vec<FrequencyEntry>,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub stop_index: u32,
    pub route_index: u32,
    pub trip_index: u32,
    pub next_stop_index: u32,
    pub travel_time: u32, // seconds to next stop
}

#[derive(Debug, Clone)]
pub struct FrequencyEntry {
    pub route_index: u32,
    pub stop_index: u32,
    pub start_time: u32,
    pub end_time: u32,
    pub headway_secs: u32,
    pub next_stop_index: u32,
    pub travel_time: u32,
}

// CSV record types
#[derive(Deserialize)]
struct StopRecord {
    stop_id: String,
    stop_name: Option<String>,
    #[serde(deserialize_with = "deserialize_f64_trim")]
    stop_lat: Option<f64>,
    #[serde(deserialize_with = "deserialize_f64_trim")]
    stop_lon: Option<f64>,
    #[serde(default)]
    location_type: Option<String>,
}

fn deserialize_f64_trim<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    match s {
        Some(ref v) if v.trim().is_empty() => Ok(None),
        Some(v) => v
            .trim()
            .parse::<f64>()
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

#[derive(Deserialize)]
struct RouteRecord {
    route_id: String,
    #[serde(default)]
    route_short_name: Option<String>,
    #[serde(default)]
    route_long_name: Option<String>,
}

#[derive(Deserialize)]
struct TripRecord {
    trip_id: String,
    route_id: String,
    service_id: String,
    #[serde(default)]
    shape_id: Option<String>,
}

#[derive(Deserialize)]
struct StopTimeRecord {
    trip_id: String,
    stop_id: String,
    arrival_time: Option<String>,
    departure_time: Option<String>,
    stop_sequence: String,
}

#[derive(Deserialize)]
struct CalendarRecord {
    service_id: String,
    monday: String,
    tuesday: String,
    wednesday: String,
    thursday: String,
    friday: String,
    saturday: String,
    sunday: String,
    start_date: String,
    end_date: String,
}

#[derive(Deserialize)]
struct CalendarDateRecord {
    service_id: String,
    date: String,
    exception_type: String,
}

#[derive(Deserialize)]
struct FrequencyRecord {
    trip_id: String,
    start_time: String,
    end_time: String,
    headway_secs: String,
}

#[derive(Deserialize)]
struct ShapeRecord {
    shape_id: String,
    shape_pt_lat: String,
    shape_pt_lon: String,
    shape_pt_sequence: String,
}

fn parse_time(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h: u32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let sec: u32 = parts[2].parse().ok()?;
    Some(h * 3600 + m * 60 + sec)
}

fn read_file_from_zip(
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
) -> Result<Option<String>> {
    // Try to find the file (may be in a subdirectory)
    let target = name.to_lowercase();
    let found = (0..archive.len()).find(|&i| {
        if let Ok(file) = archive.by_index(i) {
            let fname = file.name().to_lowercase();
            fname == target || fname.ends_with(&format!("/{}", target))
        } else {
            false
        }
    });

    match found {
        Some(idx) => {
            let mut file = archive.by_index(idx)?;
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            Ok(Some(contents))
        }
        None => Ok(None),
    }
}

pub fn parse_gtfs(path: &Path) -> Result<GtfsData> {
    let file = std::fs::File::open(path).context("Failed to open GTFS zip")?;
    let mut archive = zip::ZipArchive::new(file)?;

    // Parse stops
    let stops_csv =
        read_file_from_zip(&mut archive, "stops.txt")?.context("stops.txt not found in GTFS")?;
    let mut stops = Vec::new();
    let mut stop_id_to_index: HashMap<String, u32> = HashMap::new();
    {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(stops_csv.as_bytes());
        for result in rdr.deserialize::<StopRecord>() {
            let record = result?;
            // Skip non-stop locations (stations, entrances, etc.)
            if let Some(ref lt) = record.location_type {
                if lt != "0" && !lt.is_empty() {
                    continue;
                }
            }
            if let (Some(lat), Some(lon)) = (record.stop_lat, record.stop_lon) {
                let index = stops.len() as u32;
                stop_id_to_index.insert(record.stop_id.clone(), index);
                stops.push(Stop {
                    id: record.stop_id,
                    name: record.stop_name.unwrap_or_default(),
                    lat,
                    lon,
                    index,
                });
            }
        }
    }

    // Parse routes
    let routes_csv =
        read_file_from_zip(&mut archive, "routes.txt")?.context("routes.txt not found in GTFS")?;
    let mut routes = Vec::new();
    let mut route_id_to_index: HashMap<String, u32> = HashMap::new();
    {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(routes_csv.as_bytes());
        for result in rdr.deserialize::<RouteRecord>() {
            let record = result?;
            let index = routes.len() as u32;
            route_id_to_index.insert(record.route_id.clone(), index);
            routes.push(Route {
                id: record.route_id,
                short_name: record
                    .route_short_name
                    .or(record.route_long_name)
                    .unwrap_or_default(),
                index,
            });
        }
    }

    // Parse trips
    let trips_csv =
        read_file_from_zip(&mut archive, "trips.txt")?.context("trips.txt not found in GTFS")?;
    let mut trips = Vec::new();
    let mut trip_id_to_index: HashMap<String, u32> = HashMap::new();
    {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(trips_csv.as_bytes());
        for result in rdr.deserialize::<TripRecord>() {
            let record = result?;
            let index = trips.len() as u32;
            trip_id_to_index.insert(record.trip_id.clone(), index);
            trips.push(Trip {
                id: record.trip_id,
                route_id: record.route_id,
                service_id: record.service_id,
                shape_id: record.shape_id,
            });
        }
    }

    // Parse stop_times
    let stop_times_csv = read_file_from_zip(&mut archive, "stop_times.txt")?
        .context("stop_times.txt not found in GTFS")?;
    let mut stop_times = Vec::new();
    {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(stop_times_csv.as_bytes());
        for result in rdr.deserialize::<StopTimeRecord>() {
            let record = result?;
            let arrival = record.arrival_time.as_deref().and_then(parse_time);
            let departure = record.departure_time.as_deref().and_then(parse_time);
            if let (Some(arr), Some(dep)) = (arrival, departure) {
                stop_times.push(StopTime {
                    trip_id: record.trip_id,
                    stop_id: record.stop_id,
                    arrival_time: arr,
                    departure_time: dep,
                    stop_sequence: record.stop_sequence.parse().unwrap_or(0),
                });
            }
        }
    }

    // Parse calendar
    let mut services: HashMap<String, Service> = HashMap::new();
    if let Some(cal_csv) = read_file_from_zip(&mut archive, "calendar.txt")? {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(cal_csv.as_bytes());
        for result in rdr.deserialize::<CalendarRecord>() {
            let record = result?;
            services.insert(
                record.service_id.clone(),
                Service {
                    id: record.service_id,
                    days: [
                        record.monday == "1",
                        record.tuesday == "1",
                        record.wednesday == "1",
                        record.thursday == "1",
                        record.friday == "1",
                        record.saturday == "1",
                        record.sunday == "1",
                    ],
                    start_date: record.start_date.parse().unwrap_or(0),
                    end_date: record.end_date.parse().unwrap_or(0),
                    added_dates: Vec::new(),
                    removed_dates: Vec::new(),
                },
            );
        }
    }

    // Parse calendar_dates
    if let Some(cal_dates_csv) = read_file_from_zip(&mut archive, "calendar_dates.txt")? {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(cal_dates_csv.as_bytes());
        for result in rdr.deserialize::<CalendarDateRecord>() {
            let record = result?;
            let date: u32 = record.date.parse().unwrap_or(0);
            let service = services
                .entry(record.service_id.clone())
                .or_insert_with(|| Service {
                    id: record.service_id,
                    days: [false; 7],
                    start_date: 0,
                    end_date: 0,
                    added_dates: Vec::new(),
                    removed_dates: Vec::new(),
                });
            if record.exception_type == "1" {
                service.added_dates.push(date);
            } else if record.exception_type == "2" {
                service.removed_dates.push(date);
            }
        }
    }

    // Parse frequencies
    let mut frequencies = Vec::new();
    if let Some(freq_csv) = read_file_from_zip(&mut archive, "frequencies.txt")? {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(freq_csv.as_bytes());
        for result in rdr.deserialize::<FrequencyRecord>() {
            let record = result?;
            if let (Some(start), Some(end)) =
                (parse_time(&record.start_time), parse_time(&record.end_time))
            {
                frequencies.push(Frequency {
                    trip_id: record.trip_id,
                    start_time: start,
                    end_time: end,
                    headway_secs: record.headway_secs.parse().unwrap_or(0),
                });
            }
        }
    }

    // Parse shapes
    let mut shapes: HashMap<String, Vec<(f64, f64, u32)>> = HashMap::new();
    if let Some(shapes_csv) = read_file_from_zip(&mut archive, "shapes.txt")? {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(shapes_csv.as_bytes());
        for result in rdr.deserialize::<ShapeRecord>() {
            if let Ok(record) = result {
                if let (Ok(lat), Ok(lon), Ok(seq)) = (
                    record.shape_pt_lat.parse::<f64>(),
                    record.shape_pt_lon.parse::<f64>(),
                    record.shape_pt_sequence.parse::<u32>(),
                ) {
                    shapes
                        .entry(record.shape_id)
                        .or_default()
                        .push((lat, lon, seq));
                }
            }
        }
    }

    // Sort shapes by sequence and convert
    let shapes: HashMap<String, Vec<(f64, f64)>> = shapes
        .into_iter()
        .map(|(id, mut pts)| {
            pts.sort_by_key(|p| p.2);
            (
                id,
                pts.into_iter().map(|(lat, lon, _)| (lat, lon)).collect(),
            )
        })
        .collect();

    Ok(GtfsData {
        stops,
        routes,
        trips,
        stop_times,
        services: services.into_values().collect(),
        frequencies,
        shapes,
    })
}

/// Derive a day_mask (bit 0=Mon..6=Sun) from a list of YYYYMMDD date integers.
fn day_mask_from_dates(dates: &[u32]) -> u8 {
    let mut mask = 0u8;
    for &d in dates {
        let y = (d / 10000) as i32;
        let m = ((d % 10000) / 100) as u32;
        let day = (d % 100) as u32;
        // Tomohiko Sakamoto's algorithm: returns 0=Sun..6=Sat
        let t = [0i32, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let yy = if m < 3 { y - 1 } else { y };
        let dow_sun0 = ((yy + yy / 4 - yy / 100 + yy / 400
            + t[(m - 1) as usize]
            + day as i32)
            % 7) as u8;
        // Convert 0=Sun..6=Sat → 0=Mon..6=Sun
        let dow = if dow_sun0 == 0 { 6 } else { dow_sun0 - 1 };
        mask |= 1 << dow;
    }
    mask
}

pub fn build_service_patterns(data: &GtfsData) -> Vec<ServicePattern> {
    // Build mappings
    let mut trip_id_to_idx: HashMap<&str, u32> = HashMap::new();
    for (i, trip) in data.trips.iter().enumerate() {
        trip_id_to_idx.insert(&trip.id, i as u32);
    }

    let mut stop_id_to_idx: HashMap<&str, u32> = HashMap::new();
    for stop in &data.stops {
        stop_id_to_idx.insert(&stop.id, stop.index);
    }

    let mut route_id_to_idx: HashMap<&str, u32> = HashMap::new();
    for route in &data.routes {
        route_id_to_idx.insert(&route.id, route.index);
    }

    // Group service_ids by day mask
    let mut day_mask_groups: BTreeMap<u8, Vec<&Service>> = BTreeMap::new();
    for service in &data.services {
        let mut mask = service.days.iter().enumerate().fold(
            0u8,
            |acc, (i, &d)| {
                if d {
                    acc | (1 << i)
                } else {
                    acc
                }
            },
        );
        // For services defined only via calendar_dates (mask=0), derive the
        // day mask from their added_dates so they get included in the right
        // day-of-week patterns.
        if mask == 0 && !service.added_dates.is_empty() {
            mask = day_mask_from_dates(&service.added_dates);
        }
        day_mask_groups.entry(mask).or_default().push(service);
    }

    // For feeds that only use calendar_dates (no calendar.txt), all services may
    // still have mask 0 if date parsing failed. Merge into all-days as a fallback.
    if day_mask_groups.len() == 1 && day_mask_groups.contains_key(&0) {
        let all_date_based = day_mask_groups[&0]
            .iter()
            .all(|s| !s.added_dates.is_empty() || !s.removed_dates.is_empty());
        if all_date_based {
            let services = day_mask_groups.remove(&0).unwrap();
            day_mask_groups.insert(0x7F, services);
        }
    }

    // Sort stop_times by trip_id and stop_sequence
    let mut stop_times_by_trip: HashMap<&str, Vec<&StopTime>> = HashMap::new();
    for st in &data.stop_times {
        stop_times_by_trip.entry(&st.trip_id).or_default().push(st);
    }
    for times in stop_times_by_trip.values_mut() {
        times.sort_by_key(|st| st.stop_sequence);
    }

    // Build trip -> service_id mapping
    let mut trip_to_service: HashMap<&str, &str> = HashMap::new();
    for trip in &data.trips {
        trip_to_service.insert(&trip.id, &trip.service_id);
    }

    // Frequency-based trip IDs
    let freq_trip_ids: HashSet<&str> = data
        .frequencies
        .iter()
        .map(|f| f.trip_id.as_str())
        .collect();

    let mut patterns = Vec::new();

    for (mask, services) in &day_mask_groups {
        let service_ids: HashSet<&str> = services.iter().map(|s| s.id.as_str()).collect();

        // Collect date exceptions
        let mut adds = Vec::new();
        let mut removes = Vec::new();
        for svc in services {
            adds.extend_from_slice(&svc.added_dates);
            removes.extend_from_slice(&svc.removed_dates);
        }

        // Find min/max departure times for trips in this pattern
        let mut min_time = u32::MAX;
        let mut max_time = 0u32;

        // Collect all departure events
        let mut departure_events: Vec<(u32, Event)> = Vec::new(); // (departure_time, event)

        for trip in &data.trips {
            if !service_ids.contains(trip.service_id.as_str()) {
                continue;
            }
            if freq_trip_ids.contains(trip.id.as_str()) {
                continue; // handled separately
            }

            let route_idx = match route_id_to_idx.get(trip.route_id.as_str()) {
                Some(&idx) => idx,
                None => continue,
            };
            let trip_idx = match trip_id_to_idx.get(trip.id.as_str()) {
                Some(&idx) => idx,
                None => continue,
            };

            if let Some(times) = stop_times_by_trip.get(trip.id.as_str()) {
                // For each consecutive pair of stops, create an event at the departure stop
                for window in times.windows(2) {
                    let from = window[0];
                    let to = window[1];

                    let from_idx = match stop_id_to_idx.get(from.stop_id.as_str()) {
                        Some(&idx) => idx,
                        None => continue,
                    };
                    let to_idx = match stop_id_to_idx.get(to.stop_id.as_str()) {
                        Some(&idx) => idx,
                        None => continue,
                    };

                    let dep_time = from.departure_time;
                    let travel = to.arrival_time.saturating_sub(dep_time);

                    min_time = min_time.min(dep_time);
                    max_time = max_time.max(dep_time);

                    departure_events.push((
                        dep_time,
                        Event {
                            stop_index: from_idx,
                            route_index: route_idx,
                            trip_index: trip_idx,
                            next_stop_index: to_idx,
                            travel_time: travel,
                        },
                    ));
                }
            }
        }

        if min_time > max_time {
            min_time = 0;
            max_time = 0;
        }

        // Build direct-index event array
        let duration = if max_time >= min_time {
            (max_time - min_time + 1) as usize
        } else {
            0
        };
        let mut events: Vec<Vec<Event>> = vec![Vec::new(); duration];
        for (dep_time, event) in departure_events {
            let idx = (dep_time - min_time) as usize;
            if idx < events.len() {
                events[idx].push(event);
            }
        }

        // Build frequency entries
        let mut freq_entries = Vec::new();
        for freq in &data.frequencies {
            if let Some(&trip_idx) = trip_id_to_idx.get(freq.trip_id.as_str()) {
                let trip = &data.trips[trip_idx as usize];
                if !service_ids.contains(trip.service_id.as_str()) {
                    continue;
                }
                let route_idx = match route_id_to_idx.get(trip.route_id.as_str()) {
                    Some(&idx) => idx,
                    None => continue,
                };
                if let Some(times) = stop_times_by_trip.get(trip.id.as_str()) {
                    for window in times.windows(2) {
                        let from = window[0];
                        let to = window[1];
                        let from_idx = match stop_id_to_idx.get(from.stop_id.as_str()) {
                            Some(&idx) => idx,
                            None => continue,
                        };
                        let to_idx = match stop_id_to_idx.get(to.stop_id.as_str()) {
                            Some(&idx) => idx,
                            None => continue,
                        };
                        freq_entries.push(FrequencyEntry {
                            route_index: route_idx,
                            stop_index: from_idx,
                            start_time: freq.start_time,
                            end_time: freq.end_time,
                            headway_secs: freq.headway_secs,
                            next_stop_index: to_idx,
                            travel_time: to.arrival_time.saturating_sub(from.departure_time),
                        });
                    }
                }
            }
        }

        patterns.push(ServicePattern {
            pattern_id: patterns.len() as u32,
            day_mask: *mask,
            date_exceptions_add: adds,
            date_exceptions_remove: removes,
            events,
            min_time,
            max_time,
            frequency_routes: freq_entries,
        });
    }

    patterns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_mask_from_dates_known_days() {
        // 2026-03-29 is a Sunday (bit 6)
        assert_eq!(day_mask_from_dates(&[20260329]), 1 << 6);
        // 2026-03-30 is a Monday (bit 0)
        assert_eq!(day_mask_from_dates(&[20260330]), 1 << 0);
        // 2026-04-01 is a Wednesday (bit 2)
        assert_eq!(day_mask_from_dates(&[20260401]), 1 << 2);
        // 2025-01-01 is a Wednesday (bit 2)
        assert_eq!(day_mask_from_dates(&[20250101]), 1 << 2);
    }

    #[test]
    fn day_mask_from_dates_combines_days() {
        // Mon + Wed + Fri = bits 0,2,4 = 0b0010101 = 21
        assert_eq!(
            day_mask_from_dates(&[20260330, 20260401, 20260403]),
            (1 << 0) | (1 << 2) | (1 << 4),
        );
    }

    #[test]
    fn day_mask_from_dates_all_week() {
        // Mon 2026-03-30 through Sun 2026-04-05
        let dates: Vec<u32> = (30..=31)
            .map(|d| 20260300 + d)
            .chain((1..=5).map(|d| 20260400 + d))
            .collect();
        assert_eq!(day_mask_from_dates(&dates), 0x7F);
    }

    #[test]
    fn day_mask_from_dates_empty() {
        assert_eq!(day_mask_from_dates(&[]), 0);
    }

    fn make_gtfs(
        n_stops: u32,
        routes: &[&str],
        services: &[&str],
        n_trips: u32,
    ) -> GtfsData {
        GtfsData {
            stops: (0..n_stops)
                .map(|i| Stop {
                    id: format!("s{i}"),
                    name: format!("Stop {i}"),
                    lat: 43.0 + i as f64 * 0.001,
                    lon: -79.0 + i as f64 * 0.001,
                    index: i,
                })
                .collect(),
            routes: routes
                .iter()
                .enumerate()
                .map(|(i, &name)| Route {
                    id: name.to_string(),
                    short_name: name.to_string(),
                    index: i as u32,
                })
                .collect(),
            trips: (0..n_trips)
                .map(|i| Trip {
                    id: format!("t{i}"),
                    route_id: routes[0].to_string(),
                    service_id: services[0].to_string(),
                    shape_id: None,
                })
                .collect(),
            stop_times: vec![],
            services: services
                .iter()
                .map(|&id| Service {
                    id: id.to_string(),
                    days: [true, true, true, true, true, false, false],
                    start_date: 20260101,
                    end_date: 20261231,
                    added_dates: vec![],
                    removed_dates: vec![],
                })
                .collect(),
            frequencies: vec![],
            shapes: HashMap::new(),
        }
    }

    #[test]
    fn merge_offsets_indices() {
        let mut a = make_gtfs(3, &["BusA"], &["weekday"], 2);
        let b = make_gtfs(2, &["BusB"], &["weekday"], 1);

        a.merge(b);

        assert_eq!(a.stops.len(), 5);
        assert_eq!(a.routes.len(), 2);
        assert_eq!(a.trips.len(), 3);
        // Stop indices should be offset
        assert_eq!(a.stops[3].index, 3);
        assert_eq!(a.stops[4].index, 4);
        // Route indices should be offset
        assert_eq!(a.routes[1].index, 1);
    }

    #[test]
    fn merge_deduplicates_services() {
        let mut a = make_gtfs(1, &["A"], &["weekday"], 1);
        let mut b = make_gtfs(1, &["B"], &["weekend"], 1);
        // Add a service to b with same id as a's
        b.services.push(Service {
            id: "weekday".to_string(),
            days: [false; 7],
            start_date: 20260101,
            end_date: 20261231,
            added_dates: vec![20260401],
            removed_dates: vec![],
        });

        a.merge(b);

        // "weekday" should be deduped, "weekend" added
        assert_eq!(a.services.len(), 2);
        let weekday = a.services.iter().find(|s| s.id == "weekday").unwrap();
        assert_eq!(weekday.added_dates, vec![20260401]);
    }

    #[test]
    fn merge_empty_into_populated() {
        let mut a = make_gtfs(3, &["Bus"], &["daily"], 5);
        let b = GtfsData {
            stops: vec![],
            routes: vec![],
            trips: vec![],
            stop_times: vec![],
            services: vec![],
            frequencies: vec![],
            shapes: HashMap::new(),
        };
        a.merge(b);
        assert_eq!(a.stops.len(), 3);
        assert_eq!(a.trips.len(), 5);
    }
}
