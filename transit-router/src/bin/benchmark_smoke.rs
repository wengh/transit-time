/// Smoke harness for profile routing. Prints load timings and isochrone stats.
/// Usage:
///   cargo run --release --bin benchmark_smoke -- <city.bin> <src_lat> <src_lon> [YYYYMMDD] [window_start_hhmm] [window_minutes] [max_min] [slack_s] [repeats]
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Duration as ChronoDuration, NaiveDate};
use transit_router::{IsochroneParams, Router, SinceMidnight, TimeWindow};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "Usage: {} <city.bin> <src_lat> <src_lon> [YYYYMMDD] [window_start_hhmm] [window_minutes] [max_min] [slack_s]",
            args[0]
        );
        std::process::exit(1);
    }

    let bin_path = PathBuf::from(&args[1]);
    let src_lat: f64 = args[2].parse().expect("src_lat");
    let src_lon: f64 = args[3].parse().expect("src_lon");
    let date_yyyymmdd: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(20260413);
    let hhmm: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(900);
    let window_minutes: u32 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(60);
    let max_min: u32 = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(45);
    let slack: u32 = args.get(8).and_then(|s| s.parse().ok()).unwrap_or(60);
    let repeats: u32 = args.get(9).and_then(|s| s.parse().ok()).unwrap_or(1).max(1);

    let window_start = (hhmm / 100) * 3600 + (hhmm % 100) * 60;
    let window_end = window_start + window_minutes * 60;

    println!("Loading {:?} ...", bin_path);
    let raw = std::fs::read(&bin_path).expect("read city binary");
    let decompressed;
    let buf: &[u8] = if raw.starts_with(&[0x1f, 0x8b]) {
        let out = std::process::Command::new("gzip")
            .args(["-d", "-c", bin_path.to_str().unwrap()])
            .output()
            .expect("gzip");
        assert!(out.status.success(), "gzip failed");
        decompressed = out.stdout;
        &decompressed[..]
    } else {
        &raw[..]
    };
    // `load_with_stats` so we print the per-section breakdown the README's
    // perf table cites. Then hand the decoded data to `Router::from_prepared`
    // instead of re-decoding via `Router::from_bytes`.
    let (prepared, load_stats) =
        transit_router::data::load_with_stats(buf).expect("load with stats");
    println!();
    load_stats.print();

    let router = Router::from_prepared(Arc::new(prepared));
    let src = router.snap(src_lat, src_lon).expect("snap source");
    println!();
    println!("Source node: {}", src.get());
    println!(
        "Window: {:02}:{:02}–{:02}:{:02} ({} min), max_time={} min, slack={}s",
        window_start / 3600,
        (window_start % 3600) / 60,
        window_end / 3600,
        (window_end % 3600) / 60,
        window_minutes,
        max_min,
        slack,
    );

    let date = decode_yyyymmdd(date_yyyymmdd);
    let params = IsochroneParams {
        source: src,
        date,
        window: TimeWindow {
            start: SinceMidnight::from_seconds(window_start),
            end: SinceMidnight::from_seconds(window_end),
        },
        max_time: ChronoDuration::seconds((max_min * 60) as i64),
        transfer_slack: ChronoDuration::seconds(slack as i64),
        max_parallelism: None,
    };

    let mut timings: Vec<Duration> = Vec::with_capacity(repeats as usize);
    let mut iso_opt = None;
    for i in 0..repeats {
        let t0 = Instant::now();
        let iso = router
            .isochrone(params.clone(), |_, _| ControlFlow::Continue(()))
            .expect("isochrone");
        let dt = t0.elapsed();
        timings.push(dt);
        println!("  run {}/{}: {:.3} s", i + 1, repeats, dt.as_secs_f64());
        iso_opt = Some(iso);
    }
    let iso = iso_opt.unwrap();

    let total: Duration = timings.iter().sum();
    let avg = total / timings.len() as u32;
    let min = *timings.iter().min().unwrap();
    let max = *timings.iter().max().unwrap();
    let elapsed = avg;

    let num_threads = iso.num_threads_used();
    let mean = iso.mean_travel_time();
    let fraction = iso.reachable_fraction();

    // Reachability is signaled by `reachable_fraction > 0`; the mean is
    // undefined (zero-init) for never-reachable nodes.
    let reachable: Vec<u32> = mean
        .iter()
        .zip(fraction.iter())
        .filter(|&(_, &f)| f > 0)
        .map(|(&t, _)| t as u32)
        .collect();

    println!();
    if repeats > 1 {
        println!(
            "Profile routing ({} runs, {} {}): avg {:.3} s, min {:.3} s, max {:.3} s",
            repeats,
            num_threads,
            if num_threads == 1 {
                "thread"
            } else {
                "threads"
            },
            avg.as_secs_f64(),
            min.as_secs_f64(),
            max.as_secs_f64()
        );
    } else {
        println!(
            "Profile routing took {:.3} s using {} {}",
            elapsed.as_secs_f32(),
            num_threads,
            if num_threads == 1 {
                "thread"
            } else {
                "threads"
            },
        );
    }
    println!("Nodes reached: {} / {}", reachable.len(), mean.len());

    println!("{}", iso.stats());

    if !reachable.is_empty() {
        let min_t = reachable.iter().copied().min().unwrap_or(0);
        let max_t = reachable.iter().copied().max().unwrap_or(0);
        let avg_t = reachable.iter().map(|&t| t as u64).sum::<u64>() / reachable.len() as u64;
        println!(
            "Min travel time: {} min, avg: {} min, max: {} min",
            min_t / 60,
            avg_t / 60,
            max_t / 60
        );
        let always_reachable = fraction.iter().filter(|&&f| f == u16::MAX).count();
        let sometimes_reachable = fraction.iter().filter(|&&f| f > 0 && f < u16::MAX).count();
        println!(
            "Always reachable (fraction=1): {always_reachable}, sometimes: {sometimes_reachable}"
        );
    }
}

fn decode_yyyymmdd(date: u32) -> NaiveDate {
    let y = (date / 10_000) as i32;
    let m = (date / 100) % 100;
    let d = date % 100;
    NaiveDate::from_ymd_opt(y, m, d).expect("valid YYYYMMDD")
}
