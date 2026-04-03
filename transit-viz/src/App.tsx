import { useEffect, useCallback, useRef } from 'react';
import { AppProvider, useAppState } from './state/AppContext';
import CitySelect from './components/CitySelect';
import LoadingOverlay from './components/LoadingOverlay';
import Controls from './components/Controls';
import MapView from './components/MapView';
import Legend from './components/Legend';
import HoverInfo from './components/HoverInfo';
import { loadCity } from './utils/cityLoader';
import { getCityFromUrl } from './cities';
import { runQuery } from './utils/router';
import { getTravelTimeSummary, getMedianPath, formatSegments } from './utils/hoverInfo';
import type { RunQueryParams } from './utils/router';
import './styles.css';

function AppInner() {
  const { state, dispatch } = useAppState();
  const stateRef = useRef(state);
  stateRef.current = state;

  // Auto-load city from URL on mount
  useEffect(() => {
    const city = getCityFromUrl();
    if (city) {
      (async () => {
        try {
          await loadCity(city, dispatch, true);
        } catch (e) {
          dispatch({ type: 'LOAD_ERROR' });
          history.replaceState(null, '', '/');
          alert(`Failed to load ${city.name}: ${String(e)}`);
        }
      })();
    }
  }, [dispatch]);

  // Run query when source or params change
  const handleRunQuery = useCallback((overrides: Record<string, any> = {}) => {
    const s = stateRef.current;
    if (!s.router || s.sourceNode === null) return;

    const params: RunQueryParams = {
      sourceNode: s.sourceNode,
      mode: overrides.mode ?? s.mode,
      departureTime: overrides.departureTime ?? s.departureTime,
      date: overrides.date ?? s.date,
      nSamples: overrides.nSamples ?? s.nSamples,
      transferSlack: overrides.transferSlack ?? s.transferSlack,
      maxTime: (overrides.maxTimeMin ?? s.maxTimeMin) * 60,
      prevSsspList: s.ssspList || undefined,
    };

    dispatch({ type: 'COMPUTING' });
    setTimeout(() => {
      const start = performance.now();
      try {
        const result = runQuery(s.router!, params);
        dispatch({
          type: 'QUERY_DONE',
          travelTimes: result.travelTimes,
          ssspList: result.ssspList,
          timeMs: performance.now() - start,
        });
        dispatch({ type: 'UNPIN_DESTINATION' });
      } catch (e) {
        console.error(e);
        dispatch({ type: 'QUERY_ERROR' });
      }
    }, 10);
  }, [dispatch]);

  // Re-run query when source changes
  useEffect(() => {
    if (state.sourceNode !== null && state.router) {
      handleRunQuery();
    }
  }, [state.sourceNode, handleRunQuery]);

  // Copy info to clipboard
  const copyInfo = useCallback(() => {
    const s = stateRef.current;
    if (!s.router || s.sourceNode === null || !s.nodeCoords) return false;

    const srcLat = s.nodeCoords[s.sourceNode * 2].toFixed(6);
    const srcLon = s.nodeCoords[s.sourceNode * 2 + 1].toFixed(6);
    const lines = [`Source: ${srcLat}, ${srcLon}`];

    if (s.pinnedNode !== null) {
      const destLat = s.nodeCoords[s.pinnedNode * 2].toFixed(6);
      const destLon = s.nodeCoords[s.pinnedNode * 2 + 1].toFixed(6);
      lines.push(`Destination: ${destLat}, ${destLon}`);
    }

    lines.push('');
    lines.push(`Mode: ${s.mode}`);
    lines.push(`Date: ${s.date}`);
    lines.push(`Departure: ${new Date(s.departureTime * 1000).toISOString().substring(11, 16)}`);
    if (s.mode === 'sampled') lines.push(`Samples: ${s.nSamples}`);
    lines.push(`Max time: ${s.maxTimeMin} min`);
    lines.push(`Transfer slack: ${s.transferSlack}s`);

    if (s.hoverData) {
      lines.push('');
      const { allPaths, travelTimes } = s.hoverData;
      const timeSummary = getTravelTimeSummary(travelTimes, allPaths);
      if (timeSummary) {
        if (timeSummary.isSampled) {
          lines.push(`Travel time: ${timeSummary.min}–${timeSummary.avg}–${timeSummary.max} min (${timeSummary.count}/${timeSummary.total} samples)`);
        } else {
          lines.push(`Travel time: ${timeSummary.avg} min`);
        }
        // Add median path details
        const medianPath = getMedianPath(allPaths);
        if (medianPath && medianPath.segments.length > 0) {
          lines.push('Route:');
          for (const line of formatSegments(medianPath.segments)) {
            lines.push(`  ${line}`);
          }
        }
      }
    }

    navigator.clipboard.writeText(lines.join('\n'));
    dispatch({ type: 'SHOW_COPIED_MESSAGE' });
    setTimeout(() => dispatch({ type: 'HIDE_COPIED_MESSAGE' }), 1500);
    return true;
  }, [dispatch]);

  // Keyboard shortcut 'c' to copy info
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key !== 'c' || e.ctrlKey || e.metaKey || e.altKey) return;
      const target = e.target as HTMLElement;
      if (target.tagName === 'INPUT' || target.tagName === 'SELECT' || target.tagName === 'TEXTAREA') return;
      if (copyInfo()) e.preventDefault();
    }

    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [copyInfo]);

  return (
    <>
      <CitySelect />
      <LoadingOverlay />
      <MapView />
      <Controls
        onRunQuery={handleRunQuery}
        onCopy={() => {
          if (stateRef.current) copyInfo();
        }}
      />
      <Legend />
      <HoverInfo />
    </>
  );
}

export default function App() {
  return (
    <AppProvider>
      <AppInner />
    </AppProvider>
  );
}
