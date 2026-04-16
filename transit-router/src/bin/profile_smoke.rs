/// Smoke test for profile routing.
/// Usage:
///   cargo run --release --bin profile_smoke -- <city.bin> <src_lat> <src_lon> [YYYYMMDD] [window_start_hhmm] [window_minutes] [max_min] [slack_s]
use std::path::PathBuf;
use transit_router::{data, profile, router};

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
    let date: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(20260413);
    let hhmm: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(900);
    let window_minutes: u32 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(60);
    let max_min: u32 = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(45);
    let slack: u32 = args.get(8).and_then(|s| s.parse().ok()).unwrap_or(60);

    let window_start = (hhmm / 100) * 3600 + (hhmm % 100) * 60;
    let window_end = window_start + window_minutes * 60;
    let max_time = max_min * 60;

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
    let prepared = data::load(buf).expect("load");

    let src = router::snap_to_node(&prepared, src_lat, src_lon).expect("snap source");
    println!("Source node: {src}");
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

    let t0 = std::time::Instant::now();
    let result = profile::run_profile(
        &prepared,
        src,
        window_start,
        window_end,
        date,
        slack,
        max_time,
    );
    let elapsed = t0.elapsed();

    // Stats.
    let mut reachable_walk = 0usize;
    let mut reachable_transit = 0usize;
    let mut total_entries = 0usize;
    let mut max_frontier = 0usize;
    let mut histogram = [0usize; 10]; // 0, 1..2, 3..5, 6..10, 11..20, 21..50, 51..100, 101..200, 201..500, 500+
    for (v, f) in result.frontier.iter().enumerate() {
        if f.is_empty() {
            continue;
        }
        let has_walk = f[0].is_walk_only();
        let n_transit = f.len() - if has_walk { 1 } else { 0 };
        if has_walk {
            reachable_walk += 1;
        }
        if n_transit > 0 {
            reachable_transit += 1;
        }
        total_entries += f.len();
        max_frontier = max_frontier.max(f.len());
        let bucket = match f.len() {
            0 => 0,
            1..=2 => 1,
            3..=5 => 2,
            6..=10 => 3,
            11..=20 => 4,
            21..=50 => 5,
            51..=100 => 6,
            101..=200 => 7,
            201..=500 => 8,
            _ => 9,
        };
        histogram[bucket] += 1;
        if v == src as usize {
            println!("Source frontier len: {}", f.len());
        }
    }

    let reached_any = result.frontier.iter().filter(|f| !f.is_empty()).count();
    println!();
    println!("Profile routing took {:?}", elapsed);
    println!("Nodes reached (any): {reached_any}");
    println!("  walk-only:    {reachable_walk}");
    println!("  transit-any:  {reachable_transit}");
    println!("Total frontier entries: {total_entries}");
    if reached_any > 0 {
        println!(
            "Mean frontier length: {:.2}",
            total_entries as f64 / reached_any as f64
        );
    }
    println!("Max frontier length: {max_frontier}");
    println!("Frontier nodes: {}", result.frontier.iter().filter(|f| !f.is_empty()).count());

    let labels = [
        "0 (unreached)", "1-2", "3-5", "6-10", "11-20", "21-50", "51-100", "101-200", "201-500",
        "500+",
    ];
    println!("Frontier-length histogram:");
    for (i, c) in histogram.iter().enumerate() {
        if *c > 0 {
            println!("  {:>10}: {}", labels[i], c);
        }
    }
}
