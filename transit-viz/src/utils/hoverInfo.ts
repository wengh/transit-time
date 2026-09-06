import type { HoverPath } from './router';
import { getProfileHoverData } from './router';
import type { AppState, Destination, HoverData } from '../state/reducer';
import { useAnimMode, useAnimRenderedDeparture } from '../state/animationStore';
import { formatTime } from './format';
import { formatDistance, haversineKm } from './geo';

export function getMedianPath(allPaths: HoverPath[]): HoverPath | null {
  return allPaths[Math.floor(allPaths.length / 2)] || null;
}

// Per-segment text lines now come from the Rust-side `PathDisplay`
// (see `path.display.segmentLines`) — one source of truth for what the user
// reads. Formerly `formatSegments` duplicated this in TypeScript.
export function flattenDisplayLines(path: HoverPath): string[] {
  if (!path.display) return [];
  const out: string[] = [];
  for (const lines of path.display.segmentLines) out.push(...lines);
  return out;
}

export async function buildHoverData(
  node: number,
  travelTimesArray: Float32Array | null,
  sampleCounts: Uint32Array | null,
  totalSamples: number
): Promise<HoverData> {
  const { paths: allPaths, representativeIndex } = await getProfileHoverData(node);
  const tt = travelTimesArray ? travelTimesArray[node] : NaN;
  const avgTravelTime = isFinite(tt) ? tt : null;
  const reachableFraction =
    sampleCounts && totalSamples > 0 ? sampleCounts[node] / totalSamples : null;

  return {
    allPaths,
    representativeIndex,
    avgTravelTime,
    reachableFraction,
  };
}

// ─── chart data ───────────────────────────────────────────────────────────────

export interface ChartTip {
  tipX: number; // absolute departure time when you just catch this trip (seconds)
  tipY: number; // travel time if you just catch it (seconds)
  pathIdx: number; // index into allPaths for the representative path for this trip
  color: string;
}

export interface ChartInfo {
  tips: ChartTip[];
  walkTime: number | null;
  walkPathIdx: number | null;
  windowStart: number;
  windowEnd: number;
  yMax: number;
}

/** Plot gutters in CSS px. Shared by the canvas painter and the x→time mapping. */
export const CHART_PAD = { top: 8, right: 8, bottom: 22, left: 34 };

function computeChartInfoUncached(
  allPaths: HoverPath[],
  windowStart: number,
  windowEnd: number,
  maxTimeSec: number
): ChartInfo {
  let walkTime: number | null = null;
  let walkPathIdx: number | null = null;
  const rawTips: Array<ChartTip> = [];

  for (let i = 0; i < allPaths.length; i++) {
    const p = allPaths[i];

    const isWalkOnly = p.segments.length > 0 && p.segments.every((s) => s.edgeType === 0);
    if (isWalkOnly) {
      if (walkTime === null || p.totalTime < walkTime) {
        walkTime = p.totalTime;
        walkPathIdx = i;
      }
      continue;
    }

    const firstTransit = p.segments.find((s) => s.edgeType === 1);
    if (!firstTransit) continue;

    const w = firstTransit.waitTime;
    const tipX = p.departureTime + w;
    const tipY = p.totalTime - w;
    if (tipY < 0) continue;

    // No arrival-time dedup: Pareto dominance in the Rust profile router
    // already guarantees unique (arrival, home_departure) pairs. If two
    // entries collide here, that's a bug in the Rust filter — surface it
    // rather than masking it in the chart.
    rawTips.push({ tipX, tipY, pathIdx: i, color: p.routeColor });
  }

  const tips: ChartTip[] = rawTips.sort((a, b) => a.tipX - b.tipX);

  const yMax = maxTimeSec;
  return { tips, walkTime, walkPathIdx, windowStart, windowEnd, yMax };
}

// One entry per path list. The chart, the detail panel and the map's route
// resolver all derive from the same `HoverData` on every animation frame;
// without this the tips were rebuilt and re-sorted three times per frame.
// Keyed weakly so a replaced hover's entry is collected with its paths.
const chartInfoCache = new WeakMap<HoverPath[], { key: string; info: ChartInfo }>();

export function computeChartInfo(
  allPaths: HoverPath[],
  windowStart: number,
  windowEnd: number,
  maxTimeSec: number
): ChartInfo {
  const key = `${windowStart}|${windowEnd}|${maxTimeSec}`;
  const hit = chartInfoCache.get(allPaths);
  if (hit && hit.key === key) return hit.info;
  const info = computeChartInfoUncached(allPaths, windowStart, windowEnd, maxTimeSec);
  chartInfoCache.set(allPaths, { key, info });
  return info;
}

// ─── time ↔ x-position ↔ path index ──────────────────────────────────────────

/** Map a canvas x-pixel to the departure time it represents on the chart. */
export function timeAtCanvasX(canvasX: number, canvasWidth: number, info: ChartInfo): number {
  const plotW = canvasWidth - CHART_PAD.left - CHART_PAD.right;
  const frac = (canvasX - CHART_PAD.left) / plotW;
  return info.windowStart + frac * (info.windowEnd - info.windowStart);
}

/** Which path in `allPaths` is optimal when departing at time `t`. */
export function pathIdxAtTime(t: number, info: ChartInfo): number | null {
  const { tips, walkPathIdx, windowStart, yMax, walkTime } = info;
  const clipY = walkTime !== null ? Math.min(walkTime, yMax) : yMax;

  for (let i = 0; i < tips.length; i++) {
    const leftBound = i === 0 ? windowStart : tips[i - 1].tipX;
    const { tipX, tipY } = tips[i];
    if (t >= leftBound && t <= tipX) {
      // Entire trip is slower than walk/maxTime, or departure is in the grey zone
      if (tipY > clipY || t < tipX - (clipY - tipY)) return walkPathIdx;
      return tips[i].pathIdx;
    }
  }
  return walkPathIdx;
}

// ─── destination summary ─────────────────────────────────────────────────────

// Resolve the path to show in the detail panel. With no departure time chosen
// (average view) this is the representative/median path; with one chosen it is
// the path optimal for that departure — found by replaying the chart's
// time→path-index mapping. Unlike the old per-sample view, the leading wait is
// *kept*: when you pick a clock time, the wait until the vehicle arrives is
// real time you'd spend, so it belongs in the trip.
//
// Returns the path object from `allPaths` itself, not a copy, so callers can
// use identity to tell "same path as last frame" from "different path".
export function deriveDisplayPath(
  hoverData: HoverData,
  departureTime: number | null,
  windowStart: number,
  windowEnd: number,
  maxTimeSec: number
): HoverPath | null {
  const { allPaths, representativeIndex } = hoverData;
  if (departureTime === null) {
    return representativeIndex !== null && allPaths[representativeIndex]
      ? allPaths[representativeIndex]
      : getMedianPath(allPaths);
  }
  const info = computeChartInfo(allPaths, windowStart, windowEnd, maxTimeSec);
  const idx = pathIdxAtTime(departureTime, info);
  return idx !== null ? (allPaths[idx] ?? null) : null;
}

/**
 * One-line summary above the chart, e.g. `avg 24 min / 100% reachable / 10 km`.
 * `distanceKm` is the straight-line origin→destination distance (null if unknown).
 */
export function deriveTitleText(
  hoverData: HoverData,
  departureTime: number | null,
  displayPath: HoverPath | null,
  distanceKm: number | null = null
): string {
  const parts: string[] = [];
  if (departureTime !== null) {
    if (displayPath) {
      const depStr = formatTime(displayPath.departureTime);
      parts.push(`${Math.round(displayPath.totalTime / 60)} min (depart ${depStr})`);
    } else {
      parts.push('Unreachable');
    }
  } else {
    const avgSec = hoverData.avgTravelTime;
    const frac = hoverData.reachableFraction ?? 0;
    if (avgSec === null || frac <= 0) {
      parts.push('Unreachable');
    } else {
      parts.push(`avg ${Math.round(avgSec / 60)} min`, `${Math.round(frac * 100)}% reachable`);
    }
  }
  if (distanceKm !== null) parts.push(formatDistance(distanceKm));
  return parts.join(' / ');
}

export interface DestinationSummary {
  /** Departure the map is showing, or null in the window-average view. */
  departureTime: number | null;
  displayPath: HoverPath | null;
  distanceKm: number | null;
  titleText: string;
}

/**
 * Everything the detail panels show for a destination, derived once from the
 * animation playhead and app state. Shared by the desktop HoverInfo panel and
 * the mobile bottom sheet. Null when the destination has no hover data yet.
 */
export function useDestinationSummary(
  state: AppState,
  dest: Destination | null
): DestinationSummary | null {
  const animMode = useAnimMode();
  const animDep = useAnimRenderedDeparture();
  const hoverData = dest?.hoverData ?? null;
  if (!dest || !hoverData) return null;
  const departureTime = animMode === 'frame' ? animDep : null;
  const displayPath = deriveDisplayPath(
    hoverData,
    departureTime,
    state.windowStart,
    state.windowEnd,
    state.maxTimeMin * 60
  );
  const distanceKm = state.sourceLatLng ? haversineKm(state.sourceLatLng, dest.latLng) : null;
  const titleText = deriveTitleText(hoverData, departureTime, displayPath, distanceKm);
  return { departureTime, displayPath, distanceKm, titleText };
}
