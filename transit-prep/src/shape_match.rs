//! Stop-to-GTFS-shape projection via DP subsequence matching.
//!
//! Given a polyline (a route shape from `shapes.txt`) and an ordered list of
//! stops on that shape, find the minimum-cost monotone assignment of stops to
//! shape segments. The result is a per-stop projection (segment index +
//! parameter `t` along that segment + squared distance) used downstream to
//! split the shape into per-leg slices.

use crate::graph;

#[derive(Clone, Copy)]
pub struct ShapeMatch {
    pub seg_idx: usize,
    pub t: f64,
    pub proj: (f64, f64),
    pub dist_sq: f64,
}

/// DP subsequence matching: find the minimum-cost monotone assignment of stops
/// to shape *segments*, recording the projection of each stop onto its segment.
pub fn match_stops_to_shape(
    stop_coords: &[(f64, f64)],
    shape: &[(f64, f64)],
    cos_lat: f64,
) -> Option<Vec<ShapeMatch>> {
    let (cost, assignment) = match_stops_to_shape_impl(stop_coords, shape, cos_lat)?;

    // Try reverse direction if the cost is abnormally high.
    // This happens in Mexico City metro line 9 for example.
    let avg_cost = cost / stop_coords.len() as f64;
    const THRESHOLD: f64 = 0.0005; // ~50m
    if avg_cost > THRESHOLD * THRESHOLD
        && let Some((rev_cost, rev_assignment)) = match_stops_to_shape_impl(
            &stop_coords.iter().rev().cloned().collect::<Vec<_>>(),
            shape,
            cos_lat,
        )
    {
        // Only accept if the reverse is much better
        if rev_cost * 5.0 < cost {
            return Some(rev_assignment.into_iter().rev().collect());
        }
    }
    Some(assignment)
}

/// Forward DP over `stops × segments`, keeping only the running cost and the
/// segment parameter `t` of the current and previous stop rows plus a flat
/// `u32` backtrack table. The chosen segments are re-projected at the end;
/// storing a full projection per cell (40 bytes each) for a 3000-point shape
/// and 40 stops used to cost ~5 MB per call.
fn match_stops_to_shape_impl(
    stop_coords: &[(f64, f64)],
    shape: &[(f64, f64)],
    cos_lat: f64,
) -> Option<(f64, Vec<ShapeMatch>)> {
    let n = stop_coords.len();
    let m = shape.len();
    if n == 0 || m < 2 {
        return None;
    }
    let segs = m - 1;
    let project = |i: usize, j: usize| -> (f64, (f64, f64), f64) {
        graph::project_on_segment(stop_coords[i], shape[j], shape[j + 1], cos_lat)
    };

    // dp[j]: best cost with the previous stop on segment j; prev_t[j]: its
    // parameter along that segment.
    let mut dp = vec![f64::MAX; segs];
    let mut prev_t = vec![0.0f64; segs];
    // backtrack[i * segs + j]: segment of stop i-1 when stop i is on segment j.
    let mut backtrack = vec![0u32; n * segs];

    for (j, (cost, t)) in dp.iter_mut().zip(prev_t.iter_mut()).enumerate() {
        let (t0, _, d) = project(0, j);
        *cost = d;
        *t = t0;
    }

    let mut new_dp = vec![f64::MAX; segs];
    let mut new_t = vec![0.0f64; segs];
    for i in 1..n {
        // min over dp[0..j]: the best strictly earlier segment.
        let mut min_prev = f64::MAX;
        let mut argmin_prev = 0usize;
        for j in 0..segs {
            let (t, _, d) = project(i, j);
            let mut best = min_prev;
            let mut arg = argmin_prev;
            // The previous stop may sit on this same segment as long as the
            // order along it is preserved. Forcing strictly increasing
            // segments pushed the second of two stops on one long straight
            // segment onto the next vertex, detouring the leg polyline.
            if dp[j] < best && t >= prev_t[j] {
                best = dp[j];
                arg = j;
            }
            if best < f64::MAX {
                new_dp[j] = d + best;
                backtrack[i * segs + j] = arg as u32;
            } else {
                new_dp[j] = f64::MAX;
            }
            new_t[j] = t;
            if dp[j] < min_prev {
                min_prev = dp[j];
                argmin_prev = j;
            }
        }
        std::mem::swap(&mut dp, &mut new_dp);
        std::mem::swap(&mut prev_t, &mut new_t);
    }

    let mut best_j = 0;
    let mut best_cost = f64::MAX;
    for (j, &cost) in dp.iter().enumerate() {
        if cost < best_cost {
            best_cost = cost;
            best_j = j;
        }
    }
    if best_cost == f64::MAX {
        return None;
    }

    let mut picks = vec![0usize; n];
    picks[n - 1] = best_j;
    for i in (1..n).rev() {
        picks[i - 1] = backtrack[i * segs + picks[i]] as usize;
    }
    let result = (0..n)
        .map(|i| {
            let j = picks[i];
            let (t, proj, dist_sq) = project(i, j);
            ShapeMatch {
                seg_idx: j,
                t,
                proj,
                dist_sq,
            }
        })
        .collect();
    Some((best_cost, result))
}
