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

interface NominatimReverseResult {
  display_name?: string;
}

// Nominatim usage policy is ~1 request/second per client. Track the last
// outbound reverse-geocode wall-clock time module-wide (shared across the
// From and To inputs) so a leading-edge throttle can fire immediately when
// the window is free, and only delay back-to-back requests.
const REVERSE_MIN_INTERVAL_MS = 1000;
let lastReverseFetchAt = 0;

interface LocationSearchProps {
  mapViewRef: RefObject<MapViewHandle | null>;
  variant: 'desktop' | 'mobile';
}

// ── Shared search input ────────────────────────────────────────────────────────

interface SearchInputProps {
  placeholder: string;
  onSelect: (lat: number, lng: number) => void;
  bbox: [number, number, number, number] | null;
  /** Current source/destination lat/lng — input text reflects its address. */
  latLng: [number, number] | null;
  variant: 'desktop' | 'mobile';
  /** Extra Tailwind classes on the outer wrapper div */
  className?: string;
}

function SearchInput({
  placeholder,
  onSelect,
  bbox,
  latLng,
  variant,
  className = '',
}: SearchInputProps) {
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<NominatimResult[]>([]);
  const [isOpen, setIsOpen] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [activeIdx, setActiveIdx] = useState(-1);
  const inputRef = useRef<HTMLInputElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  // Armed in select() so the immediately-following SET_SOURCE/PIN_DESTINATION
  // dispatch (which carries a node-snapped lat/lng) doesn't trigger a
  // reverse-geocode that would overwrite the human label the user picked.
  // Consumed on the very next reverse-geocode effect run.
  const skipNextReverseRef = useRef(false);
  // True while user keystrokes are the source of `query` changes. Suppresses
  // the forward-search effect when we programmatically setQuery() from
  // reverse-geocode.
  const isUserTypingRef = useRef(false);

  // Debounced Nominatim forward search — only when the user is actively typing.
  // Programmatic setQuery() from reverse-geocode must not trigger a search.
  useEffect(() => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    if (!isUserTypingRef.current) return;
    if (!query.trim() || !bbox) {
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
        const [minLng, minLat, maxLng, maxLat] = bbox;
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
  }, [query, bbox]);

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

  // Reverse-geocode when the bound latLng changes from outside (map click, pin,
  // or a SET_SOURCE from a search-select that snapped to a graph node).
  useEffect(() => {
    if (latLng === null) {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      if (abortRef.current) abortRef.current.abort();
      skipNextReverseRef.current = false;
      isUserTypingRef.current = false;
      setQuery('');
      setResults([]);
      setIsOpen(false);
      return;
    }
    // One-shot: consume the skip armed by a search-select.
    if (skipNextReverseRef.current) {
      skipNextReverseRef.current = false;
      return;
    }

    if (debounceRef.current) clearTimeout(debounceRef.current);
    if (abortRef.current) abortRef.current.abort();

    // Leading-edge: fire immediately if at least REVERSE_MIN_INTERVAL_MS has
    // passed since the last reverse-geocode; otherwise wait just long enough
    // to land on the next allowed slot.
    const delay = Math.max(0, lastReverseFetchAt + REVERSE_MIN_INTERVAL_MS - Date.now());

    debounceRef.current = setTimeout(async () => {
      lastReverseFetchAt = Date.now();
      const controller = new AbortController();
      abortRef.current = controller;
      setIsLoading(true);
      try {
        const url = new URL('https://nominatim.openstreetmap.org/reverse');
        url.searchParams.set('format', 'json');
        url.searchParams.set('lat', String(latLng[0]));
        url.searchParams.set('lon', String(latLng[1]));
        url.searchParams.set('zoom', '18');
        const res = await fetch(url.toString(), {
          signal: controller.signal,
          headers: { 'Accept-Language': 'en' },
        });
        const data: NominatimReverseResult = await res.json();
        if (data.display_name) {
          isUserTypingRef.current = false;
          setQuery(data.display_name);
          setResults([]);
          setIsOpen(false);
        }
      } catch (e: unknown) {
        if (e instanceof Error && e.name !== 'AbortError') {
          // Leave the previous text in place.
        }
      } finally {
        setIsLoading(false);
      }
    }, delay);

    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [latLng]);

  function select(result: NominatimResult) {
    const lat = parseFloat(result.lat);
    const lng = parseFloat(result.lon);
    // Arm the one-shot snap-skip so the imminent SET_SOURCE dispatch with the
    // node-snapped lat/lng doesn't overwrite this human label.
    skipNextReverseRef.current = true;
    isUserTypingRef.current = false;
    setQuery(result.display_name);
    setResults([]);
    setIsOpen(false);
    inputRef.current?.blur();
    onSelect(lat, lng);
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
      if (results[idx]) select(results[idx]);
    } else if (e.key === 'Escape') {
      setIsOpen(false);
      setActiveIdx(-1);
    }
  }

  const isDesktop = variant === 'desktop';
  const iconSize = isDesktop ? 14 : 12;

  const inputCls = isDesktop
    ? [
        'w-full bg-transparent text-zinc-900 dark:text-zinc-100',
        'placeholder-zinc-400 dark:placeholder-zinc-500',
        'pl-8 pr-3 py-1.5 text-[13px]',
        'focus:outline-none',
      ].join(' ')
    : [
        'w-full bg-white text-zinc-900 dark:bg-zinc-800 dark:text-zinc-100',
        'placeholder-zinc-400 dark:placeholder-zinc-500',
        'border border-zinc-300 dark:border-zinc-700 rounded-md',
        'pl-7 pr-2.5 py-1 text-[12px]',
        'focus:outline-none focus:ring-1 focus:ring-blue-500/60',
      ].join(' ');

  const dropdownCls = isDesktop
    ? [
        'absolute left-0 right-0 top-full z-10',
        'bg-white dark:bg-zinc-900',
        'border border-zinc-200 dark:border-zinc-700 rounded-lg',
        'shadow-[0_4px_16px_rgba(0,0,0,0.4)]',
        'overflow-hidden mt-1',
      ].join(' ')
    : [
        'absolute left-0 right-0 top-full mt-0.5 z-[1200]',
        'bg-white dark:bg-zinc-900',
        'border border-zinc-200 dark:border-zinc-700 rounded-md',
        'shadow-[0_4px_16px_rgba(0,0,0,0.4)] dark:shadow-[0_4px_16px_rgba(0,0,0,0.6)]',
        'overflow-hidden',
      ].join(' ');

  const resultCls = (active: boolean) =>
    isDesktop
      ? [
          'w-full text-left px-3 py-2 text-[12px] leading-snug whitespace-normal break-words',
          'text-zinc-800 dark:text-zinc-200',
          active ? 'bg-blue-50 dark:bg-zinc-700' : 'hover:bg-zinc-100 dark:hover:bg-zinc-800',
        ].join(' ')
      : [
          'w-full text-left px-2.5 py-1.5 text-[11px] leading-snug whitespace-normal break-words',
          'text-zinc-800 dark:text-zinc-200',
          active ? 'bg-blue-50 dark:bg-zinc-700' : 'hover:bg-zinc-100 dark:hover:bg-zinc-800',
        ].join(' ');

  return (
    <div ref={containerRef} className={`relative ${className}`}>
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
            isDesktop ? 'left-2.5 text-zinc-400' : 'left-2 text-zinc-500',
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
          onChange={(e) => {
            isUserTypingRef.current = true;
            setQuery(e.target.value);
          }}
          onKeyDown={onKeyDown}
          onFocus={() => {
            if (results.length > 0) setIsOpen(true);
          }}
          placeholder={placeholder}
          className={inputCls}
          aria-label={placeholder}
          aria-autocomplete="list"
          aria-expanded={isOpen}
          autoComplete="off"
          spellCheck={false}
        />
        {isLoading && (
          <span
            className={[
              'absolute top-1/2 -translate-y-1/2 pointer-events-none',
              isDesktop ? 'right-2.5' : 'right-2',
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
                select(r);
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

// ── Main LocationSearch wrapper ────────────────────────────────────────────────

export default function LocationSearch({
  mapViewRef,
  variant,
}: LocationSearchProps): React.ReactNode {
  const { state } = useAppState();
  const { currentCity, loadingState, interactionMode, sourceNode } = state;

  if (loadingState !== 'ready') return null;

  const bbox = currentCity?.bbox ?? null;

  function handleOriginSelect(lat: number, lng: number) {
    mapViewRef.current?.flyTo(lat, lng);
    // keepDest=true: a search-bar origin change is a "swap origin" gesture,
    // not a "start over" gesture — preserve any pinned destination so the
    // route just re-resolves to the same destination from the new origin.
    mapViewRef.current?.setSource(lat, lng, { keepDest: true });
  }

  function handleDestSelect(lat: number, lng: number) {
    mapViewRef.current?.flyTo(lat, lng);
    mapViewRef.current?.setDestination(lat, lng);
  }

  if (variant === 'desktop') {
    return (
      <div
        className={[
          'w-[240px] focus-within:w-[360px]',
          'transition-[width] duration-150 ease-out',
          'bg-white/95 dark:bg-zinc-900/95',
          'border border-zinc-200 dark:border-zinc-700 rounded-lg',
          'shadow-[0_2px_12px_rgba(0,0,0,0.4)]',
          'overflow-visible',
        ].join(' ')}
      >
        <SearchInput
          placeholder="From…"
          onSelect={handleOriginSelect}
          bbox={bbox}
          latLng={state.sourceLatLng}
          variant="desktop"
        />
        {sourceNode !== null && (
          <>
            <div className="border-t border-zinc-100 dark:border-zinc-800 mx-2" />
            <SearchInput
              placeholder="To…"
              onSelect={handleDestSelect}
              bbox={bbox}
              latLng={state.pinnedDest?.latLng ?? null}
              variant="desktop"
            />
          </>
        )}
      </div>
    );
  }

  // Mobile: render both inputs and hide the inactive one via CSS. This keeps
  // each input's local state (query text, refs) and its previously-resolved
  // address intact across mode toggles — switching from Dest back to Origin
  // shows the origin's already-known address without a fresh Nominatim call.
  return (
    <>
      <SearchInput
        placeholder="Search origin…"
        onSelect={handleOriginSelect}
        bbox={bbox}
        latLng={state.sourceLatLng}
        variant="mobile"
        className={interactionMode === 'origin' ? 'w-full' : 'hidden'}
      />
      <SearchInput
        placeholder="Search destination…"
        onSelect={handleDestSelect}
        bbox={bbox}
        latLng={state.pinnedDest?.latLng ?? null}
        variant="mobile"
        className={interactionMode === 'dest' ? 'w-full' : 'hidden'}
      />
    </>
  );
}
