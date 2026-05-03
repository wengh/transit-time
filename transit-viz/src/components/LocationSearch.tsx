import React, { useEffect, useRef, useState } from 'react';
import type { RefObject } from 'react';
import { useAppState } from '../state/AppContext';
import type { MapViewHandle } from './MapView';

interface NominatimResult {
  place_id: number;
  display_name: string;
  lat: string;
  lon: string;
}

interface LocationSearchProps {
  mapViewRef: RefObject<MapViewHandle | null>;
  variant: 'desktop' | 'mobile';
}

export default function LocationSearch({
  mapViewRef,
  variant,
}: LocationSearchProps): React.ReactNode {
  const { state } = useAppState();
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<NominatimResult[]>([]);
  const [isOpen, setIsOpen] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [activeIdx, setActiveIdx] = useState(-1);
  const inputRef = useRef<HTMLInputElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  const { currentCity, loadingState } = state;

  // Debounced Nominatim search
  useEffect(() => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    if (!query.trim() || !currentCity) {
      setResults([]);
      setIsOpen(false);
      return;
    }

    debounceRef.current = setTimeout(async () => {
      if (abortRef.current) abortRef.current.abort();
      const controller = new AbortController();
      abortRef.current = controller;

      setIsLoading(true);
      try {
        const [minLng, minLat, maxLng, maxLat] = currentCity.bbox;
        // Nominatim viewbox: left,top,right,bottom = minLng,maxLat,maxLng,minLat
        const viewbox = `${minLng},${maxLat},${maxLng},${minLat}`;
        const url = new URL('https://nominatim.openstreetmap.org/search');
        url.searchParams.set('q', query.trim());
        url.searchParams.set('format', 'json');
        url.searchParams.set('viewbox', viewbox);
        url.searchParams.set('bounded', '1');
        url.searchParams.set('limit', '5');

        const res = await fetch(url.toString(), {
          signal: controller.signal,
          headers: { 'Accept-Language': 'en' },
        });
        const data: NominatimResult[] = await res.json();
        setResults(data);
        setIsOpen(data.length > 0);
        setActiveIdx(-1);
      } catch (e: unknown) {
        if (e instanceof Error && e.name !== 'AbortError') {
          setResults([]);
          setIsOpen(false);
        }
      } finally {
        setIsLoading(false);
      }
    }, 300);

    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [query, currentCity]);

  // Close dropdown on outside click
  useEffect(() => {
    function onPointerDown(e: PointerEvent) {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setIsOpen(false);
      }
    }
    document.addEventListener('pointerdown', onPointerDown);
    return () => document.removeEventListener('pointerdown', onPointerDown);
  }, []);

  // Reset on city change
  useEffect(() => {
    setQuery('');
    setResults([]);
    setIsOpen(false);
  }, [currentCity]);

  function selectResult(result: NominatimResult) {
    const lat = parseFloat(result.lat);
    const lng = parseFloat(result.lon);
    mapViewRef.current?.flyTo(lat, lng);
    mapViewRef.current?.setSource(lat, lng);
    setQuery('');
    setResults([]);
    setIsOpen(false);
    inputRef.current?.blur();
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (!isOpen) return;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setActiveIdx((i) => Math.min(i + 1, results.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActiveIdx((i) => Math.max(i - 1, -1));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const idx = activeIdx >= 0 ? activeIdx : 0;
      if (results[idx]) selectResult(results[idx]);
    } else if (e.key === 'Escape') {
      setIsOpen(false);
      setActiveIdx(-1);
    }
  }

  if (loadingState !== 'ready') return null;

  const inputCls =
    variant === 'desktop'
      ? [
          'w-full bg-white/95 dark:bg-zinc-900/95 text-zinc-900 dark:text-zinc-100',
          'placeholder-zinc-400 dark:placeholder-zinc-500',
          'border border-zinc-200 dark:border-zinc-700 rounded-lg',
          'pl-8 pr-3 py-1.5 text-[13px]',
          'focus:outline-none focus:ring-2 focus:ring-blue-500/50',
          'shadow-[0_2px_8px_rgba(0,0,0,0.35)]',
        ].join(' ')
      : [
          'w-full bg-zinc-800 text-zinc-100',
          'placeholder-zinc-500',
          'border border-zinc-700 rounded-md',
          'pl-7 pr-2.5 py-1 text-[12px]',
          'focus:outline-none focus:ring-1 focus:ring-blue-500/60',
        ].join(' ');

  const dropdownCls =
    variant === 'desktop'
      ? [
          'absolute left-0 right-0 top-full mt-1 z-10',
          'bg-white dark:bg-zinc-900',
          'border border-zinc-200 dark:border-zinc-700 rounded-lg',
          'shadow-[0_4px_16px_rgba(0,0,0,0.4)]',
          'overflow-hidden',
        ].join(' ')
      : [
          'absolute left-0 right-0 top-full mt-0.5 z-[1200]',
          'bg-zinc-900',
          'border border-zinc-700 rounded-md',
          'shadow-[0_4px_16px_rgba(0,0,0,0.6)]',
          'overflow-hidden',
        ].join(' ');

  const resultCls = (active: boolean) =>
    variant === 'desktop'
      ? [
          'w-full text-left px-3 py-2 text-[12px] leading-snug truncate',
          'text-zinc-800 dark:text-zinc-200',
          active ? 'bg-blue-50 dark:bg-zinc-700' : 'hover:bg-zinc-100 dark:hover:bg-zinc-800',
        ].join(' ')
      : [
          'w-full text-left px-2.5 py-1.5 text-[11px] leading-snug truncate',
          'text-zinc-200',
          active ? 'bg-zinc-700' : 'hover:bg-zinc-800',
        ].join(' ');

  const iconSize = variant === 'desktop' ? 14 : 12;

  return (
    <div
      ref={containerRef}
      className={variant === 'desktop' ? 'relative w-[240px]' : 'relative w-full'}
    >
      <div className="relative">
        <svg
          viewBox="0 0 24 24"
          width={iconSize}
          height={iconSize}
          fill="none"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={[
            'absolute top-1/2 -translate-y-1/2 pointer-events-none',
            variant === 'desktop'
              ? 'left-2.5 text-zinc-400'
              : 'left-2 text-zinc-500',
          ].join(' ')}
          aria-hidden="true"
        >
          <circle cx="11" cy="11" r="8" />
          <line x1="21" y1="21" x2="16.65" y2="16.65" />
        </svg>
        <input
          ref={inputRef}
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
          onFocus={() => {
            if (results.length > 0) setIsOpen(true);
          }}
          placeholder="Search location…"
          className={inputCls}
          aria-label="Search for a location"
          aria-autocomplete="list"
          aria-expanded={isOpen}
          autoComplete="off"
          spellCheck={false}
        />
        {isLoading && (
          <span
            className={[
              'absolute top-1/2 -translate-y-1/2 pointer-events-none',
              variant === 'desktop' ? 'right-2.5' : 'right-2',
            ].join(' ')}
            aria-hidden="true"
          >
            <svg
              width={iconSize}
              height={iconSize}
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.5"
              className="animate-spin text-zinc-400"
            >
              <circle cx="12" cy="12" r="10" strokeOpacity="0.2" />
              <path d="M12 2a10 10 0 0 1 10 10" />
            </svg>
          </span>
        )}
      </div>

      {isOpen && results.length > 0 && (
        <div className={dropdownCls} role="listbox">
          {results.map((r, i) => (
            <button
              key={r.place_id}
              role="option"
              aria-selected={i === activeIdx}
              className={resultCls(i === activeIdx)}
              onPointerDown={(e) => {
                e.preventDefault();
                selectResult(r);
              }}
              onMouseEnter={() => setActiveIdx(i)}
            >
              {r.display_name}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
