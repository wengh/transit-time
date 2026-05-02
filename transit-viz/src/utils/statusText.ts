import type { AppState } from '../state/reducer';

interface Options {
  // When true, the "set origin" / "set destination" prompts use mobile phrasing
  // ("Tap map to ..."). Otherwise the desktop double-click hint is used.
  mobile?: boolean;
}

// Single source of truth for the status / hint string shown to the user.
// Used by the desktop Controls panel and the mobile top bar so both stay in
// sync when phases change (computing, done, error, copy feedback).
export function deriveStatusText(state: AppState, opts: Options = {}): string {
  const {
    showCopiedMessage,
    computeStatus,
    computeProgress,
    computeTimeMs,
    computeNumThreads,
    sourceNode,
    nodeCount,
    stopCount,
    interactionMode,
  } = state;

  if (showCopiedMessage) return 'Copied!';
  if (computeStatus === 'computing') {
    return computeProgress
      ? `Computing... ${Math.round((computeProgress.done / computeProgress.total) * 100)}%`
      : 'Computing...';
  }
  if (computeStatus === 'done') {
    return `Done. Spent ${Math.round(computeTimeMs)} ms using ${computeNumThreads} thread${
      computeNumThreads === 1 ? '' : 's'
    }.`;
  }
  if (computeStatus === 'error') return 'Error';

  if (sourceNode === null) {
    if (opts.mobile) return 'Tap map to set origin';
    return `${nodeCount.toLocaleString()} nodes, ${stopCount.toLocaleString()} stops. Double-click map to set origin.`;
  }

  if (opts.mobile) {
    return interactionMode === 'origin' ? 'Tap map to set origin' : 'Tap map to set destination';
  }
  return `${nodeCount.toLocaleString()} nodes, ${stopCount.toLocaleString()} stops.`;
}
