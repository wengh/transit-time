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

  // Destination
  pinnedNode: number | null;
  pinnedLatLng: [number, number] | null;
  hoverData: HoverData | null;
  // Which Pareto path the user is inspecting in the chart. `selected` is
  // ephemeral (follows the cursor); `locked` pins it across cursor moves and
  // survives unpin/repin. Both are indices into `hoverData.allPaths` or null.
  selectedSampleIdx: number | null;
  lockedSampleIdx: number | null;

  // UI feedback
  showCopiedMessage: boolean;

  // Mobile interaction mode: 'origin' = next map tap sets the source,
  // 'dest' = next map tap pins (or repins) the destination. Auto-switches
  // to 'dest' after the source is set; sticky thereafter.
  interactionMode: 'origin' | 'dest';
}

export interface HoverData {
  allPaths: HoverPath[];
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
  | { type: 'SET_SOURCE'; node: number; latLng: [number, number] }
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
  | { type: 'PIN_DESTINATION'; node: number; latLng: [number, number]; hoverData: HoverData }
  | { type: 'UNPIN_DESTINATION' }
  | { type: 'SET_HOVER_DATA'; hoverData: HoverData }
  | { type: 'CLEAR_HOVER' }
  | { type: 'SELECT_SAMPLE'; idx: number | null }
  | { type: 'LOCK_SAMPLE'; idx: number | null }
  | { type: 'SHOW_COPIED_MESSAGE' }
  | { type: 'HIDE_COPIED_MESSAGE' }
  | { type: 'SET_INTERACTION_MODE'; mode: 'origin' | 'dest' };

export const initialState: AppState = {
  // City loading
  currentCity: null,
  loadingState: 'idle',
  loadingProgress: 0,

  // Controls
  mapStyle: DEFAULT_MAP_STYLE,
  windowStart: 0, // 00:00
  windowEnd: 27 * 3600, // 27:00 (03:00 of the next day to account for late night trips)
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
  pinnedNode: null,
  pinnedLatLng: null,
  hoverData: null,
  selectedSampleIdx: null,
  lockedSampleIdx: null,

  // UI feedback
  showCopiedMessage: false,

  // Mobile interaction mode (no-op on desktop)
  interactionMode: 'origin',
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
        pinnedNode: null,
        pinnedLatLng: null,
        hoverData: null,
        computeStatus: 'idle',
        computeProgress: null,
      };
    case 'LOAD_ERROR':
      return { ...state, loadingState: 'idle', currentCity: null };
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
        pinnedNode: null,
        pinnedLatLng: null,
        hoverData: null,
      };
    case 'SET_SOURCE':
      return {
        ...state,
        sourceNode: action.node,
        sourceLatLng: action.latLng,
        travelTimes: null,
        sampleCounts: null,
        pinnedNode: null,
        pinnedLatLng: null,
        hoverData: null,
        selectedSampleIdx: null,
        lockedSampleIdx: null,
        // Auto-switch to dest mode so the next map tap pins a destination.
        interactionMode: 'dest',
      };
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
      return { ...state, computeStatus: 'computing', computeProgress: null };
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
        pinnedNode: action.node,
        pinnedLatLng: action.latLng,
        hoverData: action.hoverData,
        // Pinning a new destination should show its median trip from the
        // chart, not whichever Pareto sample the user had locked from the
        // previous destination (which would be wrong data anyway since
        // hoverData.allPaths comes from the new node).
        selectedSampleIdx: null,
        lockedSampleIdx: null,
      };
    case 'UNPIN_DESTINATION':
      return {
        ...state,
        pinnedNode: null,
        pinnedLatLng: null,
        hoverData: null,
        selectedSampleIdx: null,
        lockedSampleIdx: null,
      };
    case 'SET_HOVER_DATA':
      return { ...state, hoverData: action.hoverData };
    case 'CLEAR_HOVER':
      if (state.pinnedNode !== null) return state;
      return { ...state, hoverData: null, selectedSampleIdx: null };
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
  }
}
