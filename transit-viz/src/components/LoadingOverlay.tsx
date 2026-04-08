import React from 'react';
import { useAppState } from '../state/AppContext';

export default function LoadingOverlay(): React.ReactNode {
  const { state } = useAppState();
  const { loadingState, loadingProgress, currentCity } = state;

  if (loadingState !== 'loading' && loadingState !== 'initializing') return null;

  const text =
    loadingState === 'initializing'
      ? `Initializing router for ${currentCity && currentCity.name}...`
      : `Loading ${currentCity && currentCity.name}... ${loadingProgress}%`;

  return (
    <div
      id="loading-overlay"
      className="fixed inset-0 z-[1500] flex items-center justify-center
        bg-zinc-950/92 dark:bg-zinc-950/92
        [@media(prefers-color-scheme:light)]:bg-white/92
        text-base text-zinc-500"
    >
      <div id="loading-text">{text}</div>
    </div>
  );
}
