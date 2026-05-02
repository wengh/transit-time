import { useEffect, useCallback, useRef, useState } from 'react';
import { AppProvider, useAppState } from './state/AppContext';
import CitySelect from './components/CitySelect';
import LoadingOverlay from './components/LoadingOverlay';
import Controls from './components/Controls';
import MapView from './components/MapView';
import Legend from './components/Legend';
import HoverInfo from './components/HoverInfo';
import { loadCity } from './utils/cityLoader';
import { getCityFromUrl } from './cities';
import { runQuery, getProfileHoverData, snapToNode } from './utils/router';
import { getMedianPath, flattenDisplayLines, getSortedTravelTimes } from './utils/hoverInfo';
import type { RunQueryParams } from './utils/router';
import { getHashParams, setHashParams } from './utils/urlHash';
import './styles.css';

function AppInner() {
  const { state, dispatch } = useAppState();
  const stateRef = useRef(state);
  stateRef.current = state;

  const [frontPanel, setFrontPanel] = useState<'controls' | 'hoverInfo'>('hoverInfo');

  const pendingDestRef = useRef<{ latlng: [number, number]; trip: number | null } | null>(null);

  // Auto-load city from URL on mount, restoring state from hash
  useEffect(() => {
    const city = getCityFromUrl();
    if (city) {
      (async () => {
        const hash = getHashParams();
        try {
          const { nodeCoords } = await loadCity(city, dispatch, true);
          // Restore controls
          if (hash.style) dispatch({ type: 'SET_MAP_STYLE', style: hash.style });
          if (hash.date) dispatch({ type: 'SET_DATE', value: hash.date });
          if (hash.time !== undefined) {
            const dur = hash.dur ?? 3600;
            dispatch({ type: 'SET_WINDOW', windowStart: hash.time, windowEnd: hash.time + dur });
          }
          if (hash.maxtime !== undefined) dispatch({ type: 'SET_MAX_TIME', value: hash.maxtime });
          if (hash.slack !== undefined) dispatch({ type: 'SET_SLACK', value: hash.slack });
          // Restore source (triggers query)
          if (hash.src) {
            const [lat, lng] = hash.src;
            const node = await snapToNode(lat, lng);
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
    const { nodeCoords } = state;
    if (!nodeCoords) return;
    const { latlng, trip } = pendingDestRef.current;
    pendingDestRef.current = null;
    (async () => {
      const [lat, lng] = latlng;
      const node = await snapToNode(lat, lng);
      if (node === null) return;
      const latLng: [number, number] = [nodeCoords[node * 2], nodeCoords[node * 2 + 1]];
      const allPaths = await getProfileHoverData(node);
      const travelTimes = getSortedTravelTimes(allPaths);
      // Mirror MapView.showDestination: pull the analytic summary out of the
      // Rust-side per-node arrays rather than re-aggregating from `allPaths`.
      const s = stateRef.current;
      const tt = s.travelTimes ? s.travelTimes[node] : NaN;
      const avgTravelTime = isFinite(tt) ? tt : null;
      const reachableFraction =
        s.sampleCounts && s.totalSamples > 0 ? s.sampleCounts[node] / s.totalSamples : null;
      dispatch({
        type: 'PIN_DESTINATION',
        node,
        latLng,
        hoverData: { allPaths, travelTimes, avgTravelTime, reachableFraction },
      });
      if (trip !== null && trip < allPaths.length) {
        dispatch({ type: 'LOCK_SAMPLE', idx: trip });
      }
    })();
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
      date: state.date,
      time: state.windowStart,
      dur: state.windowEnd - state.windowStart,
      maxtime: state.maxTimeMin,
      slack: state.transferSlack,
      zoom: current.zoom,
      center: current.center,
    });
  }, [
    state.sourceLatLng,
    state.pinnedLatLng,
    state.lockedSampleIdx,
    state.mapStyle,
    state.date,
    state.windowStart,
    state.windowEnd,
    state.maxTimeMin,
    state.transferSlack,
  ]);

  // Run query when source or params change
  const handleRunQuery = useCallback(
    (overrides: Record<string, any> = {}) => {
      const s = stateRef.current;
      if (s.loadingState !== 'ready' || s.sourceNode === null) return;

      const params: RunQueryParams = {
        sourceNode: s.sourceNode,
        windowStart: overrides.windowStart ?? s.windowStart,
        windowEnd: overrides.windowEnd ?? s.windowEnd,
        date: overrides.date ?? s.date,
        transferSlack: overrides.transferSlack ?? s.transferSlack,
        maxTime: (overrides.maxTimeMin ?? s.maxTimeMin) * 60,
      };

      dispatch({ type: 'COMPUTING' });
      const start = performance.now();
      runQuery(params, (done, total) => {
        dispatch({ type: 'COMPUTE_PROGRESS', done, total });
      })
        .then((result) => {
          dispatch({
            type: 'QUERY_DONE',
            travelTimes: result.travelTimes,
            sampleCounts: result.sampleCounts,
            totalSamples: result.totalSamples,
            timeMs: performance.now() - start,
            numThreads: result.numThreads,
          });
          // Don't unpin here — parameter-only changes should keep the destination
          // pin and sample selection. Pin teardown happens in `SET_SOURCE`.
        })
        .catch((e) => {
          if (String(e).includes('cancelled')) return; // query was superseded
          console.error(e);
          dispatch({ type: 'QUERY_ERROR' });
        });
    },
    [dispatch]
  );

  // Re-run query when source changes
  useEffect(() => {
    if (state.sourceNode !== null && state.loadingState === 'ready') {
      handleRunQuery();
    }
  }, [state.sourceNode, handleRunQuery]);

  // Copy info to clipboard
  const copyInfo = useCallback(() => {
    const s = stateRef.current;
    if (s.sourceNode === null || !s.nodeCoords) return false;

    const srcLat = s.nodeCoords[s.sourceNode * 2].toFixed(6);
    const srcLon = s.nodeCoords[s.sourceNode * 2 + 1].toFixed(6);
    const lines = [`Source: ${srcLat}, ${srcLon}`];

    if (s.pinnedNode !== null) {
      const destLat = s.nodeCoords[s.pinnedNode * 2].toFixed(6);
      const destLon = s.nodeCoords[s.pinnedNode * 2 + 1].toFixed(6);
      lines.push(`Destination: ${destLat}, ${destLon}`);
    }

    lines.push('');
    lines.push(`Date: ${s.date}`);
    const fmtT = (sec: number) => {
      const h = Math.floor(sec / 3600);
      const m = Math.floor((sec % 3600) / 60);
      return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
    };
    lines.push(`Departure window: ${fmtT(s.windowStart)} – ${fmtT(s.windowEnd)}`);
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
      if (
        target.tagName === 'INPUT' ||
        target.tagName === 'SELECT' ||
        target.tagName === 'TEXTAREA'
      )
        return;
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
        isFront={frontPanel === 'controls'}
        onActivate={() => setFrontPanel('controls')}
      />
      <Legend />
      <HoverInfo
        isFront={frontPanel === 'hoverInfo'}
        onActivate={() => setFrontPanel('hoverInfo')}
      />
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
