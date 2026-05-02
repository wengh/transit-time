import React, { useState, useMemo } from 'react';
import { CITIES } from '../cities';
import { useAppState } from '../state/AppContext';
import { loadCity } from '../utils/cityLoader';

const TAG_ORDER = ['usa', 'canada', 'subway', 'rail', 'bus', 'ferry'];

export default function CitySelect(): React.ReactNode {
  const { state, dispatch } = useAppState();
  const [search, setSearch] = useState('');
  const [selectedTags, setSelectedTags] = useState<Set<string>>(new Set());

  const allTags = useMemo(() => {
    const tagSet = new Set(CITIES.flatMap((c) => c.tags ?? []));
    return TAG_ORDER.filter((t) => tagSet.has(t));
  }, []);

  const filteredCities = useMemo(() => {
    const q = search.trim().toLowerCase();
    return CITIES.filter((city) => {
      if (selectedTags.size > 0) {
        const cityTags = city.tags ?? [];
        for (const tag of selectedTags) {
          if (!cityTags.includes(tag)) return false;
        }
      }
      if (q) {
        const haystack = `${city.name} ${city.detail}`.toLowerCase();
        if (!haystack.includes(q)) return false;
      }
      return true;
    });
  }, [search, selectedTags]);

  if (state.currentCity) return null;

  async function handleCityClick(city: (typeof CITIES)[0]) {
    history.replaceState(null, '', `${import.meta.env.BASE_URL}?city=${city.id}`);
    try {
      await loadCity(city, dispatch, false);
    } catch (e) {
      history.replaceState(null, '', import.meta.env.BASE_URL);
      alert(`Failed to load ${city.name}: ${String(e)}`);
    }
  }

  function toggleTag(tag: string) {
    setSelectedTags((prev) => {
      const next = new Set(prev);
      if (next.has(tag)) next.delete(tag);
      else next.add(tag);
      return next;
    });
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
        <h1
          className="text-2xl mb-2 text-zinc-100 dark:text-zinc-100
          [@media(prefers-color-scheme:light)]:text-zinc-900 font-semibold"
        >
          Transit Isochrone
        </h1>
        <p
          className="text-zinc-500 dark:text-zinc-500
          [@media(prefers-color-scheme:light)]:text-zinc-500
          mb-4 text-sm"
        >
          Select a city to visualize transit travel times
        </p>

        <input
          type="search"
          placeholder="Search cities…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="w-full px-3 py-2 mb-3 rounded-lg border text-sm
            bg-zinc-800 border-zinc-700 text-zinc-100 placeholder-zinc-500
            dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100
            [@media(prefers-color-scheme:light)]:bg-zinc-100
            [@media(prefers-color-scheme:light)]:border-zinc-300
            [@media(prefers-color-scheme:light)]:text-zinc-900
            focus:outline-none focus:border-blue-500 focus:shadow-[0_0_0_3px_rgba(74,144,217,0.2)]"
        />

        <div className="flex flex-wrap gap-1.5 justify-center mb-4">
          {allTags.map((tag) => {
            const active = selectedTags.has(tag);
            return (
              <button
                key={tag}
                onClick={() => toggleTag(tag)}
                className={`px-2.5 py-1 rounded-full text-xs font-medium border transition-colors duration-100
                  ${
                    active
                      ? 'bg-blue-600 border-blue-500 text-white'
                      : `bg-zinc-800 border-zinc-600 text-zinc-400
                       dark:bg-zinc-800 dark:border-zinc-600 dark:text-zinc-400
                       [@media(prefers-color-scheme:light)]:bg-zinc-100
                       [@media(prefers-color-scheme:light)]:border-zinc-300
                       [@media(prefers-color-scheme:light)]:text-zinc-600
                       hover:border-blue-500 hover:text-blue-400`
                  }`}
              >
                {tag}
              </button>
            );
          })}
        </div>

        <ul className="city-list list-none p-0 overflow-y-auto h-[440px]" id="city-list">
          {filteredCities.length === 0 && (
            <li className="text-zinc-500 text-sm py-4">No cities match your filters.</li>
          )}
          {filteredCities.map((city) => (
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
                <div
                  className="city-detail text-xs text-zinc-500 dark:text-zinc-500
                  [@media(prefers-color-scheme:light)]:text-zinc-500 mt-0.5"
                >
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
