import type { HoverPath, PathSegment } from './router';

export interface TravelTimeSummary {
  avg: number;
  min?: number;
  max?: number;
  isSampled: boolean;
  count?: number;
  total?: number;
}

export function getTravelTimeSummary(travelTimes: Float64Array | number[], allPaths: HoverPath[]): TravelTimeSummary | null {
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

export function formatSegments(segments: PathSegment[]): string[] {
  const lines: string[] = [];
  for (const seg of segments) {
    if (seg.edgeType === 0) {
      lines.push(`Walk ${Math.round(seg.duration / 60)} min`);
    } else {
      const fromTo =
        seg.startStopName && seg.endStopName ? ` · ${seg.startStopName} → ${seg.endStopName}` : '';
      lines.push(`${seg.routeName || 'Transit'}${fromTo} ${Math.round(seg.duration / 60)} min`);
      if (seg.waitTime > 0) {
        lines.push(`  Wait: ${(seg.waitTime / 60).toFixed(1)} min`);
      }
    }
  }
  return lines;
}
