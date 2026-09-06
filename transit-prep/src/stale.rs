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
//!
//! Services defined only in `calendar_dates.txt` have no window to extend;
//! when their dates reach the feed's horizon they are made to recur weekly
//! (on the weekdays of their dates) beyond that horizon instead. This is the
//! only place such a service is given a weekly recurrence — elsewhere it runs
//! on its listed dates alone.

use crate::gtfs;
use chrono::{Datelike, NaiveDate};

pub fn unix_epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
}

pub fn unix_days_now() -> u32 {
    (chrono::Utc::now().date_naive() - unix_epoch()).num_days() as u32
}

/// Decode a GTFS `YYYYMMDD` integer. `None` for impossible dates such as
/// `20240230`. All dates are validated with this at parse time
/// (`gtfs::parse_gtfs`), so later stages may `expect` it.
pub fn parse_yyyymmdd(date: u32) -> Option<NaiveDate> {
    let y = (date / 10000) as i32;
    let m = (date / 100) % 100;
    let d = date % 100;
    NaiveDate::from_ymd_opt(y, m, d)
}

pub fn yyyymmdd_to_days(date: u32) -> u32 {
    let nd = parse_yyyymmdd(date).expect("invalid YYYYMMDD date");
    (nd - unix_epoch()).num_days() as u32
}

fn days_to_yyyymmdd(days: u32) -> u32 {
    let d = unix_epoch() + chrono::Duration::days(days as i64);
    d.year() as u32 * 10000 + d.month() * 100 + d.day()
}

/// Pack a `calendar.txt` day row into a bit mask (bit 0 = Mon .. bit 6 = Sun).
pub fn day_mask(days: &[bool; 7]) -> u8 {
    days.iter()
        .enumerate()
        .fold(0u8, |acc, (i, &d)| if d { acc | (1 << i) } else { acc })
}

/// Weekday mask covering every weekday that occurs in `dates` (YYYYMMDD).
fn day_mask_from_dates(dates: &[u32]) -> u8 {
    dates.iter().fold(0u8, |mask, &d| {
        let dow = parse_yyyymmdd(d)
            .expect("invalid YYYYMMDD date")
            .weekday()
            .num_days_from_monday();
        mask | (1 << dow)
    })
}

/// A service with no `calendar.txt` row: it exists only through its
/// `calendar_dates.txt` exceptions and has no weekly recurrence of its own.
fn is_calendar_dates_only(s: &gtfs::Service) -> bool {
    s.start_date == 0 && s.end_date == 0 && s.days.iter().all(|&d| !d)
}

/// Give a calendar-dates-only service a weekly recurrence on the weekdays its
/// added dates fall on, valid over `[start_date, end_date]` (YYYYMMDD, 0 =
/// unbounded). The listed dates keep activating it inside the feed's own
/// horizon; the recurrence is meant to cover only the dates the feed does not
/// reach, so callers pass a window that starts after (or ends before) it.
/// This is the "extend to unbounded" counterpart for services that have no
/// date window to extend.
fn recur_weekly(s: &mut gtfs::Service, start_date: u32, end_date: u32) {
    let mask = day_mask_from_dates(&s.added_dates);
    for (i, day) in s.days.iter_mut().enumerate() {
        *day = mask & (1 << i) != 0;
    }
    s.start_date = start_date;
    s.end_date = end_date;
}

/// `(start, end)` of a service in days since the epoch, with `u32::MIN` /
/// `u32::MAX` for unbounded ends. A calendar-dates-only service's window is
/// the span of its added dates.
fn service_window(s: &gtfs::Service) -> (u32, u32) {
    if is_calendar_dates_only(s) {
        if let (Some(&first), Some(&last)) =
            (s.added_dates.iter().min(), s.added_dates.iter().max())
        {
            return (yyyymmdd_to_days(first), yyyymmdd_to_days(last));
        }
    }
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

    // Precompute (start_days, end_days) per service with sentinel values so
    // unbounded endpoints compare correctly. u32::MIN for "no start",
    // u32::MAX for "no end" (i.e. the service runs forever already).
    let service_info: Vec<(u32, u32)> = data.services.iter().map(service_window).collect();
    let feed_first = service_info
        .iter()
        .map(|&(start, _)| start)
        .filter(|&d| d != u32::MIN)
        .min();
    let feed_last = service_info
        .iter()
        .map(|&(_, end)| end)
        .filter(|&d| d != u32::MAX)
        .max();
    // Recurrence windows for calendar-dates-only services: the day after the
    // feed's last date onwards, or up to the day before its first date.
    let after_feed = feed_last.map_or(0, |last| days_to_yyyymmdd(last + 1));
    let before_feed = feed_first.map_or(0, |first| days_to_yyyymmdd(first.saturating_sub(1)));

    match allow_stale {
        Some(false) => return,
        Some(true) => {
            for s in &mut data.services {
                if is_calendar_dates_only(s) {
                    recur_weekly(s, after_feed, 0);
                } else {
                    s.start_date = 0;
                    s.end_date = 0;
                }
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

        // Calendar-dates-only services have no window to extend. Those whose
        // dates reach the feed's horizon are made to recur weekly beyond it;
        // one that ends earlier was superseded by later dates (a holiday
        // variant, a construction detour) and must not leak into every week.
        // The successor test is not used here because a feed that models each
        // day as its own service_id (GO Transit) would leave only the very
        // last day standing.
        if is_calendar_dates_only(s) {
            let reaches_end = do_stale
                && a_end != u32::MAX
                && feed_last.is_some_and(|last| a_end + THRESHOLD_DAYS >= last);
            let reaches_start = do_new
                && a_start != u32::MIN
                && feed_first.is_some_and(|first| a_start <= first + THRESHOLD_DAYS);
            if reaches_end || reaches_start {
                let start = if reaches_start { 0 } else { after_feed };
                let end = if reaches_end { 0 } else { before_feed };
                eprintln!(
                    "  {}: calendar-dates-only service '{}' ({} date(s)) → recurs weekly over {}-{}",
                    if reaches_end { "stale" } else { "too-new" },
                    s.id,
                    s.added_dates.len(),
                    start,
                    end
                );
                recur_weekly(s, start, end);
            }
            continue;
        }

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
