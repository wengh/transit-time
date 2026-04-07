use std::io::Read;
use std::path::Path;
use std::time::Instant;

/// Profile the router: run SSSP 10 times across an hour window and report timing.
///
/// Source: 41.884400, -87.629347
/// Date: Monday (day_of_week = 0)
/// Departure window: 09:00–10:00, 10 evenly spaced samples
/// Max travel time: 60 min
/// Transfer slack: 60s
#[test]
fn profile_hour_window() {
    let bin_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("transit-viz/public/data/chicago.bin");

    if !bin_path.exists() {
        eprintln!("Skipping: {:?} not found", bin_path);
        return;
    }

    let compressed = std::fs::read(&bin_path).expect("Failed to read binary");
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut data)
        .expect("Failed to decompress gzip");
    let (prepared, stats) =
        transit_router::data::load_with_stats(&data).expect("Failed to load data");
    stats.print();

    let snap_start = Instant::now();
    let source = transit_router::router::snap_to_node(&prepared, 41.884400, -87.629347);
    let snap_ms = snap_start.elapsed().as_micros();
    eprintln!(
        "snap_to_node: {}µs -> node {} ({}, {})",
        snap_ms, source, prepared.nodes[source as usize].lat, prepared.nodes[source as usize].lon
    );

    // Monday
    let mon_patterns = transit_router::router::patterns_for_date(&prepared, 20260406);
    eprintln!("Monday patterns: {} total", mon_patterns.len());

    let samples = 10;
    let window_start = 9 * 3600u32; // 09:00
    let window_end = 10 * 3600u32; // 10:00
    let step = (window_end - window_start) / samples;
    let transfer_slack = 60u32;
    let max_time = 3600u32; // 60 min

    let mut timings_us: Vec<u128> = Vec::new();
    let mut reachable_counts: Vec<usize> = Vec::new();
    let mut transit_counts: Vec<usize> = Vec::new();

    eprintln!(
        "\n{:<8} {:>10} {:>10} {:>10}",
        "Depart", "Time(ms)", "Reached", "Transit"
    );
    eprintln!("{}", "-".repeat(42));

    for i in 0..samples {
        let departure = window_start + i * step;
        let h = departure / 3600;
        let m = (departure % 3600) / 60;

        let start = Instant::now();
        let result = transit_router::router::run_tdd_multi(
            &prepared,
            source,
            departure,
            &mon_patterns,
            transfer_slack,
            max_time,
        );
        let elapsed = start.elapsed().as_micros();

        let reachable = result
            .iter()
            .filter(|r| r.arrival_delta != u16::MAX)
            .count();
        let via_transit = result.iter().filter(|r| r.route_index != u32::MAX).count();

        eprintln!(
            "{:02}:{:02}    {:>7.1}ms {:>10} {:>10}",
            h,
            m,
            elapsed as f64 / 1000.0,
            reachable,
            via_transit
        );

        timings_us.push(elapsed);
        reachable_counts.push(reachable);
        transit_counts.push(via_transit);
    }

    let total_us: u128 = timings_us.iter().sum();
    let avg_us = total_us / samples as u128;
    let min_us = *timings_us.iter().min().unwrap();
    let max_us = *timings_us.iter().max().unwrap();
    let avg_reached: usize = reachable_counts.iter().sum::<usize>() / samples as usize;

    eprintln!("\n=== Summary ({} runs) ===", samples);
    eprintln!(
        "Avg: {:.1}ms  Min: {:.1}ms  Max: {:.1}ms",
        avg_us as f64 / 1000.0,
        min_us as f64 / 1000.0,
        max_us as f64 / 1000.0
    );
    eprintln!("Avg reachable nodes: {}", avg_reached);
}
