import { initWasm, loadRouter, numPatternsForDate } from './router';
import { dateToYYYYMMDD } from './format';
import type { Action } from '../state/reducer';
import type { City } from '../cities';
import { animationStore } from '../state/animationStore';

export async function loadCity(
  city: City,
  dispatch: React.Dispatch<Action>,
  includePatternCount: boolean = false
): Promise<{ nodeCoords: Float32Array }> {
  dispatch({ type: 'START_LOADING', city });
  // A new city means a new graph — discard any timeline state from the old one.
  animationStore.reset();

  try {
    await initWasm();
    const { nodeCoords, nodeCount, stopCount, routeColors } = await loadRouter(city.file, (pct) => {
      dispatch({ type: 'LOADING_PROGRESS', progress: pct });
    });
    dispatch({ type: 'START_INITIALIZING' });
    dispatch({
      type: 'CITY_LOADED',
      nodeCoords,
      nodeCount,
      stopCount,
      routeColors,
    });

    // Get pattern count for today
    if (includePatternCount) {
      const today = new Date().toISOString().slice(0, 10);
      const count = await numPatternsForDate(dateToYYYYMMDD(today));
      dispatch({ type: 'SET_PATTERN_COUNT', count });
    }

    return { nodeCoords };
  } catch (e) {
    dispatch({ type: 'LOAD_ERROR' });
    throw e;
  }
}
