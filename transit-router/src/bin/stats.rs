use std::path::PathBuf;

fn main() {
    let bin_path = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("transit-viz/public/data/chicago.bin")
    });

    println!("Loading {:?} ...", bin_path);
    let compressed = std::fs::read(&bin_path).unwrap_or_else(|_| {
        panic!("Missing {:?} — run `make data-all` first", bin_path);
    });

    let (_data, stats) =
        transit_router::data::load_with_stats(&compressed).expect("Failed to load data");

    println!();
    stats.print();
}
