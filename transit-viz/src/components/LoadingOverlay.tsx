import React from 'react';
import { useAppState } from '../state/AppContext';

// Floating progress pill, shown for two kinds of work:
//   1. City data still loading and a placement intent is queued (clicked the
//      map or restored from URL hash) — communicates that the click is on
//      hold until the router is ready.
//   2. An isochrone query is computing — surfaces the long-running compute
//      more prominently than the small status line in the controls panel.
// The wrapper is click-through (`pointer-events-none`) so the user can keep
// panning/zooming the map underneath; only the centered pill is interactive.
export default function LoadingOverlay(): React.ReactNode {
  const { state } = useAppState();
  const {
    loadingState,
    loadingProgress,
    currentCity,
    pendingSource,
    pendingDest,
    computeStatus,
    computeProgress,
  } = state;

  const isLoading = loadingState === 'loading' || loadingState === 'initializing';
  const showComputing = computeStatus === 'computing';
  // Showing on `pendingSource` (rather than just `isLoading && hasPending`)
  // bridges the brief gap between CITY_LOADED and COMPUTING — pendingSource
  // stays set across snap → SET_SOURCE and is cleared by the COMPUTING
  // reducer, so the overlay flips directly from "pending" to "computing"
  // without a one-render flash of nothing.
  const hasPendingDuringLoad = isLoading && pendingDest !== null;
  if (!pendingSource && !showComputing && !hasPendingDuringLoad) return null;

  let text: string;
  if (showComputing) {
    const pct = computeProgress
      ? Math.round((computeProgress.done / computeProgress.total) * 100)
      : null;
    text = pct !== null ? `Computing… ${pct}%` : 'Computing…';
  } else if (loadingState === 'initializing') {
    text = `Initializing router for ${currentCity?.name ?? ''}…`;
  } else if (loadingState === 'loading') {
    text = `Loading ${currentCity?.name ?? ''}… ${loadingProgress}%`;
  } else {
    // loadingState === 'ready' but pendingSource still set: we're between
    // CITY_LOADED and COMPUTING — about to compute.
    text = 'Computing…';
  }

  return (
    <div
      id="loading-overlay"
      className="fixed inset-0 z-[1500] flex items-center justify-center pointer-events-none
        bg-zinc-950/40 dark:bg-zinc-950/40
        [@media(prefers-color-scheme:light)]:bg-white/40"
    >
      <div
        id="loading-text"
        className="px-4 py-2 rounded-full
          bg-zinc-900/95 dark:bg-zinc-900/95
          [@media(prefers-color-scheme:light)]:bg-white/95
          text-sm text-zinc-100 dark:text-zinc-100
          [@media(prefers-color-scheme:light)]:text-zinc-800
          shadow-[0_2px_12px_rgba(0,0,0,0.35)]
          flex items-center gap-2"
      >
        <span
          className="inline-block w-3 h-3 rounded-full border-2 border-current border-t-transparent animate-spin"
          aria-hidden
        />
        {text}
      </div>
    </div>
  );
}
