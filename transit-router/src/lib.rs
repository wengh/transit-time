pub use transit_data as data;
pub mod api;
pub mod path_display;
pub mod profile;
pub mod router;

pub use api::{
    Entry, Isochrone, IsochroneParams, NodeId, RouteId, Router, RouterError, SinceMidnight, StopId,
    TimeWindow,
};
pub use profile::{Path, PathSegment, SegmentKind};

use rayon::iter::IntoParallelIterator;
use rayon::prelude::*;

/// Whether the rayon thread pool has been initialized (via `initThreadPool` from JS).
/// When false, we fall back to sequential iteration.
static RAYON_INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called by the WASM wrapper after `wasm-bindgen-rayon` has initialised the
/// thread pool. Until this fires, [`maybe_par_collect`] / [`maybe_par_unzip`]
/// run sequentially on wasm. Native targets are always parallel.
pub fn mark_rayon_ready() {
    RAYON_INITIALIZED.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn rayon_available() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        RAYON_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        true
    }
}

/// Map `f` over `iter`, using rayon if available.
pub fn maybe_par_collect<I, R, F>(iter: I, f: F) -> Vec<R>
where
    I: IntoParallelIterator + IntoIterator<Item = <I as IntoParallelIterator>::Item>,
    R: Send,
    F: Fn(<I as IntoParallelIterator>::Item) -> R + Sync + Send,
{
    if crate::rayon_available() {
        iter.into_par_iter().map(&f).collect()
    } else {
        iter.into_iter().map(f).collect()
    }
}

/// Map `f(i, &mut slice[i])` over a slice, mutating each element in place and
/// collecting the per-index return value into a fresh `Vec<R>`. Uses rayon
/// when available. The companion to `maybe_par_collect` for the case where
/// the closure also needs to update some persistent per-index state.
pub fn maybe_par_map_mut_collect<T, R, F>(slice: &mut [T], f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(usize, &mut T) -> R + Sync + Send,
{
    if crate::rayon_available() {
        slice
            .par_iter_mut()
            .enumerate()
            .map(|(i, x)| f(i, x))
            .collect()
    } else {
        slice.iter_mut().enumerate().map(|(i, x)| f(i, x)).collect()
    }
}

/// Map `f` over `iter`, using rayon if available.
pub fn maybe_par_unzip<I, R1, R2, F>(iter: I, f: F) -> (Vec<R1>, Vec<R2>)
where
    I: IntoParallelIterator + IntoIterator<Item = <I as IntoParallelIterator>::Item>,
    R1: Send,
    R2: Send,
    F: Fn(<I as IntoParallelIterator>::Item) -> (R1, R2) + Sync + Send,
{
    if crate::rayon_available() {
        iter.into_par_iter().map(&f).unzip()
    } else {
        iter.into_iter().map(f).unzip()
    }
}
