//! Thin CLI over [`transit_prep::prepare`]. Local files in, `.bin` out.
//!
//! For end-to-end workflows that download GTFS/OSM, query Transitland, or
//! run a multi-city pipeline, use the `city-builder` crate instead.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "transit-prep")]
#[command(about = "Build a city.bin from local GTFS + OSM files")]
struct Cli {
    /// GTFS .zip file. Pass --gtfs multiple times to merge feeds.
    #[arg(long, required = true)]
    gtfs: Vec<PathBuf>,

    /// OSM extract (PBF or XML).
    #[arg(long)]
    osm: PathBuf,

    /// Bounding box: min_lon,min_lat,max_lon,max_lat (decimal degrees).
    #[arg(long)]
    bbox: String,

    /// City identifier (used in log lines).
    #[arg(long, default_value = "city")]
    id: String,

    /// Output `.bin` path.
    #[arg(long, default_value = "city.bin")]
    output: PathBuf,

    /// Override the stale-calendar policy. Omit for the default heuristic.
    /// `--allow-stale=true` forces unbounded service windows;
    /// `--allow-stale=false` disables the policy.
    #[arg(long)]
    allow_stale: Option<bool>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let bbox = transit_prep::parse_bbox(&cli.bbox)?;
    transit_prep::prepare(
        &cli.id,
        &cli.gtfs,
        &cli.osm,
        bbox,
        &cli.output,
        cli.allow_stale,
    )
}
