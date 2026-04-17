//! Display-layer helpers for rendered [`crate::profile::Path`]s.
//!
//! Lives outside `profile.rs` because these concerns (picking a dominant
//! colour, tweaking it for legibility against the map background) are about
//! how a path is *drawn*, not how it was *computed*.

use crate::data::PreparedData;
use crate::profile::{Path, PathSegment, SegmentKind};

/// Attach `dominant_route_color_hex` to every path in `paths`, using the
/// longest transit segment's route color with a brightness adjustment.
pub fn attach_dominant_colors(data: &PreparedData, paths: &mut [Path]) {
    for p in paths.iter_mut() {
        p.dominant_route_color_hex = dominant_route_color(data, &p.segments);
    }
}

fn dominant_route_color(data: &PreparedData, segments: &[PathSegment]) -> Option<String> {
    let dominant = segments
        .iter()
        .filter(|s| s.kind == SegmentKind::Transit)
        .max_by_key(|s| s.end_time.saturating_sub(s.start_time))?;
    let route_idx = dominant.route_index? as usize;
    let color = data.route_colors.get(route_idx)?.as_ref()?;
    adjust_color_for_visibility(&color.to_hex())
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
