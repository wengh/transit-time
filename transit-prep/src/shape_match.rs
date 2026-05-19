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
    if avg_cost > THRESHOLD * THRESHOLD {
        if let Some((rev_cost, rev_assignment)) = match_stops_to_shape_impl(
            &stop_coords.iter().rev().cloned().collect::<Vec<_>>(),
            shape,
            cos_lat,
        ) {
            // Only accept if the reverse is much better
            if rev_cost * 5.0 < cost {
                return Some(rev_assignment.into_iter().rev().collect());
            }
        }
    }
    Some(assignment)
}

fn match_stops_to_shape_impl(
    stop_coords: &[(f64, f64)],
    shape: &[(f64, f64)],
    cos_lat: f64,
) -> Option<(f64, Vec<ShapeMatch>)> {
    let n = stop_coords.len();
    let m = shape.len();
    if n == 0 || m < 2 || n > m {
        return None;
    }
    let segs = m - 1;

    // Precompute the projection of every stop onto every segment once.
    let mut matches: Vec<ShapeMatch> = Vec::with_capacity(n * segs);
    for i in 0..n {
        for j in 0..segs {
            let (t, proj, d) =
                graph::project_on_segment(stop_coords[i], shape[j], shape[j + 1], cos_lat);
            matches.push(ShapeMatch {
                seg_idx: j,
                t,
                proj,
                dist_sq: d,
            });
        }
    }
    let at = |i: usize, j: usize| matches[i * segs + j];

    let mut dp = vec![f64::MAX; segs];
    let mut backtrack = vec![vec![0usize; segs]; n];

    for j in 0..segs {
        dp[j] = at(0, j).dist_sq;
    }

    for i in 1..n {
        let mut new_dp = vec![f64::MAX; segs];
        let mut min_prev = f64::MAX;
        let mut argmin_prev = 0;

        for j in 0..segs {
            if min_prev < f64::MAX {
                new_dp[j] = at(i, j).dist_sq + min_prev;
                backtrack[i][j] = argmin_prev;
            }
            if dp[j] < min_prev {
                min_prev = dp[j];
                argmin_prev = j;
            }
        }
        dp = new_dp;
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
        picks[i - 1] = backtrack[i][picks[i]];
    }
    let result = (0..n).map(|i| at(i, picks[i])).collect();
    Some((best_cost, result))
}
