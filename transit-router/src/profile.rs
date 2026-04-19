//! Profile routing: Pareto frontier of (arrival, home_departure) per node
//! over a departure-time window. One pass replaces N-sample Dijkstra.
//!
//! # Public interface
//!
//! Two functions cross the boundary with the rest of the codebase:
//!
//! 1. [`ProfileRouting::compute`] — run routing from a source, get an opaque
//!    routing state containing the per-node [`Isochrone`] for map rendering.
//! 2. [`ProfileRouting::optimal_paths`] — given the state + a destination,
//!    get a `Vec<Path>` of all Pareto-optimal journeys with fully-resolved
//!    segment metadata (stop names, route names, times, waits, node sequences).
//!
//! Everything below those signatures is implementation detail and may be
//! rewritten freely.

use crate::data::PreparedData;
use serde::Serialize;

// ============================================================================
// Public interface: input query, isochrone, paths
// ============================================================================

/// Input to [`ProfileRouting::compute`].
#[derive(Clone, Copy, Debug)]
pub struct ProfileQuery {
    pub source_node: u32,
    /// Absolute seconds-of-day. Start of the departure-time window.
    pub window_start: u32,
    /// Absolute seconds-of-day. End of the departure-time window.
    pub window_end: u32,
    /// YYYYMMDD.
    pub date: u32,
    /// Seconds of transfer slack between transit legs.
    pub transfer_slack: u32,
    /// Isochrone budget in seconds. Nodes unreachable within `max_time` of
    /// departing home are reported as unreachable.
    pub max_time: u32,
}

/// Per-node isochrone summary for the map overlay.
#[derive(Debug, Clone)]
pub struct Isochrone {
    /// Length = `data.num_nodes`. `u32::MAX` = unreachable within `max_time`.
    /// `min_travel_time[v]` = min over all Pareto entries at `v` of
    /// `(arrival − home_departure)`. Walk-only entry contributes its walk time.
    pub min_travel_time: Vec<u32>,
    /// Length = `data.num_nodes`. In `[0.0, 1.0]`. Fraction of the query window
    /// during which `v` is reachable within `max_time`, computed as the
    /// normalised interval union over the per-node Pareto frontier.
    pub reachable_fraction: Vec<f32>,
    pub window_start: u32,
    pub window_end: u32,
}

/// One Pareto-optimal journey from source to a destination.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Path {
    /// Absolute seconds-of-day.
    pub home_departure: u32,
    /// Absolute seconds-of-day at destination.
    pub arrival_time: u32,
    /// `arrival_time − home_departure`.
    pub total_time: u32,
    pub segments: Vec<PathSegment>,
}

/// One edge of a [`Path`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathSegment {
    pub kind: SegmentKind,
    /// Absolute seconds-of-day. Transit: vehicle_dep at boarding. Walk: arrival
    /// at the first node.
    pub start_time: u32,
    /// Absolute seconds-of-day. Arrival at the last node.
    pub end_time: u32,
    /// Seconds between arriving at the boarding stop and vehicle_dep. `0` for
    /// walks.
    pub wait_time: u32,
    pub start_stop_name: String,
    pub end_stop_name: String,
    /// `None` for walks.
    pub route_index: Option<u16>,
    /// `None` for walks. Human label (e.g. "Blue Line").
    pub route_name: Option<String>,
    /// Node indices. Walk: `[start, end]` (len 2). Transit: `[boarding,
    /// intermediate…, final_alight]` (len ≥ 2). The first node is always the
    /// boarding/walk-start; the last the alight/walk-end.
    pub node_sequence: Vec<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SegmentKind {
    Walk,
    Transit,
}

/// Opaque routing state. Holds the Pareto frontier plus cached isochrone.
/// Internal representation is not part of the public interface.
pub struct ProfileRouting {
    isochrone: Isochrone,
}

impl ProfileRouting {
    /// Run profile routing from the query's source over the window.
    /// Returns the state + per-node isochrone.
    pub fn compute(_data: &PreparedData, query: &ProfileQuery) -> Self {
        let n = _data.num_nodes;
        ProfileRouting {
            isochrone: Isochrone {
                min_travel_time: vec![u32::MAX; n],
                reachable_fraction: vec![0.0; n],
                window_start: query.window_start,
                window_end: query.window_end,
            },
        }
    }

    pub fn isochrone(&self) -> &Isochrone {
        &self.isochrone
    }

    /// Enumerate all Pareto-optimal paths to `destination`, sorted ascending
    /// by `home_departure`. Route names and stop names are resolved from `data`.
    pub fn optimal_paths(&self, _data: &PreparedData, _destination: u32) -> Vec<Path> {
        Vec::new()
    }
}
