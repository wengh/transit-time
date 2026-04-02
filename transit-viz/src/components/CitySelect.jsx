import { CITIES } from '../cities.js';
import { useAppState } from '../state/AppContext.jsx';
import { initWasm, loadRouter } from '../utils/router.js';

export default function CitySelect() {
  const { state, dispatch } = useAppState();

  if (state.currentCity) return null;

  async function handleCityClick(city) {
    dispatch({ type: 'START_LOADING', city });
    history.replaceState(null, '', `/${city.id}`);
    try {
      await initWasm();
      const router = await loadRouter(city.file, (pct) => {
        dispatch({ type: 'LOADING_PROGRESS', progress: pct });
      });
      dispatch({ type: 'START_INITIALIZING' });
      const nodeCoords = router.all_node_coords();
      dispatch({
        type: 'CITY_LOADED',
        router,
        nodeCoords,
        nodeCount: router.num_nodes(),
        stopCount: router.num_stops(),
      });
    } catch (e) {
      dispatch({ type: 'LOAD_ERROR' });
      history.replaceState(null, '', '/');
      alert(`Failed to load ${city.name}: ${e.message}`);
    }
  }

  return (
    <div id="city-select">
      <div id="city-select-inner">
        <h1>Transit Isochrone</h1>
        <p>Select a city to visualize transit travel times</p>
        <ul className="city-list" id="city-list">
          {CITIES.map(city => (
            <li key={city.id}>
              <button className="city-btn" onClick={() => handleCityClick(city)}>
                <div className="city-name">{city.name}</div>
                <div className="city-detail">{city.detail}</div>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

// Auto-load city from URL
export function getCityFromUrl() {
  const path = window.location.pathname.replace(/^\//, '').replace(/\/$/, '');
  return CITIES.find(c => c.id === path) || null;
}
