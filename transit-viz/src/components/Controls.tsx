import React, { useState, useRef } from 'react';
import { useAppState } from '../state/AppContext';
import { formatTime, formatSlack, dateToYYYYMMDD } from '../utils/format';
import { freeSsspList, freeProfile } from '../utils/router';
import { MAP_STYLES } from '../utils/mapStyles';
import { LegendContent } from './Legend';

interface RangeSliderProps {
  id: string;
  min: number;
  max: number;
  step: number;
  defaultValue: number;
  formatDisplay: (v: number) => string;
  onCommit: (v: number) => void;
}

function RangeSlider({ id, min, max, step, defaultValue, formatDisplay, onCommit }: RangeSliderProps) {
  const [display, setDisplay] = useState(formatDisplay(defaultValue));
  const ref = useRef<HTMLInputElement>(null);

  function handleInput(e: React.FormEvent<HTMLInputElement>) {
    const val = (e.target as HTMLInputElement).value;
    setDisplay(formatDisplay(parseInt(val)));
  }

  function handleCommit() {
    if (ref.current) {
      onCommit(parseInt(ref.current.value));
    }
  }

  return (
    <>
      <span>{display}</span>
      <input
        type="range"
        id={id}
        ref={ref}
        min={min}
        max={max}
        step={step}
        defaultValue={defaultValue}
        className="w-full mb-1"
        onInput={handleInput}
        onMouseUp={handleCommit}
        onTouchEnd={handleCommit}
        onKeyUp={handleCommit}
      />
    </>
  );
}

interface ControlsProps {
  onRunQuery: (overrides?: Record<string, any>) => void;
  onCopy: () => void;
}

export default function Controls({ onRunQuery, onCopy }: ControlsProps): React.ReactNode {
  const { state, dispatch } = useAppState();
  const { loadingState, mode, mapStyle, departureTime, date, nSamples, maxTimeMin, transferSlack, computeStatus, computeTimeMs, patternCount, nodeCount, stopCount, sourceNode, showCopiedMessage } = state;

  const [collapsed, setCollapsed] = useState(() => window.innerWidth < 600);

  if (loadingState !== 'ready') return null;

  const statusText = showCopiedMessage
    ? 'Copied!'
    : computeStatus === 'computing'
      ? 'Computing...'
      : computeStatus === 'done'
        ? `Done. Spent ${Math.round(computeTimeMs)} ms.`
        : computeStatus === 'error'
          ? 'Error'
          : sourceNode !== null
            ? `${nodeCount.toLocaleString()} nodes, ${stopCount.toLocaleString()} stops.`
            : `${nodeCount.toLocaleString()} nodes, ${stopCount.toLocaleString()} stops. Double-click map to set origin.`;

  function handleCopy() {
    onCopy();
    dispatch({ type: 'SHOW_COPIED_MESSAGE' });
    setTimeout(() => dispatch({ type: 'HIDE_COPIED_MESSAGE' }), 1500);
  }

  function handleMapStyleChange(e: React.ChangeEvent<HTMLSelectElement>) {
    dispatch({ type: 'SET_MAP_STYLE', style: e.target.value });
  }

  function handleModeChange(e: React.ChangeEvent<HTMLSelectElement>) {
    const newMode = e.target.value as 'single' | 'sampled';
    dispatch({ type: 'SET_MODE', mode: newMode });
    onRunQuery({ mode: newMode });
  }

  function handleDateChange(e: React.ChangeEvent<HTMLInputElement>) {
    dispatch({ type: 'SET_DATE', value: e.target.value });
    if (state.router) {
      const count = state.router.num_patterns_for_date(dateToYYYYMMDD(e.target.value));
      dispatch({ type: 'SET_PATTERN_COUNT', count });
    }
    onRunQuery({ date: e.target.value });
  }

  function handleChangeCity() {
    freeSsspList(state.ssspList);
    freeProfile(state.profile);
    dispatch({ type: 'CHANGE_CITY' });
    history.replaceState(null, '', import.meta.env.BASE_URL);
  }

  // Base = light theme; dark: overrides for dark theme
  const selectClass =
    'w-full mb-1 px-1.5 py-1 rounded border text-sm ' +
    'bg-white border-zinc-300 text-zinc-900 ' +
    'dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100';

  const dateClass =
    'w-full mb-1 px-1.5 py-1 rounded border text-sm ' +
    'bg-white border-zinc-300 text-zinc-900 ' +
    'dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:[color-scheme:dark]';

  return (
    <div
      id="controls"
      className={[
        // positioning
        'absolute z-[1000]',
        // desktop: top-right panel
        'top-2.5 right-2.5',
        // sizing
        'min-w-[280px] max-h-[calc(100vh-20px)] overflow-y-auto',
        // appearance
        'rounded-lg p-4',
        'bg-white/95 dark:bg-zinc-900/95',
        'shadow-[0_2px_12px_rgba(0,0,0,0.5)]',
        // mobile: bottom sheet
        'max-sm:top-auto max-sm:bottom-0 max-sm:left-0 max-sm:right-0',
        'max-sm:rounded-t-xl max-sm:rounded-b-none max-sm:min-w-0',
        collapsed ? 'max-sm:max-h-12 max-sm:overflow-hidden' : 'max-sm:max-h-[45vh]',
      ].join(' ')}
    >
      {/* Toggle button (mobile only) */}
      <div
        className="text-center py-1 text-xs text-zinc-500 cursor-pointer sm:hidden"
        onClick={() => setCollapsed(!collapsed)}
      >
        {collapsed ? 'Show controls' : 'Hide controls'}
      </div>

      <h3 id="city-title" className="mb-2 text-zinc-900 dark:text-zinc-100 font-semibold">
        {state.currentCity && state.currentCity.name}
      </h3>

      {/* Map Style */}
      <div className="mb-0">
        <label className="block text-[13px] text-zinc-500 dark:text-zinc-400">Map Style</label>
        <select id="map-style" value={mapStyle} onChange={handleMapStyleChange}
          className={selectClass}>
          {Object.entries(MAP_STYLES).map(([id, s]) => (
            <option key={id} value={id}>{s.label}</option>
          ))}
        </select>
      </div>

      {/* Mode */}
      <div className="mb-0">
        <label className="block text-[13px] text-zinc-500 dark:text-zinc-400">Mode</label>
        <select id="mode" value={mode} onChange={handleModeChange}
          className={selectClass}>
          <option value="single">Single Departure Time</option>
          <option value="sampled">Hour-Window Average</option>
        </select>
      </div>

      {/* Date */}
      <div className="mb-0">
        <label className="block text-[13px] text-zinc-500 dark:text-zinc-400">Date</label>
        <input type="date" id="date-picker" value={date} onChange={handleDateChange}
          className={dateClass} />
      </div>

      {/* Departure Time */}
      <div className="mb-0">
        <label className="block text-[13px] text-zinc-500 dark:text-zinc-400">
          Departure Time:{' '}
          <RangeSlider
            id="time-slider"
            min={0}
            max={86400}
            step={300}
            defaultValue={departureTime}
            formatDisplay={(v) => formatTime(v)}
            onCommit={(val) => {
              dispatch({ type: 'SET_DEPARTURE_TIME', value: val });
              onRunQuery({ departureTime: val });
            }}
          />
        </label>
      </div>

      {/* Samples (sampled mode only) */}
      {mode === 'sampled' && (
        <div className="mb-0">
          <label className="block text-[13px] text-zinc-500 dark:text-zinc-400">
            Samples:{' '}
            <RangeSlider
              id="samples-slider"
              min={3}
              max={30}
              step={1}
              defaultValue={nSamples}
              formatDisplay={(v) => `${v}`}
              onCommit={(val) => {
                dispatch({ type: 'SET_SAMPLES', value: val });
                onRunQuery({ nSamples: val });
              }}
            />
          </label>
        </div>
      )}

      {/* Max travel time */}
      <div className="mb-0">
        <label className="block text-[13px] text-zinc-500 dark:text-zinc-400">
          Max travel time:{' '}
          <RangeSlider
            id="maxtime-slider"
            min={10}
            max={180}
            step={5}
            defaultValue={maxTimeMin}
            formatDisplay={(v) => `${v} min`}
            onCommit={(val) => {
              dispatch({ type: 'SET_MAX_TIME', value: val });
              onRunQuery({ maxTimeMin: val });
            }}
          />
        </label>
      </div>

      {/* Transfer slack */}
      <div className="mb-0">
        <label className="block text-[13px] text-zinc-500 dark:text-zinc-400">
          Transfer slack:{' '}
          <RangeSlider
            id="slack-slider"
            min={0}
            max={300}
            step={15}
            defaultValue={transferSlack}
            formatDisplay={(v) => formatSlack(v)}
            onCommit={(val) => {
              dispatch({ type: 'SET_SLACK', value: val });
              onRunQuery({ transferSlack: val });
            }}
          />
        </label>
      </div>

      {/* Pattern info */}
      <div id="pattern-info" className="text-[11px] text-zinc-500 dark:text-zinc-600 mt-1">
        {date}: {patternCount} service pattern{patternCount !== 1 ? 's' : ''} active
      </div>

      {/* Status */}
      <div id="status" className="text-[12px] text-zinc-500 dark:text-zinc-500 mt-2">
        {statusText}
      </div>

      {/* Buttons */}
      <div className="flex gap-2 mt-2">
        <button
          id="change-city"
          onClick={handleChangeCity}
          className="px-2.5 py-1 text-[12px] rounded border cursor-pointer
            bg-zinc-100 border-zinc-300 text-zinc-600
            dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-400
            hover:bg-zinc-200 dark:hover:bg-zinc-700"
        >
          Change city
        </button>
        {state.pinnedNode !== null && (
          <button
            id="copy-info"
            onClick={handleCopy}
            className="px-2.5 py-1 text-[12px] rounded border cursor-pointer
              bg-zinc-100 border-zinc-300 text-zinc-600
              dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-400
              hover:bg-zinc-200 dark:hover:bg-zinc-700"
          >
            Copy info
          </button>
        )}
      </div>
      <div className="sm:hidden mt-3 pt-3 border-t border-zinc-700 dark:border-zinc-700
        [@media(prefers-color-scheme:light)]:border-zinc-200">
        <LegendContent maxMin={state.maxTimeMin} />
      </div>
    </div>
  );
}
