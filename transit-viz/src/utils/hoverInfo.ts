import type { HoverPath } from './router';

export function getSortedTravelTimes(allPaths: HoverPath[]): number[] {
  return allPaths
    .map((p) => p.totalTime)
    .filter((t): t is number => t !== null && isFinite(t))
    .sort((a, b) => a - b);
}

export interface TravelTimeSummary {
  avg: number;
  min?: number;
  max?: number;
  isSampled: boolean;
  count?: number;
  total?: number;
}

export function getTravelTimeSummary(travelTimes: Float32Array | number[], allPaths: HoverPath[]): TravelTimeSummary | null {
  if (!travelTimes || travelTimes.length === 0) return null;
  const sum = typeof travelTimes[0] === 'number'
    ? (travelTimes as number[]).reduce((a, b) => a + b, 0)
    : Array.from(travelTimes).reduce((a, b) => a + b, 0);
  const avg = Math.round(sum / travelTimes.length / 60);
  if (allPaths.length > 1) {
    const min = Math.round(travelTimes[0] / 60);
    const max = Math.round(travelTimes[travelTimes.length - 1] / 60);
    return { min, avg, max, isSampled: true, count: travelTimes.length, total: allPaths.length };
  }
  return { avg, isSampled: false };
}

export function getMedianPath(allPaths: HoverPath[]): HoverPath | null {
  const reachable = allPaths.filter((p) => p.totalTime !== null);
  return reachable[Math.floor(reachable.length / 2)] || null;
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
