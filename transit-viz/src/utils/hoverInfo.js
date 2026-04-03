// Shared utilities for formatting hover/route info

export function getTravelTimeSummary(travelTimes, allPaths) {
  if (!travelTimes || travelTimes.length === 0) return null;
  const avg = Math.round(travelTimes.reduce((a, b) => a + b, 0) / travelTimes.length / 60);
  if (allPaths.length > 1) {
    const min = Math.round(travelTimes[0] / 60);
    const max = Math.round(travelTimes[travelTimes.length - 1] / 60);
    return { min, avg, max, isSampled: true, count: travelTimes.length, total: allPaths.length };
  }
  return { avg, isSampled: false };
}

export function getMedianPath(allPaths) {
  const reachable = allPaths.filter(p => p.totalTime !== null);
  return reachable[Math.floor(reachable.length / 2)] || null;
}

export function formatSegments(segments) {
  const lines = [];
  for (const seg of segments) {
    if (seg.edgeType === 0) {
      lines.push(`Walk ${Math.round(seg.duration / 60)} min`);
    } else {
      const fromTo = (seg.startStopName && seg.endStopName)
        ? ` · ${seg.startStopName} → ${seg.endStopName}` : '';
      lines.push(`${seg.routeName || 'Transit'}${fromTo} ${Math.round(seg.duration / 60)} min`);
      if (seg.waitTime > 0) {
        const label = segments.indexOf(seg) <= 1 ? 'Initial wait' : 'Transfer wait';
        lines.push(`  ${label}: ${(seg.waitTime / 60).toFixed(1)} min`);
      }
    }
  }
  return lines;
}
