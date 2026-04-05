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
    <div id="loading-overlay" style={{ display: 'flex' }}>
      <div id="loading-text">{text}</div>
    </div>
  );
}
