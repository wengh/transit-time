import { useEffect, useCallback, useRef, useState } from 'react';
import { AppProvider, useAppState } from './state/AppContext';
import CitySelect from './components/CitySelect';
import LoadingOverlay from './components/LoadingOverlay';
import Controls from './components/Controls';
import MapView from './components/MapView';
import type { MapViewHandle } from './components/MapView';
import Legend from './components/Legend';
import HoverInfo from './components/HoverInfo';
import LocationSearch from './components/LocationSearch';
import MobileTopBar from './components/MobileTopBar';
import MobileBottomSheet from './components/MobileBottomSheet';
import MobileSettingsSheet from './components/MobileSettingsSheet';
import { useIsMobile } from './utils/useIsMobile';
import { loadCity } from './utils/cityLoader';
import { getCityFromUrl } from './cities';
import { runQuery, snapToNode } from './utils/router';
import { buildHoverData, getMedianPath, flattenDisplayLines } from './utils/hoverInfo';
import type { RunQueryParams } from './utils/router';
import { getHashParams, setHashParams } from './utils/urlHash';
import './styles.css';

function AppInner() {
  const { state, dispatch } = useAppState();
  const stateRef = useRef(state);
  stateRef.current = state;

  const [frontPanel, setFrontPanel] = useState<'controls' | 'hoverInfo'>('hoverInfo');
  const mapViewRef = useRef<MapViewHandle | null>(null);

  // Guard against React 18 StrictMode's intentional double-mount in dev: it
  // would otherwise spawn two parallel loadCity calls. The second one's
  // CITY_LOADED dispatch arrives *after* the first query completes and wipes
  // sourceNode/travelTimes (since the reducer resets per-city state on load),
  // leaving the user with a stale, empty isochrone and no way to hover.
  const urlRestoredRef = useRef(false);

  // Auto-load city from URL on mount, restoring state from hash. Source/dest
  // intents from the hash are queued *immediately* (before awaiting loadCity)
  // so the loading overlay shows up alongside the loading status; consumption
  // effects below drain them once the data is ready.
  useEffect(() => {
    if (urlRestoredRef.current) return;
    urlRestoredRef.current = true;
    const city = getCityFromUrl();
    if (!city) return;
    const hash = getHashParams();

    // Restore controls synchronously — these are pure state updates that
    // don't depend on loaded data, so they apply to any queued query.
    if (hash.style) dispatch({ type: 'SET_MAP_STYLE', style: hash.style });
    if (hash.date) dispatch({ type: 'SET_DATE', value: hash.date });
    if (hash.time !== undefined) {
      const dur = hash.dur ?? 3600;
      dispatch({ type: 'SET_WINDOW', windowStart: hash.time, windowEnd: hash.time + dur });
    }
    if (hash.maxtime !== undefined) dispatch({ type: 'SET_MAX_TIME', value: hash.maxtime });
    if (hash.slack !== undefined) dispatch({ type: 'SET_SLACK', value: hash.slack });

    // Queue placement intents — these flow through the same path as in-load
    // map clicks. The dest is only meaningful with a source.
    if (hash.src) {
      dispatch({ type: 'QUEUE_PENDING_SOURCE', latLng: hash.src });
      if (hash.dst) {
        dispatch({ type: 'QUEUE_PENDING_DEST', latLng: hash.dst, trip: hash.trip ?? null });
      }
    }

    (async () => {
      try {
        await loadCity(city, dispatch, true);
      } catch (e) {
        dispatch({ type: 'LOAD_ERROR' });
        history.replaceState(null, '', import.meta.env.BASE_URL);
        alert(`Failed to load ${city.name}: ${String(e)}`);
      }
    })();
  }, [dispatch]);

  // Drain pendingSource once the city data is ready. We deliberately do NOT
  // dispatch CONSUME_PENDING_SOURCE up front — clearing it now would hide the
  // overlay for the renders between SET_SOURCE and COMPUTING. Instead, the
  // COMPUTING reducer clears pendingSource when the query starts, giving a
  // continuous overlay (pending → computing). The fallback dispatch below
  // handles the rare case where setSource bails (e.g. snap returned null).
  useEffect(() => {
    if (state.loadingState !== 'ready' || !state.pendingSource) return;
    const [lat, lng] = state.pendingSource.latLng;
    (async () => {
      const ok = (await mapViewRef.current?.setSource(lat, lng)) ?? false;
      if (!ok) dispatch({ type: 'CONSUME_PENDING_SOURCE' });
    })();
  }, [state.loadingState, state.pendingSource, dispatch]);

  // Restore pinned destination (and locked trip) after the first query
  // completes. Reads the queued intent from state so URL-restore and
  // in-load click both flow through the same path.
  useEffect(() => {
    if (state.computeStatus !== 'done' || !state.pendingDest) return;
    const { nodeCoords } = state;
    if (!nodeCoords) return;
    const { latLng: latlng, trip } = state.pendingDest;
    dispatch({ type: 'CONSUME_PENDING_DEST' });
    (async () => {
      const [lat, lng] = latlng;
      const node = await snapToNode(lat, lng);
      if (node === null) return;
      const latLng: [number, number] = [nodeCoords[node * 2], nodeCoords[node * 2 + 1]];
      const s = stateRef.current;
      const hoverData = await buildHoverData(node, s.travelTimes, s.sampleCounts, s.totalSamples);
      dispatch({
        type: 'PIN_DESTINATION',
        node,
        latLng,
        hoverData,
      });
      if (trip !== null && trip < hoverData.allPaths.length) {
        dispatch({ type: 'LOCK_SAMPLE', idx: trip });
      }
    })();
  }, [state.computeStatus, state.pendingDest, dispatch]); // eslint-disable-line react-hooks/exhaustive-deps

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
        .then(async (result) => {
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

          // Refresh pinned destination data when a new query completes (e.g. parameter change).
          const currentS = stateRef.current;
          if (currentS.pinnedNode !== null) {
            const node = currentS.pinnedNode;
            const hoverData = await buildHoverData(
              node,
              result.travelTimes,
              result.sampleCounts,
              result.totalSamples
            );

            // Abort if another query started or the user unpinned/changed the node
            // while we were waiting for the worker round-trip.
            const latestS = stateRef.current;
            if (latestS.computeStatus !== 'done' || latestS.pinnedNode !== node) return;

            dispatch({
              type: 'SET_HOVER_DATA',
              hoverData,
            });
            // Clear any locked sample since the array of paths has likely changed
            dispatch({ type: 'LOCK_SAMPLE', idx: null });
          }
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

  const isMobile = useIsMobile();
  const [settingsOpen, setSettingsOpen] = useState(false);
  const handleCopy = () => {
    if (stateRef.current) copyInfo();
  };

  return (
    <>
      <CitySelect />
      <LoadingOverlay />
      <MapView ref={mapViewRef} />
      {isMobile ? (
        <>
          <MobileTopBar onOpenSettings={() => setSettingsOpen(true)} mapViewRef={mapViewRef} />
          <MobileBottomSheet />
          {settingsOpen && (
            <MobileSettingsSheet
              onClose={() => setSettingsOpen(false)}
              onRunQuery={handleRunQuery}
              onCopy={handleCopy}
            />
          )}
        </>
      ) : (
        <>
          <div className="absolute z-[1000] top-2.5 left-2.5 flex flex-col gap-2">
            <LocationSearch mapViewRef={mapViewRef} variant="desktop" />
            <div className="flex flex-col rounded-lg overflow-hidden shadow-[0_2px_8px_rgba(0,0,0,0.35)] border border-zinc-200 dark:border-zinc-700 w-fit">
              {[
                { label: '+', title: 'Zoom in', action: () => mapViewRef.current?.zoomIn() },
                { label: '−', title: 'Zoom out', action: () => mapViewRef.current?.zoomOut() },
              ].map(({ label, title, action }, i) => (
                <button
                  key={label}
                  title={title}
                  aria-label={title}
                  onClick={action}
                  className={[
                    'w-8 h-8 flex items-center justify-center',
                    'bg-white/95 dark:bg-zinc-900/95',
                    'text-zinc-700 dark:text-zinc-200 text-lg font-light leading-none',
                    'hover:bg-zinc-100 dark:hover:bg-zinc-800',
                    'active:bg-zinc-200 dark:active:bg-zinc-700',
                    i === 0 ? 'border-b border-zinc-200 dark:border-zinc-700' : '',
                  ].join(' ')}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>
          <Controls
            onRunQuery={handleRunQuery}
            onCopy={handleCopy}
            isFront={frontPanel === 'controls'}
            onActivate={() => setFrontPanel('controls')}
          />
          <Legend />
          <HoverInfo
            isFront={frontPanel === 'hoverInfo'}
            onActivate={() => setFrontPanel('hoverInfo')}
          />
        </>
      )}
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
