import React, { useState, useRef } from 'react';
import { useAppState } from '../state/AppContext';
import { formatTime, formatSlack, dateToYYYYMMDD } from '../utils/format';
import { freeSsspList } from '../utils/router';

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
  const { loadingState, mode, departureTime, date, nSamples, maxTimeMin, transferSlack, computeStatus, computeTimeMs, patternCount, nodeCount, stopCount, sourceNode, showCopiedMessage } = state;

  const [collapsed, setCollapsed] = useState(false);

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
    dispatch({ type: 'CHANGE_CITY' });
    history.replaceState(null, '', import.meta.env.BASE_URL);
  }

  return (
    <div id="controls" className={collapsed ? 'collapsed' : ''}>
      <div className="controls-toggle" onClick={() => setCollapsed(!collapsed)}>
        {collapsed ? 'Show controls' : 'Hide controls'}
      </div>
      <h3 id="city-title">{state.currentCity && state.currentCity.name}</h3>
      <div className="control-group">
        <label>Mode</label>
        <select id="mode" value={mode} onChange={handleModeChange}>
          <option value="single">Single Departure Time</option>
          <option value="sampled">Hour-Window Average</option>
        </select>
      </div>
      <div className="control-group">
        <label>Date</label>
        <input type="date" id="date-picker" value={date} onChange={handleDateChange} />
      </div>
      <div className="control-group">
        <label>
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
      {mode === 'sampled' && (
        <div className="control-group">
          <label>
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
      <div className="control-group">
        <label>
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
      <div className="control-group">
        <label>
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
      <div id="pattern-info">
        {date}: {patternCount} service pattern{patternCount !== 1 ? 's' : ''} active
      </div>
      <div id="status">{statusText}</div>
      <div style={{ display: 'flex', gap: '8px', marginTop: '8px' }}>
        <button id="change-city" onClick={handleChangeCity}>
          Change city
        </button>
        {state.pinnedNode !== null && <button id="copy-info" onClick={handleCopy}>
          Copy info
        </button>}
      </div>
    </div>
  );
}
