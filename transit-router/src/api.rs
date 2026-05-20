//! Public [`Router`] / [`Isochrone`] API. Thin idiomatic-Rust wrapper over
//! the engine in [`crate::profile`]; every external consumer (WASM wrapper,
//! future PyO3 wrapper, native CLI tools) routes through these types.

use std::ops::ControlFlow;
use std::sync::Arc;

use chrono::{Duration, NaiveDate};

use crate::data::PreparedData;
use crate::profile::{Path, ProfileQuery, ProfileRouter as _, SplitProfileRouting};
use crate::router::{patterns_for_date, snap_to_node};

/// Identifies a node in the prepared graph. Stable for the lifetime of one
/// [`PreparedData`]; not portable across builds. Distinct from [`StopId`] and
/// [`RouteId`] so the type system catches accidental mixing — every call site
/// has to convert explicitly at the boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct StopId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct RouteId(pub u32);

macro_rules! id_impls {
    ($t:ident) => {
        impl $t {
            pub const fn new(idx: u32) -> Self {
                Self(idx)
            }
            pub const fn get(self) -> u32 {
                self.0
            }
            pub const fn as_usize(self) -> usize {
                self.0 as usize
            }
        }
        impl From<u32> for $t {
            fn from(v: u32) -> Self {
                Self(v)
            }
        }
        impl From<$t> for u32 {
            fn from(v: $t) -> u32 {
                v.0
            }
        }
    };
}
id_impls!(NodeId);
id_impls!(StopId);
id_impls!(RouteId);

/// Time elapsed since 00:00 of the query's [`IsochroneParams::date`].
///
/// Used uniformly for window bounds, entry departures, and entry arrivals.
/// Values ≤ 24h are clock-time-of-day on the query date; values past 24h
/// represent the following day(s) — arrivals can roll past midnight even
/// though the engine constrains window bounds to `[0, 24h)`.
///
/// Stored as `u32` seconds (`repr(transparent)`); conversions to
/// [`chrono::Duration`] are explicit via [`SinceMidnight::as_duration`] /
/// [`SinceMidnight::from_duration`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct SinceMidnight(u32);

impl SinceMidnight {
    pub const ZERO: Self = Self(0);

    pub const fn from_seconds(s: u32) -> Self {
        Self(s)
    }
    pub const fn from_hms(h: u32, m: u32, s: u32) -> Self {
        Self(h * 3600 + m * 60 + s)
    }
    pub fn from_duration(d: Duration) -> Self {
        Self(d.num_seconds().max(0) as u32)
    }
    pub const fn as_seconds(self) -> u32 {
        self.0
    }
    pub fn as_duration(self) -> Duration {
        Duration::seconds(self.0 as i64)
    }
}

impl std::ops::Add<Duration> for SinceMidnight {
    type Output = Self;
    fn add(self, rhs: Duration) -> Self {
        let secs = rhs.num_seconds();
        if secs >= 0 {
            Self(self.0.saturating_add(secs as u32))
        } else {
            Self(self.0.saturating_sub((-secs) as u32))
        }
    }
}

impl std::ops::Sub for SinceMidnight {
    type Output = Duration;
    fn sub(self, rhs: Self) -> Duration {
        Duration::seconds(self.0 as i64 - rhs.0 as i64)
    }
}

/// Half-open departure window `[start, end)` on [`IsochroneParams::date`].
/// Engine asserts `start <= end` and both within `[0, 24h)`.
#[derive(Debug, Copy, Clone)]
pub struct TimeWindow {
    pub start: SinceMidnight,
    pub end: SinceMidnight,
}

/// Parameters for a single isochrone query.
#[derive(Debug, Clone)]
pub struct IsochroneParams {
    pub source: NodeId,
    pub date: NaiveDate,
    pub window: TimeWindow,
    /// Per-trip travel-time budget. Nodes unreachable within this from a
    /// given departure are not enumerated for that departure.
    pub max_time: Duration,
    /// Minimum connection time when switching transit vehicles.
    pub transfer_slack: Duration,
    /// Hint capping the per-query worker count below the rayon thread pool's
    /// global thread count. `None` (default) = use all rayon workers.
    /// `Some(1)` forces a single-pass run (one chunk over the full window).
    ///
    /// **Hint, not a guarantee** — the engine may still create more chunks
    /// for very long windows where one chunk can't span the budget.
    pub max_parallelism: Option<usize>,
}

impl Default for IsochroneParams {
    fn default() -> Self {
        Self {
            source: NodeId(0),
            date: NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
            window: TimeWindow {
                start: SinceMidnight::ZERO,
                end: SinceMidnight::ZERO,
            },
            max_time: Duration::zero(),
            transfer_slack: Duration::zero(),
            max_parallelism: None,
        }
    }
}

/// One Pareto-optimal entry on the per-destination frontier.
///
/// The slice [`Isochrone::entries`] returns is the load-bearing primitive
/// for everything that isn't a bulk map render — "earliest arrival for
/// departure at T", "latest departure that arrives by T", trip-distribution
/// stats. All of those reduce to a search/fold over this slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Entry {
    /// Departure since 00:00 of `params.date`. Within the window.
    pub departure: SinceMidnight,
    /// Arrival since 00:00 of `params.date`. May exceed 24h.
    pub arrival: SinceMidnight,
}

/// Failures from running the router.
#[derive(Debug)]
pub enum RouterError {
    /// Failed to decode the `.bin` payload.
    Data(String),
    /// `on_progress` returned [`ControlFlow::Break`].
    Cancelled,
    /// Bad parameter combination (e.g. window.end < window.start).
    InvalidParams(&'static str),
}

impl std::fmt::Display for RouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouterError::Data(msg) => write!(f, "data: {msg}"),
            RouterError::Cancelled => write!(f, "cancelled"),
            RouterError::InvalidParams(msg) => write!(f, "invalid params: {msg}"),
        }
    }
}

impl std::error::Error for RouterError {}

/// Owns a prepared graph and serves snap + isochrone queries. Cheap to clone
/// — the graph is `Arc`-shared, so two `Router`s built from the same bytes
/// can run queries concurrently without duplicating the 8MB of data.
#[derive(Clone)]
pub struct Router {
    data: Arc<PreparedData>,
}

impl Router {
    /// Decode a `.bin` payload and build a router over it.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RouterError> {
        let data = crate::data::load(bytes).map_err(|e| RouterError::Data(format!("{e}")))?;
        Ok(Self {
            data: Arc::new(data),
        })
    }

    /// Wrap an already-decoded [`PreparedData`].
    pub fn from_prepared(data: Arc<PreparedData>) -> Self {
        Self { data }
    }

    pub fn data(&self) -> &PreparedData {
        &self.data
    }

    /// Snap a `(lat, lon)` to the nearest graph node.
    pub fn snap(&self, lat: f64, lon: f64) -> Option<NodeId> {
        snap_to_node(&self.data, lat, lon).map(NodeId)
    }

    /// Number of patterns (service-pattern + day-mask) running on `date`.
    /// Drives the "no service today" warning in the UI.
    pub fn patterns_for_date(&self, date: NaiveDate) -> usize {
        patterns_for_date(&self.data, date).len()
    }

    /// Compute the isochrone. `on_progress` is called as `(done, total)` from
    /// the transit phase; returning [`ControlFlow::Break`] cancels and yields
    /// [`RouterError::Cancelled`]. Pass `|_, _| ControlFlow::Continue(())`
    /// to disable progress reporting.
    pub fn isochrone(
        &self,
        params: IsochroneParams,
        on_progress: impl FnMut(usize, usize) -> ControlFlow<()>,
    ) -> Result<Isochrone, RouterError> {
        self.isochrone_inner(params, on_progress, false)
    }

    /// Same as [`Router::isochrone`] but instructs the engine to use one chunk
    /// per worker rather than per-window splitting. The result is identical;
    /// the only difference is internal scheduling. Useful for warming up the
    /// rayon thread pool / WASM tier-up cache before the user's first real
    /// query.
    pub fn isochrone_warmup(
        &self,
        params: IsochroneParams,
        on_progress: impl FnMut(usize, usize) -> ControlFlow<()>,
    ) -> Result<Isochrone, RouterError> {
        self.isochrone_inner(params, on_progress, true)
    }

    fn isochrone_inner(
        &self,
        params: IsochroneParams,
        on_progress: impl FnMut(usize, usize) -> ControlFlow<()>,
        is_warmup: bool,
    ) -> Result<Isochrone, RouterError> {
        if params.window.end < params.window.start {
            return Err(RouterError::InvalidParams("window.end < window.start"));
        }
        let query = ProfileQuery {
            source_node: params.source.get(),
            window_start: params.window.start.as_seconds(),
            window_end: params.window.end.as_seconds(),
            date: params.date,
            transfer_slack: params.transfer_slack.num_seconds().max(0) as u32,
            max_time: params.max_time.num_seconds().max(0) as u32,
            is_warmup,
            max_parallelism: params.max_parallelism,
        };
        match SplitProfileRouting::compute(&self.data, &query, on_progress) {
            ControlFlow::Continue(inner) => Ok(Isochrone {
                data: self.data.clone(),
                params,
                inner,
            }),
            ControlFlow::Break(()) => Err(RouterError::Cancelled),
        }
    }
}

/// Result of one isochrone query. Holds the engine's per-node Pareto frontier
/// (for [`Isochrone::entries`] and [`Isochrone::paths`]) plus bulk mean and
/// reachable-fraction arrays for the map overlay.
pub struct Isochrone {
    data: Arc<PreparedData>,
    params: IsochroneParams,
    inner: SplitProfileRouting,
}

impl Isochrone {
    pub fn data(&self) -> &PreparedData {
        &self.data
    }
    pub fn params(&self) -> &IsochroneParams {
        &self.params
    }

    /// Mean travel time (seconds) per node, indexed by [`NodeId`]. Undefined
    /// when `reachable_fraction()[i] == 0`; the sentinel is [`u16::MAX`].
    /// Length = `data().num_nodes`.
    pub fn mean_travel_time(&self) -> &[u16] {
        &self.inner.isochrone().mean_travel_time
    }

    /// Fraction of the query window during which each node is reachable,
    /// quantised over [`u16::MAX`] (divide by `65535.0` for `f32` in
    /// `[0.0, 1.0]`). Length = `data().num_nodes`.
    pub fn reachable_fraction(&self) -> &[u16] {
        &self.inner.isochrone().reachable_fraction
    }

    /// Per-node travel time (seconds) for a single `departure` — one frame of
    /// an animated isochrone. Sweep `departure` across `params().window` to
    /// animate; values outside the window are clamped into it.
    ///
    /// The per-departure counterpart of [`Isochrone::mean_travel_time`]. Nodes
    /// unreachable within `params().max_time` get the sentinel [`u16::MAX`].
    /// Length = `data().num_nodes`, indexed by [`NodeId`].
    pub fn travel_times_at(&self, departure: SinceMidnight) -> Vec<u16> {
        self.inner.travel_times_at(departure.as_seconds())
    }

    /// Pareto frontier for `dest`, ascending by [`Entry::departure`]. Empty
    /// when `dest` is unreachable by transit within the budget (walking-only
    /// reachability is *not* yielded here — check
    /// [`Isochrone::reachable_fraction`] for that).
    ///
    /// Allocates a fresh `Vec` per call; the engine walks an internal chain
    /// arena to produce it. For repeated queries against the same `dest`,
    /// cache the result on the caller side.
    pub fn entries(&self, dest: NodeId) -> Vec<Entry> {
        self.inner
            .entries(dest.get())
            .map(|(d, a)| Entry {
                departure: SinceMidnight::from_seconds(d),
                arrival: SinceMidnight::from_seconds(a),
            })
            .collect()
    }

    /// All Pareto-optimal paths to `dest`, sorted ascending by departure.
    /// Empty when `dest` is unreachable by transit.
    pub fn paths(&self, dest: NodeId) -> Vec<Path> {
        self.inner.optimal_paths(&self.data, dest.get())
    }

    /// Number of worker threads actually used for the parallel split phase.
    /// Driven by the rayon thread pool at query time. Surfaced for the
    /// frontend's debug overlay.
    pub fn num_threads_used(&self) -> u32 {
        self.inner.isochrone().num_threads
    }

    /// Opaque diagnostic string with engine-internal phase timings
    /// (`phase1=… phase2=… phase3=… …`). Useful for benchmarks; not stable
    /// across releases — don't parse it.
    pub fn stats(&self) -> String {
        self.inner.stats()
    }
}

// Public types must be Send + Sync.
#[allow(dead_code)]
const fn assert_send_sync<T: Send + Sync>() {}
const _: () = {
    assert_send_sync::<Router>();
    assert_send_sync::<Isochrone>();
    assert_send_sync::<Path>();
    assert_send_sync::<RouterError>();
    assert_send_sync::<IsochroneParams>();
    assert_send_sync::<Entry>();
};
