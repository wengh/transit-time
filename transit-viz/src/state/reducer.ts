import type { HoverPath } from '../utils/router';
import type { City } from '../cities';
import { DEFAULT_MAP_STYLE } from '../utils/mapStyles';

export interface AppState {
  // City loading
  currentCity: City | null;
  loadingState: 'idle' | 'loading' | 'initializing' | 'ready';
  loadingProgress: number;

  // Controls
  mapStyle: string;
  windowStart: number;
  windowEnd: number;
  date: string;
  maxTimeMin: number;
  transferSlack: number;

  // Router state (WASM lives in worker; main thread caches coords + colors)
  nodeCoords: Float32Array | null;
  routeColors: string[];
  sourceNode: number | null;
  sourceLatLng: [number, number] | null;

  // Query results
  travelTimes: Float32Array | null;
  sampleCounts: Uint32Array | null;
  totalSamples: number;
  computeStatus: 'idle' | 'computing' | 'done' | 'error';
  computeProgress: { done: number; total: number } | null;
  computeTimeMs: number;
  computeNumThreads: number;
  patternCount: number;
  nodeCount: number;
  stopCount: number;

  // Destination — split into two slots so hover writes can't race with pin
  // writes. The displayed destination is `pinnedDest ?? hoverDest` (see
  // `currentDest`). `pinnedDest.hoverData` may briefly be `null` between a
  // SET_SOURCE { keepDest: true } and the post-requery patch from App.tsx.
  pinnedDest: Destination | null;
  hoverDest: Destination | null;
  // Which Pareto path the user is inspecting in the chart. `selected` is
  // ephemeral (follows the cursor); `locked` pins it across cursor moves and
  // survives unpin/repin. Both are indices into the displayed destination's
  // `hoverData.allPaths` or null.
  selectedSampleIdx: number | null;
  lockedSampleIdx: number | null;

  // UI feedback
  showCopiedMessage: boolean;

  // Mobile interaction mode: 'origin' = next map tap sets the source,
  // 'dest' = next map tap pins (or repins) the destination. Auto-switches
  // to 'dest' after the source is set; sticky thereafter.
  interactionMode: 'origin' | 'dest';

  // Placement intents captured while the city data is still loading. Drained
  // by App.tsx once loadingState becomes 'ready' (and, for pendingDest, after
  // the first query completes). Each click while loading replaces the prior
  // pending intent so "last click wins".
  pendingSource: { latLng: [number, number] } | null;
  pendingDest: { latLng: [number, number]; trip: number | null } | null;
}

export interface Destination {
  node: number;
  latLng: [number, number];
  // Null only while a pinned destination is awaiting fresh hoverData after a
  // source/parameter change. Hover destinations always carry hoverData.
  hoverData: HoverData | null;
}

export function currentDest(state: AppState): Destination | null {
  return state.pinnedDest ?? state.hoverDest;
}

export interface HoverData {
  allPaths: HoverPath[];
  representativeIndex: number | null;
  travelTimes: number[];
  // Per-node analytic summary from the Rust profile router. Populated from
  // `state.travelTimes[node]` and `state.sampleCounts[node] / state.totalSamples`.
  // `avgTravelTime` is null when the node is unreachable.
  avgTravelTime: number | null;
  reachableFraction: number | null;
}

export type Action =
  | { type: 'START_LOADING'; city: City }
  | { type: 'LOADING_PROGRESS'; progress: number }
  | { type: 'START_INITIALIZING' }
  | {
      type: 'CITY_LOADED';
      nodeCoords: Float32Array;
      nodeCount: number;
      stopCount: number;
      routeColors: string[];
    }
  | { type: 'LOAD_ERROR' }
  | { type: 'CHANGE_CITY' }
  | { type: 'SET_SOURCE'; node: number; latLng: [number, number]; keepDest?: boolean }
  | { type: 'SET_MAP_STYLE'; style: string }
  | { type: 'SET_WINDOW'; windowStart: number; windowEnd: number }
  | { type: 'SET_DATE'; value: string }
  | { type: 'SET_MAX_TIME'; value: number }
  | { type: 'SET_SLACK'; value: number }
  | { type: 'SET_PATTERN_COUNT'; count: number }
  | { type: 'COMPUTING' }
  | { type: 'COMPUTE_PROGRESS'; done: number; total: number }
  | {
      type: 'QUERY_DONE';
      travelTimes: Float32Array;
      sampleCounts: Uint32Array;
      totalSamples: number;
      timeMs: number;
      numThreads: number;
    }
  | { type: 'QUERY_ERROR' }
  | { type: 'PIN_DESTINATION'; dest: Destination }
  | { type: 'UNPIN_DESTINATION' }
  // Patches pinnedDest.hoverData in place (e.g., after a parameter-only
  // requery rebuilds the route to the same pinned node).
  | { type: 'SET_PINNED_HOVER_DATA'; hoverData: HoverData }
  | { type: 'SET_HOVER_DEST'; dest: Destination }
  | { type: 'CLEAR_HOVER' }
  | { type: 'SELECT_SAMPLE'; idx: number | null }
  | { type: 'LOCK_SAMPLE'; idx: number | null }
  | { type: 'SHOW_COPIED_MESSAGE' }
  | { type: 'HIDE_COPIED_MESSAGE' }
  | { type: 'SET_INTERACTION_MODE'; mode: 'origin' | 'dest' }
  | { type: 'QUEUE_PENDING_SOURCE'; latLng: [number, number] }
  | { type: 'QUEUE_PENDING_DEST'; latLng: [number, number]; trip: number | null }
  | { type: 'CONSUME_PENDING_SOURCE' }
  | { type: 'CONSUME_PENDING_DEST' };

export const initialState: AppState = {
  // City loading
  currentCity: null,
  loadingState: 'idle',
  loadingProgress: 0,

  // Controls
  mapStyle: DEFAULT_MAP_STYLE,
  windowStart: 6 * 3600, // 06:00
  windowEnd: 24 * 3600, // 24:00
  date: new Date().toISOString().slice(0, 10),
  maxTimeMin: 45,
  transferSlack: 60,

  // Router state
  nodeCoords: null,
  routeColors: [],
  sourceNode: null,
  sourceLatLng: null,

  // Query results
  travelTimes: null,
  sampleCounts: null,
  totalSamples: 1,
  computeStatus: 'idle',
  computeProgress: null,
  computeTimeMs: 0,
  computeNumThreads: 1,
  patternCount: 0,
  nodeCount: 0,
  stopCount: 0,

  // Destination
  pinnedDest: null,
  hoverDest: null,
  selectedSampleIdx: null,
  lockedSampleIdx: null,

  // UI feedback
  showCopiedMessage: false,

  // Mobile interaction mode (no-op on desktop)
  interactionMode: 'origin',

  // Pending placements (no intent queued by default)
  pendingSource: null,
  pendingDest: null,
};

export function reducer(state: AppState, action: Action): AppState {
  switch (action.type) {
    case 'START_LOADING':
      return { ...state, loadingState: 'loading', loadingProgress: 0, currentCity: action.city };
    case 'LOADING_PROGRESS':
      return { ...state, loadingProgress: action.progress };
    case 'START_INITIALIZING':
      return { ...state, loadingState: 'initializing' };
    case 'CITY_LOADED':
      return {
        ...state,
        loadingState: 'ready',
        nodeCoords: action.nodeCoords,
        routeColors: action.routeColors,
        nodeCount: action.nodeCount,
        stopCount: action.stopCount,
        sourceNode: null,
        sourceLatLng: null,
        travelTimes: null,
        pinnedDest: null,
        hoverDest: null,
        computeStatus: 'idle',
        computeProgress: null,
      };
    case 'LOAD_ERROR':
      return {
        ...state,
        loadingState: 'idle',
        currentCity: null,
        pendingSource: null,
        pendingDest: null,
      };
    case 'CHANGE_CITY':
      return {
        ...state,
        loadingState: 'idle',
        currentCity: null,
        nodeCoords: null,
        routeColors: [],
        travelTimes: null,
        sourceNode: null,
        sourceLatLng: null,
        pinnedDest: null,
        hoverDest: null,
        pendingSource: null,
        pendingDest: null,
      };
    case 'SET_SOURCE': {
      // keepDest=true preserves the pinned destination across a source change
      // (used when the source is set via the search bar). The pinned dest's
      // hoverData is nulled because routing is about to re-run; App.tsx
      // patches it via SET_PINNED_HOVER_DATA when the new query completes.
      const keepDest = action.keepDest === true;
      return {
        ...state,
        sourceNode: action.node,
        sourceLatLng: action.latLng,
        travelTimes: null,
        sampleCounts: null,
        pinnedDest: keepDest && state.pinnedDest ? { ...state.pinnedDest, hoverData: null } : null,
        hoverDest: null,
        selectedSampleIdx: null,
        lockedSampleIdx: null,
        // Auto-switch to dest mode so the next map tap pins a destination.
        interactionMode: 'dest',
      };
    }
    case 'SET_MAP_STYLE':
      return { ...state, mapStyle: action.style };
    case 'SET_WINDOW':
      return { ...state, windowStart: action.windowStart, windowEnd: action.windowEnd };
    case 'SET_DATE':
      return { ...state, date: action.value };
    case 'SET_MAX_TIME':
      return { ...state, maxTimeMin: action.value };
    case 'SET_SLACK':
      return { ...state, transferSlack: action.value };
    case 'SET_PATTERN_COUNT':
      return { ...state, patternCount: action.count };
    case 'COMPUTING':
      // Clear pendingSource here (rather than synchronously in the consumption
      // effect) so the overlay's "pendingSource set" branch hands off
      // continuously to the "computing" branch — without this, the overlay
      // flickers off for one render between SET_SOURCE and COMPUTING.
      return {
        ...state,
        computeStatus: 'computing',
        computeProgress: null,
        pendingSource: null,
      };
    case 'COMPUTE_PROGRESS':
      return { ...state, computeProgress: { done: action.done, total: action.total } };
    case 'QUERY_DONE':
      return {
        ...state,
        travelTimes: action.travelTimes,
        sampleCounts: action.sampleCounts,
        totalSamples: action.totalSamples,
        computeStatus: 'done',
        computeTimeMs: action.timeMs,
        computeNumThreads: action.numThreads,
        computeProgress: null,
      };
    case 'QUERY_ERROR':
      return { ...state, computeStatus: 'error', computeProgress: null };
    case 'PIN_DESTINATION':
      return {
        ...state,
        pinnedDest: action.dest,
        hoverDest: null,
        // Pinning a new destination should show its median trip from the
        // chart, not whichever Pareto sample the user had locked from the
        // previous destination (which would be wrong data anyway since
        // the new dest's allPaths come from a different node).
        selectedSampleIdx: null,
        lockedSampleIdx: null,
      };
    case 'UNPIN_DESTINATION':
      return {
        ...state,
        pinnedDest: null,
        hoverDest: null,
        selectedSampleIdx: null,
        lockedSampleIdx: null,
      };
    case 'SET_PINNED_HOVER_DATA':
      if (state.pinnedDest === null) return state;
      return {
        ...state,
        pinnedDest: { ...state.pinnedDest, hoverData: action.hoverData },
      };
    case 'SET_HOVER_DEST':
      return { ...state, hoverDest: action.dest };
    case 'CLEAR_HOVER':
      if (state.hoverDest === null) return state;
      return {
        ...state,
        hoverDest: null,
        // Selected sample is only meaningful while the chart is showing the
        // hover preview; clear it when the preview goes away. Locked sample
        // belongs to the pin (if any) and stays.
        selectedSampleIdx: state.pinnedDest ? state.selectedSampleIdx : null,
      };
    case 'SELECT_SAMPLE':
      return { ...state, selectedSampleIdx: action.idx };
    case 'LOCK_SAMPLE':
      return { ...state, lockedSampleIdx: action.idx, selectedSampleIdx: action.idx };
    case 'SHOW_COPIED_MESSAGE':
      return { ...state, showCopiedMessage: true };
    case 'HIDE_COPIED_MESSAGE':
      return { ...state, showCopiedMessage: false };
    case 'SET_INTERACTION_MODE':
      return { ...state, interactionMode: action.mode };
    case 'QUEUE_PENDING_SOURCE':
      return { ...state, pendingSource: { latLng: action.latLng } };
    case 'QUEUE_PENDING_DEST':
      return { ...state, pendingDest: { latLng: action.latLng, trip: action.trip } };
    case 'CONSUME_PENDING_SOURCE':
      return { ...state, pendingSource: null };
    case 'CONSUME_PENDING_DEST':
      return { ...state, pendingDest: null };
  }
}
