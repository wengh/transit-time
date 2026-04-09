use std::path::PathBuf;

fn main() {
    let bin_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("transit-viz/public/data/chicago.bin");

    let data = std::fs::read(&bin_path).unwrap_or_else(|_| {
        panic!("Missing {:?} — run `make data-all` first", bin_path);
    });
    let prepared = transit_router::data::load(&data).expect("Failed to load data");

    let source = transit_router::router::snap_to_node(&prepared, 41.884400, -87.629347).unwrap();
    let mon_patterns = transit_router::router::patterns_for_date(&prepared, 20260406); // Monday

    let samples = 10;
    let window_start = 9 * 3600u32;
    let step = 360u32; // 6 min apart over 1 hour
    let transfer_slack = 60u32;
    let max_time = 3600u32;

    for i in 0..samples {
        let departure = window_start + i * step;
        let _result = transit_router::router::run_tdd_multi(
            &prepared,
            source,
            departure,
            &mon_patterns,
            transfer_slack,
            max_time,
        );
    }
}
