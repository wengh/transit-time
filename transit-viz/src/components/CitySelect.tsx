import React from 'react';
import { CITIES } from '../cities';
import { useAppState } from '../state/AppContext';
import { loadCity } from '../utils/cityLoader';

export default function CitySelect(): React.ReactNode {
  const { state, dispatch } = useAppState();

  if (state.currentCity) return null;

  async function handleCityClick(city: typeof CITIES[0]) {
    history.replaceState(null, '', `/${city.id}`);
    try {
      await loadCity(city, dispatch, false);
    } catch (e) {
      history.replaceState(null, '', '/');
      alert(`Failed to load ${city.name}: ${(e as Error).message}`);
    }
  }

  return (
    <div id="city-select">
      <div id="city-select-inner">
        <h1>Transit Isochrone</h1>
        <p>Select a city to visualize transit travel times</p>
        <ul className="city-list" id="city-list">
          {CITIES.map((city) => (
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
