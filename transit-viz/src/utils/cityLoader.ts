import { initWasm, loadRouter } from './router';
import type { Action } from '../state/reducer';
import type { City } from '../cities';
import { animationStore } from '../state/animationStore';

export async function loadCity(
  city: City,
  dispatch: React.Dispatch<Action>
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

    return { nodeCoords };
  } catch (e) {
    dispatch({ type: 'LOAD_ERROR' });
    throw e;
  }
}
