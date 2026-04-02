import { useAppState } from '../state/AppContext.jsx';

export default function LoadingOverlay() {
  const { state } = useAppState();
  const { loadingState, loadingProgress, currentCity } = state;

  if (loadingState !== 'loading' && loadingState !== 'initializing') return null;

  const text = loadingState === 'initializing'
    ? `Initializing router for ${currentCity?.name}...`
    : `Loading ${currentCity?.name}... ${loadingProgress}%`;

  return (
    <div id="loading-overlay" style={{ display: 'flex' }}>
      <div id="loading-text">{text}</div>
    </div>
  );
}
