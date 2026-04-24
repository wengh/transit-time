use std::path::PathBuf;

use transit_router::profile::{ProfileQuery, ProfileRouter, ProfileRouting};

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

    let query = ProfileQuery {
        source_node: source,
        window_start: 9 * 3600,
        window_end: 10 * 3600,
        date: 20260406, // Monday
        transfer_slack: 60,
        max_time: 3600,
    };

    let _result = ProfileRouting::compute(&prepared, &query, |_, _| {});
}
