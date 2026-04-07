import { initWasm, loadRouter } from './router';
import type { Router } from './router';
import { dateToYYYYMMDD } from './format';
import type { Action } from '../state/reducer';
import type { City } from '../cities';

export async function loadCity(
  city: City,
  dispatch: React.Dispatch<Action>,
  includePatternCount: boolean = false,
): Promise<{ router: Router; nodeCoords: Float32Array }> {
  dispatch({ type: 'START_LOADING', city });

  try {
    await initWasm();
    const router = await loadRouter(city.file, (pct) => {
      dispatch({ type: 'LOADING_PROGRESS', progress: pct });
    });
    dispatch({ type: 'START_INITIALIZING' });
    const allCoords = router.all_node_coords();
    // Convert Float64Array to Float32Array for storage
    const nodeCoords = new Float32Array(allCoords);
    dispatch({
      type: 'CITY_LOADED',
      router,
      nodeCoords,
      nodeCount: router.num_nodes(),
      stopCount: router.num_stops(),
    });

    // Get pattern count for today
    if (includePatternCount) {
      const today = new Date().toISOString().slice(0, 10);
      const count = router.num_patterns_for_date(dateToYYYYMMDD(today));
      dispatch({ type: 'SET_PATTERN_COUNT', count });
    }

    return { router, nodeCoords };
  } catch (e) {
    dispatch({ type: 'LOAD_ERROR' });
    throw e;
  }
}
