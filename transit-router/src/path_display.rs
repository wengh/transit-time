//! Display helpers for [`crate::profile::Path`]s.
//!
//! Everything here is a *pure function* of `Path` (+ `PreparedData` for colour
//! lookups). `Path` itself is the canonical data; display strings and the
//! dominant route colour are computed views, assembled at the serialisation
//! boundary via [`PathView`]. This keeps `Path` free of derived fields so the
//! same struct can be locked down by tests without drifting into a view type.

use crate::data::PreparedData;
use crate::profile::{Path, PathSegment, SegmentKind};
use pco;
use serde::Serialize;

/// Human-readable strings derived from a [`Path`].
///
/// Shapes match the per-segment / total-time text previously built by the
/// frontend in `hoverInfo.ts::formatSegments`. Keep the Rust formatter the
/// single source of truth for the rendered text so tests can assert on it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathDisplay {
    /// One entry per segment, each a short list of display lines. A transit
    /// segment with a non-zero wait emits two lines (primary + "  Wait: ...").
    pub segment_lines: Vec<Vec<String>>,
    /// Summary line for the whole journey (e.g. `"Total: 17 min"`).
    pub total_time_line: String,
}

/// JSON-boundary wrapper: flattens `Path`'s fields at the top level and adds
/// derived data (display strings, colour). Never constructed in pure-Rust
/// hot paths — callers work with `&Path` directly and call the helpers below.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathView<'a> {
    #[serde(flatten)]
    pub path: &'a Path,
    pub display: PathDisplay,
    pub dominant_route_color_hex: Option<String>,
}

impl<'a> PathView<'a> {
    pub fn new(data: &PreparedData, path: &'a Path) -> Self {
        Self {
            display: display(path),
            dominant_route_color_hex: dominant_route_color(data, path),
            path,
        }
    }
}

/// Produce the per-segment text lines + total-time summary for a path.
pub fn display(path: &Path) -> PathDisplay {
    let segment_lines = path.segments.iter().map(format_segment).collect();
    let total_min = (path.total_time + 30) / 60;
    PathDisplay {
        segment_lines,
        total_time_line: format!("Total: {total_min} min"),
    }
}

/// Pick the longest transit segment's route colour, brightness-adjusted for
/// legibility on a light map. Returns `None` for walk-only paths or unknown
/// colours. Same logic the frontend used to run per-hover.
pub fn dominant_route_color(data: &PreparedData, path: &Path) -> Option<String> {
    let dominant = path
        .segments
        .iter()
        .filter(|s| s.kind == SegmentKind::Transit)
        .max_by_key(|s| s.end_time.saturating_sub(s.start_time))?;
    let route_idx = dominant.route_index? as usize;
    let color = data.route_colors.get(route_idx)?.as_ref()?;
    adjust_color_for_visibility(&color.to_hex())
}

fn format_segment(seg: &PathSegment) -> Vec<String> {
    let dur_min = (seg.end_time.saturating_sub(seg.start_time) + 30) / 60;
    match seg.kind {
        SegmentKind::Walk => vec![format!("Walk {dur_min} min")],
        SegmentKind::Transit => {
            let route = seg.route_name.as_deref().unwrap_or("Transit");
            let from_to = if !seg.start_stop_name.is_empty() && !seg.end_stop_name.is_empty() {
                format!(" · {} → {}", seg.start_stop_name, seg.end_stop_name)
            } else {
                String::new()
            };
            let mut out = vec![format!("{route}{from_to} {dur_min} min")];
            if seg.wait_time > 0 {
                out.push(format!("  Wait: {:.1} min", seg.wait_time as f32 / 60.0));
            }
            out
        }
    }
}

/// Build a flat `[lat, lon, …]` polyline for a path segment.
///
/// `route_index`: `None` for walk (straight line through nodes); `Some(r)` for
/// transit (chains per-leg GTFS shapes with straight-line fallback).
pub fn segment_shape(data: &PreparedData, route_index: Option<u16>, nodes: &[u32]) -> Vec<f32> {
    if nodes.len() < 2 {
        return Vec::new();
    }
    match route_index {
        None => {
            let mut out = Vec::with_capacity(nodes.len() * 2);
            for &n in nodes {
                out.push(data.nodes[n as usize].lat as f32);
                out.push(data.nodes[n as usize].lon as f32);
            }
            out
        }
        Some(route_idx) => {
            let mut out: Vec<f32> = Vec::new();
            for pair in nodes.windows(2) {
                let leg =
                    leg_shape_between(data, route_idx as u32, pair[0], pair[1]).unwrap_or_default();
                let skip = if out.is_empty() { 0 } else { 2 };
                if leg.len() >= 4 {
                    out.extend(leg[skip..].iter().copied());
                } else {
                    if out.is_empty() {
                        out.push(data.nodes[pair[0] as usize].lat as f32);
                        out.push(data.nodes[pair[0] as usize].lon as f32);
                    }
                    out.push(data.nodes[pair[1] as usize].lat as f32);
                    out.push(data.nodes[pair[1] as usize].lon as f32);
                }
            }
            out
        }
    }
}

fn leg_shape_between(
    data: &PreparedData,
    route_idx: u32,
    from_node: u32,
    to_node: u32,
) -> Option<Vec<f32>> {
    let from_stop = *data.node_to_stop.get(&from_node)?;
    let to_stop = *data.node_to_stop.get(&to_node)?;
    let key = (route_idx, from_stop, to_stop);
    let idx = data.leg_shape_keys.binary_search(&key).ok()?;
    let start = data.leg_shapes.offsets[idx] as usize;
    let end = data.leg_shapes.offsets[idx + 1] as usize;
    let compressed = &data.leg_shapes.data[start..end];
    if compressed.is_empty() {
        return None;
    }
    let coords_u32: Vec<u32> = pco::standalone::simple_decompress(compressed).ok()?;
    let mut out = Vec::with_capacity(coords_u32.len());
    for chunk in coords_u32.chunks(2) {
        if chunk.len() == 2 {
            out.push(f32::from_bits(chunk[0]));
            out.push(f32::from_bits(chunk[1]));
        }
    }
    Some(out)
}

fn adjust_color_for_visibility(hex: &str) -> Option<String> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f32;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f32;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f32;
    let lum = (r * 299.0 + g * 587.0 + b * 114.0) / 1000.0;
    let (r, g, b) = if lum > 0.0 && lum < 100.0 {
        let s = 100.0 / lum;
        ((r * s).min(255.0), (g * s).min(255.0), (b * s).min(255.0))
    } else if lum > 220.0 {
        let s = 220.0 / lum;
        (r * s, g * s, b * s)
    } else {
        (r, g, b)
    };
    Some(format!(
        "#{:02x}{:02x}{:02x}",
        r.round() as u8,
        g.round() as u8,
        b.round() as u8
    ))
}
