use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::Path;

pub use transit_data::Color;

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
    pub color: Option<Color>,
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
    pub trip_index: u32,     // index into GtfsData.trips
    pub stop_index: u32,     // index into GtfsData.stops (at parse time; remapped in run_prep)
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

#[derive(Debug)]
pub struct GtfsData {
    pub stops: Vec<Stop>,
    pub routes: Vec<Route>,
    pub trips: Vec<Trip>,
    pub stop_times: Vec<StopTime>,
    pub services: Vec<Service>,
    pub frequencies: Vec<Frequency>,
    pub shapes: HashMap<String, Vec<(f64, f64)>>, // shape_id -> [(lat, lon)]
    /// From feed_info.txt — publisher's authoritative end-of-coverage date
    /// (YYYYMMDD). None if feed_info.txt or its feed_end_date column is absent.
    /// Only meaningful pre-merge; after merging feeds it reflects whichever
    /// feed was the merge base and should not be relied on.
    pub feed_start_date: Option<u32>,
    pub feed_end_date: Option<u32>,
}

impl GtfsData {
    /// Merge another feed into this one.
    ///
    /// All string IDs in `other` are prefixed with `"<ordinal>:"` (the feed's
    /// position in the city's feed list; the first feed is the merge base and
    /// stays unprefixed) before insertion so that stop/trip/route/service IDs
    /// can never collide across feeds. Without this, two feeds that happen to
    /// share a stop ID (e.g. both use "1234") would have their stop_times
    /// cross-mapped to the wrong physical location, producing phantom
    /// "instant" transit legs.
    pub fn merge(&mut self, other: GtfsData, ordinal: usize) {
        let stop_offset = self.stops.len() as u32;
        let route_offset = self.routes.len() as u32;
        let trip_offset = self.trips.len() as u32;
        let p = format!("{ordinal}:");

        for mut stop in other.stops {
            stop.id = format!("{p}{}", stop.id);
            stop.index += stop_offset;
            self.stops.push(stop);
        }
        for mut route in other.routes {
            route.id = format!("{p}{}", route.id);
            route.index += route_offset;
            self.routes.push(route);
        }
        for mut trip in other.trips {
            trip.id = format!("{p}{}", trip.id);
            trip.route_id = format!("{p}{}", trip.route_id);
            trip.service_id = format!("{p}{}", trip.service_id);
            trip.shape_id = trip.shape_id.map(|s| format!("{p}{s}"));
            self.trips.push(trip);
        }
        for mut st in other.stop_times {
            st.trip_index += trip_offset;
            st.stop_index += stop_offset;
            self.stop_times.push(st);
        }
        for mut svc in other.services {
            svc.id = format!("{p}{}", svc.id);
            self.services.push(svc);
        }
        for mut freq in other.frequencies {
            freq.trip_id = format!("{p}{}", freq.trip_id);
            self.frequencies.push(freq);
        }
        let shapes: HashMap<String, Vec<(f64, f64)>> = other
            .shapes
            .into_iter()
            .map(|(k, v)| (format!("{p}{k}"), v))
            .collect();
        self.shapes.extend(shapes);
    }
}

/// A service pattern groups service_ids that share the same day-of-week mask.
#[derive(Debug, Clone)]
pub struct ServicePattern {
    pub pattern_id: u32,
    pub day_mask: u8,    // bit 0=Mon .. bit 6=Sun
    pub start_date: u32, // YYYYMMDD, 0 = unbounded
    pub end_date: u32,   // YYYYMMDD, 0 = unbounded
    pub date_exceptions_add: Vec<u32>,
    pub date_exceptions_remove: Vec<u32>,
    pub events: Vec<(u32, Event)>, // (departure_time, event), in trip order
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
    /// Index of the next FrequencyEntry in the same trip (for through-riding without re-boarding).
    /// u32::MAX if this is the last leg of the trip.
    pub next_freq_index: u32,
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
    #[serde(default)]
    route_color: Option<String>,
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

/// Parse a numeric CSV field, naming the feed, file, data row (1-based,
/// header excluded) and column on failure. A bare `parse().unwrap()` here
/// aborts the whole build inside a rayon worker with no way to tell which
/// of a city's twenty feeds is at fault.
fn parse_field<T: std::str::FromStr>(
    value: &str,
    feed: &str,
    file: &str,
    row: usize,
    field: &str,
) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    value
        .trim()
        .parse::<T>()
        .map_err(|e| anyhow::anyhow!("{feed}: {file} row {row}: invalid {field} {value:?}: {e}"))
}

/// Parse a `YYYYMMDD` field and reject impossible dates (`20240230`) so no
/// later stage can panic on them.
fn parse_date_field(value: &str, feed: &str, file: &str, row: usize, field: &str) -> Result<u32> {
    let date: u32 = parse_field(value, feed, file, row, field)?;
    crate::stale::parse_yyyymmdd(date)
        .map(|_| date)
        .ok_or_else(|| anyhow::anyhow!("{feed}: {file} row {row}: invalid {field} {value:?}"))
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

/// Find the index of a zip entry by filename, handling subdirectory prefixes
/// (e.g. "feed/stop_times.txt" matches target "stop_times.txt").
/// Uses `file_names()` which takes `&self` — no mutable borrow needed.
fn find_zip_entry_name(archive: &zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let target = name.to_lowercase();
    archive
        .file_names()
        .find(|n| {
            let lower = n.to_lowercase();
            lower == target || lower.ends_with(&format!("/{}", target))
        })
        .map(|s| s.to_owned())
}

pub fn parse_gtfs(path: &Path, bbox: (f64, f64, f64, f64)) -> Result<GtfsData> {
    let file =
        std::fs::File::open(path).with_context(|| format!("Failed to open GTFS zip {path:?}"))?;
    let mut archive = zip::ZipArchive::new(file)?;
    // Feed name for error messages: which of a city's feeds is at fault.
    let feed = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let feed = feed.as_str();

    // Parse stops
    let stops_csv =
        read_file_from_zip(&mut archive, "stops.txt")?.context("stops.txt not found in GTFS")?;
    let mut stops = Vec::new();
    let mut stop_id_to_index: HashMap<String, u32> = HashMap::new();
    {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(stops_csv.as_bytes());
        for result in rdr.deserialize::<StopRecord>() {
            let record = result?;
            // Skip non-stop locations (stations, entrances, etc.)
            if let Some(ref lt) = record.location_type
                && lt != "0"
                && !lt.is_empty()
            {
                continue;
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
            .trim(csv::Trim::All)
            .from_reader(routes_csv.as_bytes());
        for result in rdr.deserialize::<RouteRecord>() {
            let record = result?;
            let index = routes.len() as u32;
            route_id_to_index.insert(record.route_id.clone(), index);
            let color = record.route_color.as_ref().and_then(|c| Color::from_hex(c));
            routes.push(Route {
                id: record.route_id,
                short_name: record
                    .route_short_name
                    .or(record.route_long_name)
                    .unwrap_or_default(),
                color,
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
            .trim(csv::Trim::All)
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

    let stop_times = parse_stop_times(
        &mut archive,
        feed,
        &trip_id_to_index,
        &stop_id_to_index,
        &stops,
        trips.len(),
        bbox,
    )?;

    // Parse calendar
    let mut services: HashMap<String, Service> = HashMap::new();
    if let Some(cal_csv) = read_file_from_zip(&mut archive, "calendar.txt")? {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(cal_csv.as_bytes());
        for (row, result) in rdr.deserialize::<CalendarRecord>().enumerate() {
            let record = result?;
            let start_date = parse_date_field(
                &record.start_date,
                feed,
                "calendar.txt",
                row + 1,
                "start_date",
            )?;
            let end_date =
                parse_date_field(&record.end_date, feed, "calendar.txt", row + 1, "end_date")?;
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
                    start_date,
                    end_date,
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
            .trim(csv::Trim::All)
            .from_reader(cal_dates_csv.as_bytes());
        for (row, result) in rdr.deserialize::<CalendarDateRecord>().enumerate() {
            let record = result?;
            let date = parse_date_field(&record.date, feed, "calendar_dates.txt", row + 1, "date")?;
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

    // Parse feed_info.txt — optional file, optional column. Read feed_end_date
    // by header-index lookup so a missing column doesn't error.
    let mut feed_start_date: Option<u32> = None;
    let mut feed_end_date: Option<u32> = None;
    if let Some(fi_csv) = read_file_from_zip(&mut archive, "feed_info.txt")? {
        let get = |col_name: &str| {
            let mut rdr = csv::ReaderBuilder::new()
                .flexible(true)
                .trim(csv::Trim::All)
                .from_reader(fi_csv.as_bytes());
            let headers = rdr.headers().ok()?;
            let col = headers.iter().position(|h| h == col_name)?;
            rdr.records()
                .filter_map(|r| r.ok())
                .filter_map(|rec| rec.get(col).and_then(|v| v.trim().parse::<u32>().ok()))
                .next()
        };
        feed_start_date = get("feed_start_date");
        feed_end_date = get("feed_end_date");
    }

    // Parse frequencies
    let mut frequencies = Vec::new();
    if let Some(freq_csv) = read_file_from_zip(&mut archive, "frequencies.txt")? {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(freq_csv.as_bytes());
        for (row, result) in rdr.deserialize::<FrequencyRecord>().enumerate() {
            let record = result?;
            let (Some(start), Some(end)) =
                (parse_time(&record.start_time), parse_time(&record.end_time))
            else {
                continue;
            };
            // A malformed frequency row only loses that row's departures, so
            // warn and skip rather than fail the city build.
            let headway_secs: u32 = match parse_field(
                &record.headway_secs,
                feed,
                "frequencies.txt",
                row + 1,
                "headway_secs",
            ) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("WARNING: {e:#} — skipping row");
                    continue;
                }
            };
            if headway_secs == 0 {
                eprintln!(
                    "WARNING: {feed}: frequencies.txt row {}: headway_secs is 0 — skipping row",
                    row + 1
                );
                continue;
            }
            // The router boards while `board < end_time`, so a row with
            // `start == end` (Hong Kong has 349 single-departure entries)
            // would never be boardable. Widen it to exactly one departure.
            let end = end.max(start + 1);
            frequencies.push(Frequency {
                trip_id: record.trip_id,
                start_time: start,
                end_time: end,
                headway_secs,
            });
        }
    }

    // Parse shapes — stream directly from zip to avoid loading full CSV as String.
    let mut shapes: HashMap<String, Vec<(f64, f64, u32)>> = HashMap::new();
    if let Some(entry_name) = find_zip_entry_name(&archive, "shapes.txt") {
        let entry = archive
            .by_name(&entry_name)
            .context("Failed to open shapes.txt")?;
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(entry);
        // Reused byte record; the shape id is only copied when first seen.
        // Malformed rows are skipped, as before.
        let headers = rdr.byte_headers()?.clone();
        let col = |name: &str| headers.iter().position(|h| h == name.as_bytes());
        if let (Some(c_id), Some(c_lat), Some(c_lon), Some(c_seq)) = (
            col("shape_id"),
            col("shape_pt_lat"),
            col("shape_pt_lon"),
            col("shape_pt_sequence"),
        ) {
            let mut record = csv::ByteRecord::new();
            while rdr.read_byte_record(&mut record)? {
                let parsed = (|| {
                    let id = std::str::from_utf8(record.get(c_id)?).ok()?;
                    let num = |i: usize| std::str::from_utf8(record.get(i)?).ok();
                    let lat: f64 = num(c_lat)?.parse().ok()?;
                    let lon: f64 = num(c_lon)?.parse().ok()?;
                    let seq: u32 = num(c_seq)?.parse().ok()?;
                    Some((id, lat, lon, seq))
                })();
                let Some((id, lat, lon, seq)) = parsed else {
                    continue;
                };
                match shapes.get_mut(id) {
                    Some(pts) => pts.push((lat, lon, seq)),
                    None => {
                        shapes.insert(id.to_string(), vec![(lat, lon, seq)]);
                    }
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
        services: {
            // HashMap order would make service indices — and with them trip
            // order inside a pattern and event tie order at a stop — vary
            // from run to run.
            let mut v: Vec<Service> = services.into_values().collect();
            v.sort_by(|a, b| a.id.cmp(&b.id));
            v
        },
        frequencies,
        shapes,
        feed_start_date,
        feed_end_date,
    })
}

/// One `stop_times.txt` row with IDs resolved to indices and times parsed.
struct RawStopTime {
    trip_index: u32,
    stop_index: u32,
    arrival: Option<u32>,
    departure: Option<u32>,
    stop_sequence: u32,
}

/// Stream `stop_times.txt` from the zip entry, resolving stop/trip IDs to
/// compact u32 indices on the fly (rows referencing unknown stops or trips
/// are skipped). Streaming avoids materialising the CSV as a String, which
/// can be several GB for large feeds like NYC buses. `on_row` returns
/// `false` to stop early.
fn for_each_stop_time_row(
    archive: &mut zip::ZipArchive<std::fs::File>,
    feed: &str,
    trip_id_to_index: &HashMap<String, u32>,
    stop_id_to_index: &HashMap<String, u32>,
    mut on_row: impl FnMut(RawStopTime) -> bool,
) -> Result<()> {
    let entry_name = find_zip_entry_name(archive, "stop_times.txt")
        .context("stop_times.txt not found in GTFS")?;
    let entry = archive
        .by_name(&entry_name)
        .context("Failed to open stop_times.txt")?;
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(entry);
    // One reused byte record instead of five owned Strings per row: this is
    // the largest file in a feed (NYC: 7.8 million rows), and the per-row
    // allocations were most of the GTFS phase's time and heap churn.
    let headers = rdr.byte_headers()?.clone();
    let col = |name: &str| headers.iter().position(|h| h == name.as_bytes());
    let (Some(c_trip), Some(c_stop), Some(c_seq)) =
        (col("trip_id"), col("stop_id"), col("stop_sequence"))
    else {
        anyhow::bail!("{feed}: stop_times.txt lacks trip_id, stop_id or stop_sequence");
    };
    let (c_arr, c_dep) = (col("arrival_time"), col("departure_time"));
    let mut record = csv::ByteRecord::new();
    let mut row = 0usize;
    while rdr.read_byte_record(&mut record)? {
        row += 1;
        let text = |i: usize| -> Result<&str> {
            std::str::from_utf8(record.get(i).unwrap_or(b""))
                .with_context(|| format!("{feed}: stop_times.txt row {row}: invalid UTF-8"))
        };
        let Some(&trip_index) = trip_id_to_index.get(text(c_trip)?) else {
            continue;
        };
        let Some(&stop_index) = stop_id_to_index.get(text(c_stop)?) else {
            continue;
        };
        let time = |c: Option<usize>| -> Result<Option<u32>> {
            Ok(match c {
                Some(i) => parse_time(text(i)?),
                None => None,
            })
        };
        let raw = RawStopTime {
            trip_index,
            stop_index,
            arrival: time(c_arr)?,
            departure: time(c_dep)?,
            stop_sequence: parse_field(text(c_seq)?, feed, "stop_times.txt", row, "stop_sequence")?,
        };
        if !on_row(raw) {
            break;
        }
    }
    Ok(())
}

/// Accumulates one trip's rows at a time and emits them through
/// [`flush_trip`] on every trip transition, so only O(max stops per trip)
/// raw rows are resident. `push` reports whether the row order is still
/// grouped by trip; a trip that reappears after being flushed means the file
/// is not grouped and the caller must fall back to sorting.
struct TripFlusher<'a> {
    stops: &'a [Stop],
    bbox: (f64, f64, f64, f64),
    buf: Vec<RawStopTime>,
    current: Option<u32>,
    seen: Vec<bool>,
    out: Vec<StopTime>,
    /// Rows dropped because their bracketing timepoints ran backwards.
    dropped: usize,
}

impl<'a> TripFlusher<'a> {
    fn new(stops: &'a [Stop], num_trips: usize, bbox: (f64, f64, f64, f64)) -> Self {
        Self {
            stops,
            bbox,
            buf: Vec::new(),
            current: None,
            seen: vec![false; num_trips],
            out: Vec::new(),
            dropped: 0,
        }
    }

    fn push(&mut self, raw: RawStopTime) -> bool {
        if self.current != Some(raw.trip_index) {
            self.flush();
            if std::mem::replace(&mut self.seen[raw.trip_index as usize], true) {
                return false;
            }
            self.current = Some(raw.trip_index);
        }
        self.buf.push(raw);
        true
    }

    fn flush(&mut self) {
        self.dropped += flush_trip(&mut self.buf, self.stops, self.bbox, &mut self.out);
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.current = None;
        self.seen.iter_mut().for_each(|s| *s = false);
        self.out.clear();
        self.dropped = 0;
    }

    fn finish(mut self) -> (Vec<StopTime>, usize) {
        self.flush();
        (self.out, self.dropped)
    }
}

/// Parse `stop_times.txt` into in-bbox rows with complete, strictly
/// increasing times.
///
/// GTFS feeds almost universally emit stop_times grouped by trip, so the
/// fast path streams the file and flushes on trip transitions. A feed that
/// interleaves trips (Prince George's County "TheBus") used to flush almost
/// every row as its own trip: untimed rows were dropped and the monotonicity
/// pass never saw consecutive stops, leaving 47,928 zero-length legs in
/// `washington_dc.bin`. Such a feed is now detected on the first reappearing
/// trip and re-read in full, sorted by `(trip, stop_sequence)`.
fn parse_stop_times(
    archive: &mut zip::ZipArchive<std::fs::File>,
    feed: &str,
    trip_id_to_index: &HashMap<String, u32>,
    stop_id_to_index: &HashMap<String, u32>,
    stops: &[Stop],
    num_trips: usize,
    bbox: (f64, f64, f64, f64),
) -> Result<Vec<StopTime>> {
    let mut flusher = TripFlusher::new(stops, num_trips, bbox);
    let mut grouped = true;
    for_each_stop_time_row(archive, feed, trip_id_to_index, stop_id_to_index, |raw| {
        grouped = flusher.push(raw);
        grouped
    })?;

    if !grouped {
        eprintln!("WARNING: {feed}: stop_times.txt is not grouped by trip — sorting all rows");
        flusher.reset();
        let mut rows: Vec<RawStopTime> = Vec::new();
        for_each_stop_time_row(archive, feed, trip_id_to_index, stop_id_to_index, |raw| {
            rows.push(raw);
            true
        })?;
        // Stable: rows with equal stop_sequence keep file order.
        rows.sort_by_key(|r| (r.trip_index, r.stop_sequence));
        for raw in rows {
            let ok = flusher.push(raw);
            debug_assert!(ok, "sorted rows must be grouped by trip");
        }
    }

    let (stop_times, dropped) = flusher.finish();
    if dropped > 0 {
        eprintln!(
            "WARNING: {feed}: dropped {dropped} untimed stop_times row(s) whose bracketing timepoints run backwards"
        );
    }
    Ok(stop_times)
}

/// Interpolate missing times, enforce monotonicity and emit the in-bbox rows
/// of one trip from `buf` (drained). Returns the number of rows dropped
/// because their bracketing timepoints ran backwards.
///
/// GTFS allows empty arrival/departure at non-timepoint stops; consumers
/// must interpolate (spec §stop_times.txt). Hong Kong, for example, only
/// timestamps the first and last stop of each trip. Only stops within the
/// bbox are emitted so the stop_times Vec stays small even for large feeds
/// (e.g. UK Rail with 5M null-timed rows worldwide); out-of-bbox stops are
/// still used as interpolation anchors.
fn flush_trip(
    buf: &mut Vec<RawStopTime>,
    stops: &[Stop],
    bbox: (f64, f64, f64, f64),
    out: &mut Vec<StopTime>,
) -> usize {
    if buf.is_empty() {
        return 0;
    }
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    let mut dropped = 0usize;
    buf.sort_by_key(|r| r.stop_sequence);
    let n = buf.len();
    let mut i = 0;
    while i < n {
        let dep_known = buf[i].departure.or(buf[i].arrival);
        let arr_known = buf[i].arrival.or(buf[i].departure);
        if dep_known.is_some() && arr_known.is_some() {
            buf[i].departure = dep_known;
            buf[i].arrival = arr_known;
            i += 1;
        } else {
            // Find bracketing timepoints.
            let prev = (0..i)
                .rev()
                .find(|&j| buf[j].arrival.is_some() || buf[j].departure.is_some());
            let next = (i..n).find(|&j| buf[j].arrival.is_some() || buf[j].departure.is_some());
            match (prev, next) {
                (Some(p), Some(q)) if q > i => {
                    let t_p = buf[p].departure.or(buf[p].arrival).unwrap() as i64;
                    let t_q = buf[q].arrival.or(buf[q].departure).unwrap() as i64;
                    if t_q < t_p {
                        // Timepoints run backwards: nothing sensible to
                        // interpolate. Leave the rows untimed so they are
                        // dropped below rather than given garbage times
                        // (the u32 subtraction used to wrap here).
                        dropped += (i..q).filter(|&k| buf[k].arrival.is_none()).count();
                    } else {
                        let span = (q - p) as i64;
                        for (k, r) in buf.iter_mut().enumerate().take(q).skip(i) {
                            if r.arrival.is_some() {
                                continue;
                            }
                            let t = (t_p + (k - p) as i64 * (t_q - t_p) / span) as u32;
                            r.arrival = Some(t);
                            r.departure = Some(t);
                        }
                    }
                    i = q;
                }
                _ => {
                    i += 1;
                }
            }
        }
    }
    // Enforce strict monotonicity across the trip: every stop's
    // arrival must be at least 1s after the previous stop's departure,
    // and departure >= arrival. Many feeds emit minute-rounded times
    // (HH:MM:00 at every stop), so two consecutive bus stops a half
    // minute apart end up with `to.arrival == from.departure`.
    //
    // Downstream uses `travel_time == 0` (i.e. `to.arrival ==
    // from.departure`) as the trip-end sentinel marker, so a
    // non-sentinel zero-second leg would be indistinguishable from a
    // trip terminator. Bumping forward by 1s per tie stretches the
    // trip by at most a few seconds — negligible vs. the rounding
    // already baked into the source data.
    let mut prev_dep: Option<u32> = None;
    for r in buf.iter_mut() {
        let (Some(arr), Some(dep)) = (r.arrival, r.departure) else {
            continue;
        };
        let new_arr = match prev_dep {
            Some(pd) if arr <= pd => pd + 1,
            _ => arr,
        };
        let new_dep = new_arr.max(dep);
        r.arrival = Some(new_arr);
        r.departure = Some(new_dep);
        prev_dep = Some(new_dep);
    }

    for r in buf.drain(..) {
        if let (Some(arr), Some(dep)) = (r.arrival, r.departure) {
            let s = &stops[r.stop_index as usize];
            if s.lat >= min_lat && s.lat <= max_lat && s.lon >= min_lon && s.lon <= max_lon {
                out.push(StopTime {
                    trip_index: r.trip_index,
                    stop_index: r.stop_index,
                    arrival_time: arr,
                    departure_time: dep,
                    stop_sequence: r.stop_sequence,
                });
            }
        }
    }
    dropped
}

/// The stop_times of one trip, in stop_sequence order. `stop_times` must be
/// sorted by `(trip_index, stop_sequence)`; the slice is found by binary
/// search, so no per-trip grouping structure is needed.
pub fn trip_stop_times(stop_times: &[StopTime], trip_idx: u32) -> &[StopTime] {
    let start = stop_times.partition_point(|st| st.trip_index < trip_idx);
    let end = start + stop_times[start..].partition_point(|st| st.trip_index <= trip_idx);
    &stop_times[start..end]
}

pub fn build_service_patterns(data: &GtfsData) -> Vec<ServicePattern> {
    // Build mappings
    let mut trip_id_to_idx: HashMap<&str, u32> = HashMap::new();
    for (i, trip) in data.trips.iter().enumerate() {
        trip_id_to_idx.insert(&trip.id, i as u32);
    }

    let mut route_id_to_idx: HashMap<&str, u32> = HashMap::new();
    for route in &data.routes {
        route_id_to_idx.insert(&route.id, route.index);
    }

    #[derive(PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct ServiceKey {
        mask: u8,
        start_date: u32,
        end_date: u32,
        added_dates: Vec<u32>,
        removed_dates: Vec<u32>,
    }

    // A service defined only through calendar_dates.txt keeps `mask == 0`:
    // the router then activates it on its added dates alone (see
    // `patterns_for_date`). Synthesising a weekday mask from those dates
    // with an unbounded date range — as this used to do — made every
    // single-date service recur weekly forever, so all of GO Transit's
    // holiday and construction variants ran at once on any weekday. The only
    // place such a service is deliberately widened is the stale policy.
    let mut day_mask_groups: BTreeMap<ServiceKey, Vec<&Service>> = BTreeMap::new();
    for service in &data.services {
        let mask = crate::stale::day_mask(&service.days);
        let mut added_dates = service.added_dates.clone();
        added_dates.sort_unstable();
        added_dates.dedup();
        let mut removed_dates = service.removed_dates.clone();
        removed_dates.sort_unstable();
        removed_dates.dedup();

        let key = ServiceKey {
            mask,
            start_date: service.start_date,
            end_date: service.end_date,
            added_dates,
            removed_dates,
        };
        day_mask_groups.entry(key).or_default().push(service);
    }

    // stop_times are pre-sorted by (trip_index, stop_sequence) by the caller
    // and pre-filtered to in-bbox stops, so `trip_stop_times` can slice.
    let trip_stops = |trip_idx: u32| trip_stop_times(&data.stop_times, trip_idx);

    // Group trips by service_id for O(1) per-pattern access instead of scanning all trips.
    let mut trips_by_service_id: HashMap<&str, Vec<&Trip>> = HashMap::new();
    for trip in &data.trips {
        trips_by_service_id
            .entry(trip.service_id.as_str())
            .or_default()
            .push(trip);
    }

    // Frequency-based trip IDs
    let freq_trip_ids: HashSet<&str> = data
        .frequencies
        .iter()
        .map(|f| f.trip_id.as_str())
        .collect();

    // Convert BTreeMap to Vec to preserve sorted order for enumerate() indices,
    // then build each pattern independently in parallel — all shared data is read-only.
    let groups: Vec<(ServiceKey, Vec<&Service>)> = day_mask_groups.into_iter().collect();
    let patterns: Vec<ServicePattern> = groups
        .into_par_iter()
        .enumerate()
        .map(|(pattern_id, (key, services))| {
            // Every service in the group shares the key's mask, date range
            // and (sorted) exception lists, so take them from the key once.
            // Appending each service's copy made Waterloo write 12,321
            // add-dates of which 11,170 were duplicates, all scanned
            // linearly by `patterns_for_date`.
            let ServiceKey {
                mask,
                start_date,
                end_date,
                added_dates: adds,
                removed_dates: removes,
            } = key;
            // Ordered set: iteration order is what makes the pattern's event
            // order reproducible (a HashSet varied it from run to run).
            let service_ids: BTreeSet<&str> = services.iter().map(|s| s.id.as_str()).collect();

            // Find min/max departure times for trips in this pattern
            let mut min_time = u32::MAX;
            let mut max_time = 0u32;

            // Collect all departure events
            let mut departure_events: Vec<(u32, Event)> = Vec::new(); // (departure_time, event)

            for service_id in &service_ids {
                let Some(trips) = trips_by_service_id.get(service_id) else {
                    continue;
                };
                for trip in trips {
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

                    // stop_times are pre-sorted and in-bbox only; binary search gives the slice.
                    let times = trip_stops(trip_idx);
                    if times.len() >= 2 {
                        for window in times.windows(2) {
                            let from = &window[0];
                            let to = &window[1];

                            let from_idx = from.stop_index;
                            let to_idx = to.stop_index;
                            if from_idx == to_idx {
                                continue;
                            }

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
            }

            if min_time > max_time {
                min_time = 0;
                max_time = 0;
            }

            // Left in trip order: the writer sorts by (trip, time) and then
            // by (stop, time) itself, and nothing in between needs time order.
            let events = departure_events;

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
                    let times = trip_stops(trip_idx);
                    if times.len() >= 2 {
                        let trip_start = freq_entries.len();
                        for window in times.windows(2) {
                            let from = &window[0];
                            let to = &window[1];
                            let from_idx = from.stop_index;
                            let to_idx = to.stop_index;
                            if from_idx == to_idx {
                                continue;
                            }
                            freq_entries.push(FrequencyEntry {
                                route_index: route_idx,
                                stop_index: from_idx,
                                start_time: freq.start_time,
                                end_time: freq.end_time,
                                headway_secs: freq.headway_secs,
                                next_stop_index: to_idx,
                                travel_time: to.arrival_time.saturating_sub(from.departure_time),
                                next_freq_index: u32::MAX,
                            });
                        }
                        // Link consecutive legs of this trip for through-riding.
                        let trip_end = freq_entries.len();
                        for (j, entry) in freq_entries
                            .iter_mut()
                            .enumerate()
                            .take(trip_end.saturating_sub(1))
                            .skip(trip_start)
                        {
                            entry.next_freq_index = (j + 1) as u32;
                        }
                    }
                }
            }

            ServicePattern {
                pattern_id: pattern_id as u32,
                day_mask: mask,
                start_date,
                end_date,
                date_exceptions_add: adds,
                date_exceptions_remove: removes,
                events,
                min_time,
                max_time,
                frequency_routes: freq_entries,
            }
        })
        .collect();

    patterns
}
