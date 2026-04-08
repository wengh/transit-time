import React from 'react';
import { CITIES } from '../cities';
import { useAppState } from '../state/AppContext';
import { loadCity } from '../utils/cityLoader';

export default function CitySelect(): React.ReactNode {
  const { state, dispatch } = useAppState();

  if (state.currentCity) return null;

  async function handleCityClick(city: typeof CITIES[0]) {
    history.replaceState(null, '', `${import.meta.env.BASE_URL}?city=${city.id}`);
    try {
      await loadCity(city, dispatch, false);
    } catch (e) {
      history.replaceState(null, '', import.meta.env.BASE_URL);
      alert(`Failed to load ${city.name}: ${String(e)}`);
    }
  }

  return (
    <div
      id="city-select"
      className="fixed inset-0 z-[2000] flex items-center justify-center
        bg-zinc-950 dark:bg-zinc-950
        [@media(prefers-color-scheme:light)]:bg-zinc-100"
    >
      <div
        id="city-select-inner"
        className="bg-zinc-900 dark:bg-zinc-900
          [@media(prefers-color-scheme:light)]:bg-white
          p-10 rounded-xl shadow-[0_4px_24px_rgba(0,0,0,0.5)]
          max-w-[520px] w-[90%] text-center"
      >
        <h1 className="text-2xl mb-2 text-zinc-100 dark:text-zinc-100
          [@media(prefers-color-scheme:light)]:text-zinc-900 font-semibold">
          Transit Isochrone
        </h1>
        <p className="text-zinc-500 dark:text-zinc-500
          [@media(prefers-color-scheme:light)]:text-zinc-500
          mb-6 text-sm">
          Select a city to visualize transit travel times
        </p>
        <ul className="city-list list-none p-0" id="city-list">
          {CITIES.map((city) => (
            <li key={city.id} className="mb-2.5">
              <button
                className="city-btn block w-full px-5 py-3.5 text-left rounded-lg border
                  cursor-pointer text-[15px] transition-[border-color,box-shadow] duration-150
                  bg-zinc-800 border-zinc-700 text-zinc-100
                  dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100
                  [@media(prefers-color-scheme:light)]:bg-zinc-100
                  [@media(prefers-color-scheme:light)]:border-zinc-300
                  [@media(prefers-color-scheme:light)]:text-zinc-900
                  hover:border-blue-500 hover:shadow-[0_0_0_3px_rgba(74,144,217,0.2)]"
                onClick={() => handleCityClick(city)}
              >
                <div className="city-name font-semibold">{city.name}</div>
                <div className="city-detail text-xs text-zinc-500 dark:text-zinc-500
                  [@media(prefers-color-scheme:light)]:text-zinc-500 mt-0.5">
                  {city.detail}
                </div>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
