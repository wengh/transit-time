import { useEffect, useCallback, useRef } from 'react';
import { AppProvider, useAppState } from './state/AppContext.jsx';
import CitySelect, { getCityFromUrl } from './components/CitySelect.jsx';
import LoadingOverlay from './components/LoadingOverlay.jsx';
import Controls from './components/Controls.jsx';
import MapView from './components/MapView.jsx';
import Legend from './components/Legend.jsx';
import HoverInfo from './components/HoverInfo.jsx';
import { initWasm, loadRouter, runQuery } from './utils/router.js';
import { dateToYYYYMMDD } from './utils/format.js';
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
        dispatch({ type: 'START_LOADING', city });
        try {
          await initWasm();
          const router = await loadRouter(city.file, (pct) => {
            dispatch({ type: 'LOADING_PROGRESS', progress: pct });
          });
          dispatch({ type: 'START_INITIALIZING' });
          const nodeCoords = router.all_node_coords();
          const count = router.num_patterns_for_date(dateToYYYYMMDD(new Date().toISOString().slice(0, 10)));
          dispatch({
            type: 'CITY_LOADED',
            router, nodeCoords,
            nodeCount: router.num_nodes(),
            stopCount: router.num_stops(),
          });
          dispatch({ type: 'SET_PATTERN_COUNT', count });
        } catch (e) {
          dispatch({ type: 'LOAD_ERROR' });
          history.replaceState(null, '', '/');
          alert(`Failed to load ${city.name}: ${e.message}`);
        }
      })();
    }
  }, [dispatch]);

  // Run query when source or params change
  const handleRunQuery = useCallback((overrides = {}) => {
    const s = stateRef.current;
    if (!s.router || s.sourceNode === null) return;

    const params = {
      sourceNode: s.sourceNode,
      mode: overrides.mode ?? s.mode,
      departureTime: overrides.departureTime ?? s.departureTime,
      date: dateToYYYYMMDD(overrides.date ?? s.date),
      nSamples: overrides.nSamples ?? s.nSamples,
      transferSlack: overrides.transferSlack ?? s.transferSlack,
      maxTime: (overrides.maxTimeMin ?? s.maxTimeMin) * 60,
      prevSsspList: s.ssspList,
    };

    dispatch({ type: 'COMPUTING' });
    setTimeout(() => {
      const start = performance.now();
      try {
        const result = runQuery(s.router, params);
        dispatch({ type: 'QUERY_DONE', travelTimes: result.travelTimes, ssspList: result.ssspList, timeMs: performance.now() - start });
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
  }, [state.sourceNode]);

  // Keyboard shortcut 'c' to copy
  useEffect(() => {
    function onKeyDown(e) {
      if (e.key !== 'c' || e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.target.tagName === 'INPUT' || e.target.tagName === 'SELECT' || e.target.tagName === 'TEXTAREA') return;
      const s = stateRef.current;
      if (!s.router || s.sourceNode === null) return;

      const srcLat = s.nodeCoords[s.sourceNode * 2].toFixed(6);
      const srcLon = s.nodeCoords[s.sourceNode * 2 + 1].toFixed(6);
      const lines = [`Source: ${srcLat}, ${srcLon}`];

      if (s.pinnedNode !== null && s.ssspList?.length > 0) {
        const destLat = s.nodeCoords[s.pinnedNode * 2].toFixed(6);
        const destLon = s.nodeCoords[s.pinnedNode * 2 + 1].toFixed(6);
        lines.push(`Destination: ${destLat}, ${destLon}`);
      }

      lines.push(`Date: ${s.date}`);
      lines.push(`Departure: ${new Date(s.departureTime * 1000).toISOString().substring(11, 16)}`);
      lines.push(`Transfer slack: ${s.transferSlack}s`);

      if (s.pinnedNode !== null && s.ssspList?.length > 0) {
        const sssp = s.ssspList[0];
        if (sssp.__wbg_ptr !== 0) {
          try {
            const arrival = s.router.node_arrival_time(sssp, s.pinnedNode);
            if (arrival < 0xFFFFFFFF) {
              const dep = s.router.sssp_departure_time(sssp);
              lines.push(`Travel time: ${Math.round((arrival - dep) / 60)} min`);
            }
          } catch (e) {
            if (!e.message || !e.message.includes('null pointer')) throw e;
          }
        }
      }

      navigator.clipboard.writeText(lines.join('\n'));
    }

    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, []);

  return (
    <>
      <CitySelect />
      <LoadingOverlay />
      <MapView />
      <Controls onRunQuery={handleRunQuery} />
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
