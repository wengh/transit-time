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
import { runQuery, getAnyHoverData } from './utils/router';
import { getMedianPath, flattenDisplayLines, getSortedTravelTimes } from './utils/hoverInfo';
import type { RunQueryParams } from './utils/router';
import { getHashParams, setHashParams } from './utils/urlHash';
import './styles.css';

function AppInner() {
  const { state, dispatch } = useAppState();
  const stateRef = useRef(state);
  stateRef.current = state;

  const pendingDestRef = useRef<{ latlng: [number, number]; trip: number | null } | null>(null);

  // Auto-load city from URL on mount, restoring state from hash
  useEffect(() => {
    const city = getCityFromUrl();
    if (city) {
      (async () => {
        const hash = getHashParams();
        try {
          const { router, nodeCoords } = await loadCity(city, dispatch, true);
          // Restore controls
          if (hash.style) dispatch({ type: 'SET_MAP_STYLE', style: hash.style });
          if (hash.mode) dispatch({ type: 'SET_MODE', mode: hash.mode });
          if (hash.date) dispatch({ type: 'SET_DATE', value: hash.date });
          if (hash.time !== undefined) dispatch({ type: 'SET_DEPARTURE_TIME', value: hash.time });
          if (hash.samples !== undefined) dispatch({ type: 'SET_SAMPLES', value: hash.samples });
          if (hash.maxtime !== undefined) dispatch({ type: 'SET_MAX_TIME', value: hash.maxtime });
          if (hash.slack !== undefined) dispatch({ type: 'SET_SLACK', value: hash.slack });
          // Restore source (triggers query)
          if (hash.src) {
            const [lat, lng] = hash.src;
            const node = router.snap_to_node(lat, lng);
            if (node !== null) {
              const latLng: [number, number] = [nodeCoords[node * 2], nodeCoords[node * 2 + 1]];
              dispatch({ type: 'SET_SOURCE', node, latLng });
              if (hash.dst) pendingDestRef.current = { latlng: hash.dst, trip: hash.trip ?? null };
            }
          }
        } catch (e) {
          dispatch({ type: 'LOAD_ERROR' });
          history.replaceState(null, '', import.meta.env.BASE_URL);
          alert(`Failed to load ${city.name}: ${String(e)}`);
        }
      })();
    }
  }, [dispatch]);

  // Restore pinned destination (and locked trip) after query completes
  useEffect(() => {
    if (state.computeStatus !== 'done' || !pendingDestRef.current) return;
    const { router, ssspList, profile, nodeCoords } = state;
    if (!router || (!ssspList && !profile) || !nodeCoords) return;
    const { latlng, trip } = pendingDestRef.current;
    pendingDestRef.current = null;
    const [lat, lng] = latlng;
    const node = router.snap_to_node(lat, lng);
    if (node === null) return;
    const latLng: [number, number] = [nodeCoords[node * 2], nodeCoords[node * 2 + 1]];
    const allPaths = getAnyHoverData(router, ssspList, profile, node);
    const travelTimes = getSortedTravelTimes(allPaths);
    // Mirror MapView.showDestination: pull the analytic summary out of the
    // Rust-side per-node arrays rather than re-aggregating from `allPaths`.
    const tt = state.travelTimes ? state.travelTimes[node] : NaN;
    const avgTravelTime = isFinite(tt) ? tt : null;
    const reachableFraction = state.sampleCounts && state.totalSamples > 0
      ? state.sampleCounts[node] / state.totalSamples
      : null;
    dispatch({
      type: 'PIN_DESTINATION',
      node,
      latLng,
      hoverData: { allPaths, travelTimes, avgTravelTime, reachableFraction },
    });
    if (trip !== null && trip < allPaths.length) {
      dispatch({ type: 'LOCK_SAMPLE', idx: trip });
    }
  }, [state.computeStatus, dispatch]); // eslint-disable-line react-hooks/exhaustive-deps

  // Sync state to URL hash (only when source is selected)
  useEffect(() => {
    if (!state.sourceLatLng) return;
    const current = getHashParams();
    setHashParams({
      src: state.sourceLatLng,
      dst: state.pinnedLatLng ?? undefined,
      trip: state.lockedSampleIdx ?? undefined,
      style: state.mapStyle,
      mode: state.mode,
      date: state.date,
      time: state.departureTime,
      samples: state.nSamples,
      maxtime: state.maxTimeMin,
      slack: state.transferSlack,
      zoom: current.zoom,
      center: current.center,
    });
  }, [state.sourceLatLng, state.pinnedLatLng, state.lockedSampleIdx, state.mapStyle, state.mode, state.date, state.departureTime, state.nSamples, state.maxTimeMin, state.transferSlack]);

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
      prevProfile: s.profile || undefined,
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
          profile: result.profile,
          sampleCounts: result.sampleCounts,
          totalSamples: result.totalSamples,
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
      const { allPaths, avgTravelTime, reachableFraction } = s.hoverData;
      if (avgTravelTime !== null) {
        const avgMin = Math.round(avgTravelTime / 60);
        if (reachableFraction !== null) {
          const pct = Math.round(reachableFraction * 100);
          lines.push(`Avg travel time: ${avgMin} min (${pct}% reachable)`);
        } else {
          lines.push(`Travel time: ${avgMin} min`);
        }
      }
      // Add median path details
      const medianPath = getMedianPath(allPaths);
      if (medianPath && medianPath.segments.length > 0) {
        lines.push('Route:');
        for (const line of flattenDisplayLines(medianPath)) {
          lines.push(`  ${line}`);
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
