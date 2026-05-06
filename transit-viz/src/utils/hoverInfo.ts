import type { HoverPath } from './router';
import { getProfileHoverData } from './router';
import type { HoverData } from '../state/reducer';

export function getSortedTravelTimes(allPaths: HoverPath[]): number[] {
  return allPaths
    .map((p) => p.totalTime)
    .filter((t): t is number => t !== null && isFinite(t))
    .sort((a, b) => a - b);
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

export async function buildHoverData(
  node: number,
  travelTimesArray: Float32Array | null,
  sampleCounts: Uint32Array | null,
  totalSamples: number
): Promise<HoverData> {
  const { paths: allPaths, representativeIndex } = await getProfileHoverData(node);
  const travelTimes = getSortedTravelTimes(allPaths);
  const tt = travelTimesArray ? travelTimesArray[node] : NaN;
  const avgTravelTime = isFinite(tt) ? tt : null;
  const reachableFraction =
    sampleCounts && totalSamples > 0 ? sampleCounts[node] / totalSamples : null;

  return {
    allPaths,
    representativeIndex,
    travelTimes,
    avgTravelTime,
    reachableFraction,
  };
}
