//! Stale-calendar policy for GTFS feeds.
//!
//! Feeds frequently lag behind the date the router will be queried on. To keep
//! today's queries answerable, we optionally extend a service's date window
//! when:
//!   * the feed's authoritative `feed_end_date` is already in the past
//!     (the "stale" case — push `end_date` to unbounded), or
//!   * `feed_start_date` is still in the future (the "too-new" case — push
//!     `start_date` to unbounded).
//!
//! Services with a near-adjacent successor (stale) or predecessor (too-new)
//! are left alone so the router naturally hands off between calendars on the
//! intended date.

use crate::gtfs;
use chrono::NaiveDate;

pub fn unix_epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
}

pub fn unix_days_now() -> u32 {
    (chrono::Utc::now().date_naive() - unix_epoch()).num_days() as u32
}

pub fn yyyymmdd_to_days(date: u32) -> u32 {
    let y = (date / 10000) as i32;
    let m = (date / 100) % 100;
    let d = date % 100;
    let nd = NaiveDate::from_ymd_opt(y, m, d).expect("invalid YYYYMMDD date");
    (nd - unix_epoch()).num_days() as u32
}

/// Warn if the last service date in `data` is more than 1 day before today.
pub fn warn_if_expired(feed_id: &str, data: &gtfs::GtfsData) {
    let last = data
        .services
        .iter()
        .flat_map(|s| {
            s.added_dates.iter().copied().chain(if s.end_date != 0 {
                Some(s.end_date)
            } else {
                None
            })
        })
        .max();
    if let Some(last_date) = last {
        let today = unix_days_now();
        let last_days = yyyymmdd_to_days(last_date);
        if last_days + 1 < today {
            eprintln!(
                "WARNING: feed '{}' last service date is {} — {} day(s) ago",
                feed_id,
                last_date,
                today - last_days,
            );
        }
    }
}

/// Adjust per-feed service calendars for stale or not-yet-started feeds so the
/// data remains useful for isochrone queries on today's date.
pub fn apply_stale_policy(data: &mut gtfs::GtfsData, allow_stale: Option<bool>, today_days: u32) {
    const THRESHOLD_DAYS: u32 = 7;

    match allow_stale {
        Some(false) => return,
        Some(true) => {
            for s in &mut data.services {
                s.start_date = 0;
                s.end_date = 0;
            }
            return;
        }
        None => {}
    }

    // Gate on the publisher's authoritative dates from feed_info.txt only.
    // If a date isn't specified, we can't tell whether the feed covers today,
    // so be conservative and apply the corresponding extension.
    let do_stale = data
        .feed_end_date
        .filter(|&d| d != 0)
        .map(yyyymmdd_to_days)
        .map_or(true, |m| today_days + THRESHOLD_DAYS > m);

    let do_new = data
        .feed_start_date
        .filter(|&d| d != 0)
        .map(yyyymmdd_to_days)
        .map_or(true, |m| m + THRESHOLD_DAYS > today_days);

    if !do_stale && !do_new {
        return;
    }

    eprintln!(
        "Applying stale policy: feed date from {:?} to {:?} → do_stale={}, do_new={}",
        data.feed_start_date, data.feed_end_date, do_stale, do_new,
    );

    // Precompute (start_days, end_days) per service with sentinel values so
    // unbounded endpoints compare correctly. u32::MIN for "no start",
    // u32::MAX for "no end" (i.e. the service runs forever already).
    let service_info: Vec<(u32, u32)> = data
        .services
        .iter()
        .map(|s| {
            (
                if s.start_date != 0 {
                    yyyymmdd_to_days(s.start_date)
                } else {
                    u32::MIN
                },
                if s.end_date != 0 {
                    yyyymmdd_to_days(s.end_date)
                } else {
                    u32::MAX
                },
            )
        })
        .collect();

    let has_successor: Vec<bool> = service_info
        .iter()
        .enumerate()
        .map(|(i, &(_, a_end))| {
            if a_end == u32::MAX {
                return false;
            }
            let handoff = a_end as i64 + 1;
            service_info
                .iter()
                .enumerate()
                .any(|(j, &(b_start, b_end))| {
                    if i == j {
                        return false;
                    }
                    if b_start == u32::MIN {
                        return false;
                    }
                    if b_end <= a_end {
                        return false;
                    }
                    (b_start as i64 - handoff).abs() <= THRESHOLD_DAYS as i64
                })
        })
        .collect();

    let has_predecessor: Vec<bool> = service_info
        .iter()
        .enumerate()
        .map(|(i, &(a_start, _))| {
            if a_start == u32::MIN {
                return false;
            }
            let handoff = a_start as i64 - 1;
            service_info
                .iter()
                .enumerate()
                .any(|(j, &(b_start, b_end))| {
                    if i == j {
                        return false;
                    }
                    if b_end == u32::MAX {
                        return false;
                    }
                    if b_start >= a_start {
                        return false;
                    }
                    (b_end as i64 - handoff).abs() <= THRESHOLD_DAYS as i64
                })
        })
        .collect();

    for (i, s) in data.services.iter_mut().enumerate() {
        let (a_start, a_end) = service_info[i];

        if do_stale && a_end != u32::MAX && !has_successor[i] {
            eprintln!(
                "  stale: extending service '{}' ({}-{}) end → unbounded",
                s.id, s.start_date, s.end_date
            );
            s.end_date = 0;
        }

        if do_new && a_start != u32::MIN && !has_predecessor[i] {
            eprintln!(
                "  too-new: extending service '{}' ({}-{}) start → unbounded",
                s.id, s.start_date, s.end_date
            );
            s.start_date = 0;
        }
    }
}
