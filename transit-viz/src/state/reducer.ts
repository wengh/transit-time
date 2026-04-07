import type { Router, SsspList, HoverPath } from '../utils/router';
import type { City } from '../cities';

export interface AppState {
  // City loading
  currentCity: City | null;
  loadingState: 'idle' | 'loading' | 'initializing' | 'ready';
  loadingProgress: number;

  // Controls
  mode: 'single' | 'sampled';
  departureTime: number;
  date: string;
  nSamples: number;
  maxTimeMin: number;
  transferSlack: number;

  // Router state
  router: Router | null;
  nodeCoords: Float32Array | null;
  sourceNode: number | null;
  sourceLatLng: [number, number] | null;

  // Query results
  travelTimes: Float32Array | null;
  ssspList: SsspList | null;
  sampleCounts: Uint32Array | null;
  totalSamples: number;
  computeStatus: 'idle' | 'computing' | 'done' | 'error';
  computeTimeMs: number;
  patternCount: number;
  nodeCount: number;
  stopCount: number;

  // Destination
  pinnedNode: number | null;
  pinnedLatLng: [number, number] | null;
  hoverData: HoverData | null;
  selectedSampleIdx: number | null;
  lockedSampleIdx: number | null;

  // UI feedback
  showCopiedMessage: boolean;
}

export interface HoverData {
  allPaths: HoverPath[];
  travelTimes: number[];
}

export type Action =
  | { type: 'START_LOADING'; city: City }
  | { type: 'LOADING_PROGRESS'; progress: number }
  | { type: 'START_INITIALIZING' }
  | { type: 'CITY_LOADED'; router: Router; nodeCoords: Float32Array; nodeCount: number; stopCount: number }
  | { type: 'LOAD_ERROR' }
  | { type: 'CHANGE_CITY' }
  | { type: 'SET_SOURCE'; node: number; latLng: [number, number] }
  | { type: 'SET_MODE'; mode: 'single' | 'sampled' }
  | { type: 'SET_DEPARTURE_TIME'; value: number }
  | { type: 'SET_DATE'; value: string }
  | { type: 'SET_SAMPLES'; value: number }
  | { type: 'SET_MAX_TIME'; value: number }
  | { type: 'SET_SLACK'; value: number }
  | { type: 'SET_PATTERN_COUNT'; count: number }
  | { type: 'COMPUTING' }
  | { type: 'QUERY_DONE'; travelTimes: Float32Array; ssspList: SsspList; sampleCounts: Uint32Array | null; totalSamples: number; timeMs: number }
  | { type: 'QUERY_ERROR' }
  | { type: 'PIN_DESTINATION'; node: number; latLng: [number, number]; hoverData: HoverData }
  | { type: 'UNPIN_DESTINATION' }
  | { type: 'SET_HOVER_DATA'; hoverData: HoverData }
  | { type: 'CLEAR_HOVER' }
  | { type: 'SELECT_SAMPLE'; idx: number | null }
  | { type: 'LOCK_SAMPLE'; idx: number | null }
  | { type: 'SHOW_COPIED_MESSAGE' }
  | { type: 'HIDE_COPIED_MESSAGE' };

export const initialState: AppState = {
  // City loading
  currentCity: null,
  loadingState: 'idle',
  loadingProgress: 0,

  // Controls
  mode: 'sampled',
  departureTime: 11 * 3600, // 11:00 AM
  date: new Date().toISOString().slice(0, 10),
  nSamples: 15,
  maxTimeMin: 45,
  transferSlack: 60,

  // Router state
  router: null,
  nodeCoords: null,
  sourceNode: null,
  sourceLatLng: null,

  // Query results
  travelTimes: null,
  ssspList: null,
  sampleCounts: null,
  totalSamples: 1,
  computeStatus: 'idle',
  computeTimeMs: 0,
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
        router: action.router,
        nodeCoords: action.nodeCoords,
        nodeCount: action.nodeCount,
        stopCount: action.stopCount,
        sourceNode: null,
        sourceLatLng: null,
        travelTimes: null,
        ssspList: null,
        pinnedNode: null,
        pinnedLatLng: null,
        hoverData: null,
        selectedSampleIdx: null,
        lockedSampleIdx: null,
        computeStatus: 'idle',
      };
    case 'LOAD_ERROR':
      return { ...state, loadingState: 'idle', currentCity: null };
    case 'CHANGE_CITY':
      return {
        ...state,
        loadingState: 'idle',
        currentCity: null,
        router: null,
        nodeCoords: null,
        travelTimes: null,
        ssspList: null,
        sourceNode: null,
        sourceLatLng: null,
        pinnedNode: null,
        pinnedLatLng: null,
        hoverData: null,
        selectedSampleIdx: null,
        lockedSampleIdx: null,
      };
    case 'SET_SOURCE':
      return { ...state, sourceNode: action.node, sourceLatLng: action.latLng, pinnedNode: null, pinnedLatLng: null, hoverData: null, selectedSampleIdx: null, lockedSampleIdx: null };
    case 'SET_MODE':
      return { ...state, mode: action.mode };
    case 'SET_DEPARTURE_TIME':
      return { ...state, departureTime: action.value };
    case 'SET_DATE':
      return { ...state, date: action.value };
    case 'SET_SAMPLES':
      return { ...state, nSamples: action.value };
    case 'SET_MAX_TIME':
      return { ...state, maxTimeMin: action.value };
    case 'SET_SLACK':
      return { ...state, transferSlack: action.value };
    case 'SET_PATTERN_COUNT':
      return { ...state, patternCount: action.count };
    case 'COMPUTING':
      return { ...state, computeStatus: 'computing' };
    case 'QUERY_DONE':
      return { ...state, travelTimes: action.travelTimes, ssspList: action.ssspList, sampleCounts: action.sampleCounts, totalSamples: action.totalSamples, computeStatus: 'done', computeTimeMs: action.timeMs, selectedSampleIdx: null, lockedSampleIdx: null };
    case 'QUERY_ERROR':
      return { ...state, computeStatus: 'error' };
    case 'SELECT_SAMPLE':
      return { ...state, selectedSampleIdx: action.idx };
    case 'LOCK_SAMPLE':
      return { ...state, selectedSampleIdx: action.idx, lockedSampleIdx: action.idx };
    case 'PIN_DESTINATION':
      return { ...state, pinnedNode: action.node, pinnedLatLng: action.latLng, hoverData: action.hoverData, selectedSampleIdx: null, lockedSampleIdx: null };
    case 'UNPIN_DESTINATION':
      return { ...state, pinnedNode: null, pinnedLatLng: null, hoverData: null, selectedSampleIdx: null, lockedSampleIdx: null };
    case 'SET_HOVER_DATA':
      return { ...state, hoverData: action.hoverData };
    case 'CLEAR_HOVER':
      if (state.pinnedNode !== null) return state;
      return { ...state, hoverData: null };
    case 'SHOW_COPIED_MESSAGE':
      return { ...state, showCopiedMessage: true };
    case 'HIDE_COPIED_MESSAGE':
      return { ...state, showCopiedMessage: false };
  }
}
