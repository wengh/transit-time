import type { HoverPath } from './router';

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
