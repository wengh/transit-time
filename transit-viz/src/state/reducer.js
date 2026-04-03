export const initialState = {
  // City loading
  currentCity: null,
  loadingState: 'idle', // 'idle' | 'loading' | 'initializing' | 'ready'
  loadingProgress: 0,

  // Controls
  mode: 'single',
  departureTime: 28800, // 08:00
  date: new Date().toISOString().slice(0, 10),
  nSamples: 10,
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
  computeStatus: 'idle', // 'idle' | 'computing' | 'done' | 'error'
  computeTimeMs: 0,
  patternCount: 0,
  nodeCount: 0,
  stopCount: 0,

  // Destination
  pinnedNode: null,
  pinnedLatLng: null,
  hoverData: null,

  // UI feedback
  showCopiedMessage: false,
};

export function reducer(state, action) {
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
      };
    case 'SET_SOURCE':
      return { ...state, sourceNode: action.node, sourceLatLng: action.latLng, pinnedNode: null, pinnedLatLng: null, hoverData: null };
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
      return { ...state, travelTimes: action.travelTimes, ssspList: action.ssspList, computeStatus: 'done', computeTimeMs: action.timeMs };
    case 'QUERY_ERROR':
      return { ...state, computeStatus: 'error' };
    case 'PIN_DESTINATION':
      return { ...state, pinnedNode: action.node, pinnedLatLng: action.latLng, hoverData: action.hoverData };
    case 'UNPIN_DESTINATION':
      return { ...state, pinnedNode: null, pinnedLatLng: null, hoverData: null };
    case 'SET_HOVER_DATA':
      return { ...state, hoverData: action.hoverData };
    case 'CLEAR_HOVER':
      if (state.pinnedNode !== null) return state;
      return { ...state, hoverData: null };
    case 'SHOW_COPIED_MESSAGE':
      return { ...state, showCopiedMessage: true };
    case 'HIDE_COPIED_MESSAGE':
      return { ...state, showCopiedMessage: false };
    default:
      return state;
  }
}
