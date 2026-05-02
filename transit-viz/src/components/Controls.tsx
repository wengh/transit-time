import React, { useState, useRef, useCallback } from 'react';
import { useAppState } from '../state/AppContext';
import { formatTime, formatSlack, dateToYYYYMMDD } from '../utils/format';
import { freeProfile, numPatternsForDate } from '../utils/router';
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

function RangeSlider({
  id,
  min,
  max,
  step,
  defaultValue,
  formatDisplay,
  onCommit,
}: RangeSliderProps) {
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

// ── Dual-ended range slider ────────────────────────────────────────────────

const STEP = 300; // 5 minutes
const SERVICE_WINDOW_MAX = 27 * 3600; // GTFS service-day times can extend past midnight.

interface DualRangeSliderProps {
  windowStart: number;
  windowEnd: number;
  onChange: (start: number, end: number) => void;
  onCommit: (start: number, end: number) => void;
}

function DualRangeSlider({ windowStart, windowEnd, onChange, onCommit }: DualRangeSliderProps) {
  const trackRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{
    kind: 'start' | 'end' | 'middle';
    originX: number;
    origStart: number;
    origEnd: number;
  } | null>(null);
  const liveRef = useRef({ start: windowStart, end: windowEnd });
  liveRef.current = { start: windowStart, end: windowEnd };

  const pct = (v: number) => (v / SERVICE_WINDOW_MAX) * 100;

  const snap = (v: number) => Math.round(v / STEP) * STEP;
  const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

  const xToSec = useCallback((clientX: number) => {
    const rect = trackRef.current!.getBoundingClientRect();
    return clamp(
      snap(((clientX - rect.left) / rect.width) * SERVICE_WINDOW_MAX),
      0,
      SERVICE_WINDOW_MAX
    );
  }, []);

  const handlePointerDown = useCallback(
    (kind: 'start' | 'end' | 'middle', e: React.PointerEvent) => {
      e.preventDefault();
      (e.target as HTMLElement).setPointerCapture(e.pointerId);
      dragRef.current = {
        kind,
        originX: e.clientX,
        origStart: liveRef.current.start,
        origEnd: liveRef.current.end,
      };
    },
    []
  );

  const handlePointerMove = useCallback(
    (e: React.PointerEvent) => {
      const d = dragRef.current;
      if (!d) return;
      const { kind, origStart, origEnd } = d;
      const sec = xToSec(e.clientX);
      let s = liveRef.current.start,
        en = liveRef.current.end;

      if (kind === 'start') {
        s = clamp(sec, 0, en - STEP);
      } else if (kind === 'end') {
        en = clamp(sec, s + STEP, SERVICE_WINDOW_MAX);
      } else {
        const dur = origEnd - origStart;
        const rect = trackRef.current!.getBoundingClientRect();
        const dx = e.clientX - d.originX;
        const dSec = snap((dx / rect.width) * SERVICE_WINDOW_MAX);
        s = clamp(origStart + dSec, 0, SERVICE_WINDOW_MAX - dur);
        en = s + dur;
      }
      s = snap(s);
      en = snap(en);
      onChange(s, en);
    },
    [xToSec, onChange]
  );

  const handlePointerUp = useCallback(
    (e: React.PointerEvent) => {
      if (!dragRef.current) return;
      dragRef.current = null;
      (e.target as HTMLElement).releasePointerCapture(e.pointerId);
      onCommit(liveRef.current.start, liveRef.current.end);
    },
    [onCommit]
  );

  const leftPct = pct(windowStart);
  const widthPct = pct(windowEnd - windowStart);

  return (
    <div
      ref={trackRef}
      className="relative w-full h-5 mb-1 select-none touch-none cursor-grab active:cursor-grabbing"
      onPointerDown={(e) => handlePointerDown('middle', e)}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
    >
      {/* Track background */}
      <div
        className="absolute top-[9px] left-0 right-0 h-[3px] rounded bg-zinc-600 dark:bg-zinc-600
        [@media(prefers-color-scheme:light)]:bg-zinc-300 pointer-events-none"
      />
      {/* Active range */}
      <div
        className="absolute top-[9px] h-[3px] rounded bg-blue-500 pointer-events-none"
        style={{ left: `${leftPct}%`, width: `${widthPct}%` }}
      />
      {/* Start thumb */}
      <div
        className="absolute top-[5px] w-[12px] h-[12px] rounded-full bg-white border-2 border-blue-500 cursor-ew-resize -translate-x-1/2 z-10"
        style={{ left: `${leftPct}%` }}
        onPointerDown={(e) => {
          e.stopPropagation();
          handlePointerDown('start', e);
        }}
      />
      {/* End thumb */}
      <div
        className="absolute top-[5px] w-[12px] h-[12px] rounded-full bg-white border-2 border-blue-500 cursor-ew-resize -translate-x-1/2 z-10"
        style={{ left: `${leftPct + widthPct}%` }}
        onPointerDown={(e) => {
          e.stopPropagation();
          handlePointerDown('end', e);
        }}
      />
    </div>
  );
}

interface ControlsProps {
  onRunQuery: (overrides?: Record<string, any>) => void;
  onCopy: () => void;
  isFront: boolean;
  onActivate: () => void;
}

export default function Controls({
  onRunQuery,
  onCopy,
  isFront,
  onActivate,
}: ControlsProps): React.ReactNode {
  const { state, dispatch } = useAppState();
  const justActivatedRef = useRef(false);
  const {
    loadingState,
    mapStyle,
    windowStart,
    windowEnd,
    date,
    maxTimeMin,
    transferSlack,
    computeStatus,
    computeProgress,
    computeTimeMs,
    computeNumThreads,
    patternCount,
    nodeCount,
    stopCount,
    sourceNode,
    showCopiedMessage,
  } = state;

  const [collapsed, setCollapsed] = useState(() => window.innerWidth < 600);
  // Live (dragging) window values — committed on pointer up
  const [liveStart, setLiveStart] = useState(windowStart);
  const [liveEnd, setLiveEnd] = useState(windowEnd);
  // Sync live values when committed state changes (e.g. URL restore)
  const prevStart = useRef(windowStart);
  const prevEnd = useRef(windowEnd);
  if (prevStart.current !== windowStart || prevEnd.current !== windowEnd) {
    prevStart.current = windowStart;
    prevEnd.current = windowEnd;
    setLiveStart(windowStart);
    setLiveEnd(windowEnd);
  }

  if (loadingState !== 'ready') return null;

  const statusText = showCopiedMessage
    ? 'Copied!'
    : computeStatus === 'computing'
      ? computeProgress
        ? `Computing... ${Math.round((computeProgress.done / computeProgress.total) * 100)}%`
        : 'Computing...'
      : computeStatus === 'done'
        ? `Done. Spent ${Math.round(computeTimeMs)} ms using ${computeNumThreads} thread${computeNumThreads === 1 ? '' : 's'}.`
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

  function handleDateChange(e: React.ChangeEvent<HTMLInputElement>) {
    dispatch({ type: 'SET_DATE', value: e.target.value });
    numPatternsForDate(dateToYYYYMMDD(e.target.value)).then((count) => {
      dispatch({ type: 'SET_PATTERN_COUNT', count });
    });
    onRunQuery({ date: e.target.value });
  }

  function handleChangeCity() {
    freeProfile();
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
      onPointerDownCapture={(e) => {
        if (!isFront) {
          justActivatedRef.current = true;
          onActivate();
          e.stopPropagation();
          e.preventDefault();
        }
      }}
      onClickCapture={(e) => {
        if (justActivatedRef.current) {
          justActivatedRef.current = false;
          e.stopPropagation();
          e.preventDefault();
        }
      }}
      className={[
        // positioning
        `absolute ${isFront ? 'z-[1001]' : 'z-[1000]'}`,
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
        <select
          id="map-style"
          value={mapStyle}
          onChange={handleMapStyleChange}
          className={selectClass}
        >
          {Object.entries(MAP_STYLES).map(([id, s]) => (
            <option key={id} value={id}>
              {s.label}
            </option>
          ))}
        </select>
      </div>

      {/* Date */}
      <div className="mb-0">
        <label className="block text-[13px] text-zinc-500 dark:text-zinc-400">Date</label>
        <input
          type="date"
          id="date-picker"
          value={date}
          onChange={handleDateChange}
          className={dateClass}
        />
      </div>

      {/* Departure Window */}
      <div className="mb-0">
        <label className="block text-[13px] text-zinc-500 dark:text-zinc-400">
          Departure Window:{' '}
          <span>
            {formatTime(liveStart)} – {formatTime(liveEnd)}
          </span>
          <DualRangeSlider
            windowStart={liveStart}
            windowEnd={liveEnd}
            onChange={(s, e) => {
              setLiveStart(s);
              setLiveEnd(e);
            }}
            onCommit={(s, e) => {
              dispatch({ type: 'SET_WINDOW', windowStart: s, windowEnd: e });
              onRunQuery({ windowStart: s, windowEnd: e });
            }}
          />
        </label>
      </div>

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
      <div
        className="sm:hidden mt-3 pt-3 border-t border-zinc-700 dark:border-zinc-700
        [@media(prefers-color-scheme:light)]:border-zinc-200"
      >
        <LegendContent maxMin={state.maxTimeMin} />
      </div>
    </div>
  );
}
