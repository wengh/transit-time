import React from 'react';
import { useAppState } from '../state/AppContext';
import { legendGradient } from '../utils/colors';

export default function Legend(): React.ReactNode {
  const { state } = useAppState();
  if (state.loadingState !== 'ready') return null;

  const maxMin = state.maxTimeMin;
  const maxSec = maxMin * 60;

  return (
    <div id="legend">
      <div id="legend-gradient" style={{ background: legendGradient(maxSec) }} />
      <div id="legend-labels">
        <span>0 min</span>
        <span>{Math.round(maxMin / 2)}</span>
        <span>{maxMin} min</span>
      </div>
    </div>
  );
}
