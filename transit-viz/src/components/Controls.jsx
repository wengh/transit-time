import { useState } from 'react';
import { useAppState } from '../state/AppContext.jsx';
import { formatTime, formatSlack, dateToYYYYMMDD } from '../utils/format.js';

export default function Controls({ onRunQuery }) {
  const { state, dispatch } = useAppState();
  const { loadingState, mode, departureTime, date, nSamples, maxTimeMin, transferSlack,
          computeStatus, computeTimeMs, patternCount, nodeCount, stopCount, sourceNode } = state;

  // Local display values for live slider feedback
  const [timeDisplay, setTimeDisplay] = useState(formatTime(departureTime));
  const [samplesDisplay, setSamplesDisplay] = useState(nSamples);
  const [maxTimeDisplay, setMaxTimeDisplay] = useState(`${maxTimeMin} min`);
  const [slackDisplay, setSlackDisplay] = useState(formatSlack(transferSlack));
  const [collapsed, setCollapsed] = useState(false);

  if (loadingState !== 'ready') return null;

  const statusText = computeStatus === 'computing' ? 'Computing...'
    : computeStatus === 'done' ? `Done. Spent ${Math.round(computeTimeMs)} ms.`
    : computeStatus === 'error' ? 'Error'
    : sourceNode !== null
      ? `${nodeCount.toLocaleString()} nodes, ${stopCount.toLocaleString()} stops.`
      : `${nodeCount.toLocaleString()} nodes, ${stopCount.toLocaleString()} stops. Double-click map to set origin.`;

  function handleModeChange(e) {
    dispatch({ type: 'SET_MODE', mode: e.target.value });
    onRunQuery({ mode: e.target.value });
  }

  function handleDateChange(e) {
    dispatch({ type: 'SET_DATE', value: e.target.value });
    if (state.router) {
      const count = state.router.num_patterns_for_date(dateToYYYYMMDD(e.target.value));
      dispatch({ type: 'SET_PATTERN_COUNT', count });
    }
    onRunQuery({ date: e.target.value });
  }

  function handleTimeChange(e) {
    const val = parseInt(e.target.value);
    dispatch({ type: 'SET_DEPARTURE_TIME', value: val });
    onRunQuery({ departureTime: val });
  }

  function handleSamplesChange(e) {
    const val = parseInt(e.target.value);
    dispatch({ type: 'SET_SAMPLES', value: val });
    onRunQuery({ nSamples: val });
  }

  function handleMaxTimeChange(e) {
    const val = parseInt(e.target.value);
    dispatch({ type: 'SET_MAX_TIME', value: val });
    onRunQuery({ maxTimeMin: val });
  }

  function handleSlackChange(e) {
    const val = parseInt(e.target.value);
    dispatch({ type: 'SET_SLACK', value: val });
    onRunQuery({ transferSlack: val });
  }

  function handleChangeCity() {
    dispatch({ type: 'CHANGE_CITY' });
    history.replaceState(null, '', '/');
  }

  return (
    <div id="controls" className={collapsed ? 'collapsed' : ''}>
      <div className="controls-toggle" onClick={() => setCollapsed(!collapsed)}>
        {collapsed ? 'Show controls' : 'Hide controls'}
      </div>
      <h3 id="city-title">{state.currentCity?.name}</h3>
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
        <label>Departure Time: <span>{timeDisplay}</span></label>
        <input type="range" id="time-slider" min="0" max="86400" value={departureTime} step="300"
          onInput={e => setTimeDisplay(formatTime(parseInt(e.target.value)))}
          onChange={handleTimeChange} />
      </div>
      {mode === 'sampled' && (
        <div className="control-group">
          <label>Samples: <span>{samplesDisplay}</span></label>
          <input type="range" id="samples-slider" min="3" max="30" value={nSamples}
            onInput={e => setSamplesDisplay(e.target.value)}
            onChange={handleSamplesChange} />
        </div>
      )}
      <div className="control-group">
        <label>Max travel time: <span>{maxTimeDisplay}</span></label>
        <input type="range" id="maxtime-slider" min="10" max="180" value={maxTimeMin} step="5"
          onInput={e => setMaxTimeDisplay(`${e.target.value} min`)}
          onChange={handleMaxTimeChange} />
      </div>
      <div className="control-group">
        <label>Transfer slack: <span>{slackDisplay}</span></label>
        <input type="range" id="slack-slider" min="0" max="300" value={transferSlack} step="15"
          onInput={e => setSlackDisplay(formatSlack(parseInt(e.target.value)))}
          onChange={handleSlackChange} />
      </div>
      <div id="pattern-info">
        {date}: {patternCount} service pattern{patternCount !== 1 ? 's' : ''} active
      </div>
      <div id="status">{statusText}</div>
      <button id="change-city" onClick={handleChangeCity}>Change city</button>
    </div>
  );
}
