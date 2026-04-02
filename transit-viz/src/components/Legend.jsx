import { useAppState } from '../state/AppContext.jsx';
import { legendGradient } from '../utils/colors.js';

export default function Legend() {
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
