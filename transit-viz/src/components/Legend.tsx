import React from 'react';
import { useAppState } from '../state/AppContext';
import { legendGradient } from '../utils/colors';

export default function Legend(): React.ReactNode {
  const { state } = useAppState();
  if (state.loadingState !== 'ready') return null;

  const maxMin = state.maxTimeMin;
  const maxSec = maxMin * 60;

  return (
    <div
      id="legend"
      className="absolute bottom-5 left-2.5 z-[1000]
        bg-zinc-900 dark:bg-zinc-900
        [@media(prefers-color-scheme:light)]:bg-white
        p-3 rounded-lg shadow-[0_2px_12px_rgba(0,0,0,0.5)]"
    >
      <div
        id="legend-gradient"
        className="w-[200px] h-5 rounded"
        style={{ background: legendGradient(maxSec) }}
      />
      <div
        id="legend-labels"
        className="flex justify-between text-[11px] mt-1
          text-zinc-500 dark:text-zinc-500
          [@media(prefers-color-scheme:light)]:text-zinc-500"
      >
        <span>0 min</span>
        <span>{Math.round(maxMin / 2)}</span>
        <span>{maxMin} min</span>
      </div>
    </div>
  );
}
