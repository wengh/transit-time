/// Loads a city binary, runs profile routing from a fixed origin, then
/// reconstructs paths to every reachable destination. Intended to surface
/// the path-reconstruction panic the user reported on a Chicago build that
/// was preprocessed with the new deg-2 collapse.
///
/// Usage: cargo run --release --bin repro_panic -- <city.bin>
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use transit_router::profile::{ProfileQuery, ProfileRouter as _, SplitProfileRouting};

fn main() {
    let bin = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: repro_panic <city.bin>"),
    );
    let raw = std::fs::read(&bin).expect("read");
    let decompressed;
    let bytes: &[u8] = if raw.starts_with(&[0x1f, 0x8b]) {
        let out = std::process::Command::new("gzip")
            .args(["-d", "-c", bin.to_str().unwrap()])
            .output()
            .expect("gzip");
        assert!(out.status.success());
        decompressed = out.stdout;
        &decompressed[..]
    } else {
        &raw[..]
    };
    let prepared = transit_router::data::load(bytes).expect("load");

    // Source from todo.md line 81: 41.883251, -87.627007 (Chicago Loop)
    let source = transit_router::router::snap_to_node(&prepared, 41.883251, -87.627007).unwrap();
    let query = ProfileQuery {
        source_node: source,
        window_start: 11 * 3600,
        window_end: 12 * 3600,
        date: 20260416, // Thursday, per todo.md line 85
        transfer_slack: 60,
        max_time: 45 * 60,
    };

    let routing = match SplitProfileRouting::compute(&prepared, &query, |_, _| {
        std::ops::ControlFlow::Continue(())
    }) {
        std::ops::ControlFlow::Continue(r) => r,
        std::ops::ControlFlow::Break(()) => unreachable!("repro progress never cancels"),
    };

    let target: Option<u32> = std::env::args().nth(2).and_then(|s| s.parse().ok());
    if let Some(dst) = target {
        println!("--- target destination {} ---", dst);
        let _ = routing.optimal_paths(&prepared, dst);
        return;
    }

    println!("Reconstructing paths to every node ...");
    let n = prepared.num_nodes;
    let done = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let progress_done = Arc::clone(&done);
    let progress_finished = Arc::clone(&finished);
    let progress = std::thread::spawn(move || {
        while !progress_finished.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(1));
            report_progress(progress_done.load(Ordering::Relaxed), n, started);
        }
    });

    let (total, mut panicked_at) = (0..n as u32)
        .into_par_iter()
        .map(|dst| {
            let outcome = reconstruct_destination(&prepared, &routing, dst);
            done.fetch_add(1, Ordering::Relaxed);
            outcome
        })
        .fold(
            || (0usize, Vec::new()),
            |(mut reachable, mut panics), outcome| {
                match outcome {
                    DestinationOutcome::Reachable => reachable += 1,
                    DestinationOutcome::Panicked(dst) => panics.push(dst),
                    DestinationOutcome::Unreachable => {}
                }
                (reachable, panics)
            },
        )
        .reduce(
            || (0usize, Vec::new()),
            |(left_reachable, mut left_panics), (right_reachable, right_panics)| {
                left_panics.extend(right_panics);
                (left_reachable + right_reachable, left_panics)
            },
        );

    finished.store(true, Ordering::Relaxed);
    progress.join().expect("progress reporter thread panicked");
    panicked_at.sort_unstable();
    println!();
    println!(
        "Finished path reconstruction in {}",
        format_duration(started.elapsed())
    );
    println!(
        "Reconstructed {} reachable destinations; {} panicked",
        total,
        panicked_at.len()
    );
    if !panicked_at.is_empty() {
        eprintln!(
            "first 5 panic destinations: {:?}",
            &panicked_at[..panicked_at.len().min(5)]
        );
        std::process::exit(1);
    }
}

enum DestinationOutcome {
    Reachable,
    Unreachable,
    Panicked(u32),
}

fn reconstruct_destination(
    prepared: &transit_router::data::PreparedData,
    routing: &SplitProfileRouting,
    dst: u32,
) -> DestinationOutcome {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        routing.optimal_paths(prepared, dst)
    }));
    match result {
        Ok(paths) if paths.is_empty() => DestinationOutcome::Unreachable,
        Ok(_) => DestinationOutcome::Reachable,
        Err(_) => DestinationOutcome::Panicked(dst),
    }
}

fn report_progress(done: usize, total: usize, started: Instant) {
    let elapsed = started.elapsed();
    let rate = if elapsed.is_zero() {
        0.0
    } else {
        done as f64 / elapsed.as_secs_f64()
    };
    let eta = if done == 0 || rate <= f64::EPSILON {
        None
    } else {
        Some(Duration::from_secs_f64((total - done) as f64 / rate))
    };

    eprintln!(
        "progress: {}/{} ({:.1}%), {:.0} nodes/s, elapsed {}, eta {}",
        done,
        total,
        if total == 0 {
            100.0
        } else {
            done as f64 * 100.0 / total as f64
        },
        rate,
        format_duration(elapsed),
        eta.map(format_duration)
            .unwrap_or_else(|| "unknown".to_string())
    );
}

fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}
