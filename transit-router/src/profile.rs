//! Profile routing: Pareto frontier of (arrival, home_departure) per node
//! over a departure-time window. One pass replaces N-sample Dijkstra.
//!
//! # Public interface
//!
//! [`ProfileRouter`] is the contract. The concrete type [`ProfileRouting`]
//! implements it. Callers hold `impl ProfileRouter` or the concrete type;
//! internal representation is free to change.

use crate::data::PreparedData;
use serde::Serialize;

// ============================================================================
// Input / output types
// ============================================================================

/// Input to [`ProfileRouter::compute`].
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

// ============================================================================
// Trait
// ============================================================================

/// Contract for profile routing. Implement this to replace the routing engine
/// without touching callers in `lib.rs` or tests.
pub trait ProfileRouter: Sized {
    /// Run profile routing from `query.source_node` over the departure window.
    fn compute(data: &PreparedData, query: &ProfileQuery) -> Self;

    /// Per-node isochrone for map rendering.
    fn isochrone(&self) -> &Isochrone;

    /// All Pareto-optimal paths to `destination`, sorted ascending by
    /// `home_departure`. Stop and route names resolved from `data`.
    fn optimal_paths(&self, data: &PreparedData, destination: u32) -> Vec<Path>;
}

// ============================================================================
// Stub implementation (replace with real algorithm)
// ============================================================================

/// Opaque routing state. Internal representation is not part of the public
/// interface — swap freely as long as [`ProfileRouter`] is satisfied.
pub struct ProfileRouting {
    isochrone: Isochrone,
}

impl ProfileRouter for ProfileRouting {
    fn compute(data: &PreparedData, query: &ProfileQuery) -> Self {
        ProfileRouting {
            isochrone: Isochrone {
                min_travel_time: vec![u32::MAX; data.num_nodes],
                reachable_fraction: vec![0.0; data.num_nodes],
                window_start: query.window_start,
                window_end: query.window_end,
            },
        }
    }

    fn isochrone(&self) -> &Isochrone {
        &self.isochrone
    }

    fn optimal_paths(&self, _data: &PreparedData, _destination: u32) -> Vec<Path> {
        Vec::new()
    }
}
