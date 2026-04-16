import init, { initThreadPool, TransitRouter, WasmSsspResult, WasmProfileResult, __markRayonReady } from '../../pkg/transit_router';
import { ROUTE_COLORS, hexToRgb } from './colors';

let wasmReady = false;

export type Router = TransitRouter;
export type SsspList = WasmSsspResult[];
export type Profile = WasmProfileResult;

// Scale factor mapping profile fraction ∈ [0,1] into the integer (sampleCounts, totalSamples) pair
// the existing webgl shader consumes. Chosen so rounding error is < 0.1% of a full window.
const PROFILE_FRACTION_SCALE = 1024;

export interface PathSegment {
  edgeType: number;
  routeIdx: number;
  routeName: string;
  startStopName: string;
  endStopName: string;
  endNodeIdx: number;
  duration: number;
  waitTime: number;
  coords: Array<[number, number]>;
}

export interface QueryResult {
  travelTimes: Float32Array;
  ssspList: SsspList;          // single mode: [sssp]; sampled mode: []
  profile: Profile | null;     // sampled mode: the WasmProfileResult; single mode: null
  sampleCounts: Uint32Array | null; // null in single mode; counts[i]/totalSamples = reachable fraction
  totalSamples: number;             // 1 in single; PROFILE_FRACTION_SCALE in sampled (profile-backed)
  departureTime: number;            // window start (sampled) or the exact departure (single)
}

// WALK_ONLY sentinel value matching Rust-side WasmProfileResult.
export const PROFILE_WALK_ONLY = 0xffff;

export interface RunQueryParams {
  sourceNode: number;
  mode: 'single' | 'sampled';
  departureTime: number;
  date: string;
  nSamples: number; // ignored in sampled mode now — profile is analytic, not sampled
  transferSlack: number;
  maxTime: number;
  prevSsspList?: SsspList;
  prevProfile?: Profile | null;
}

export async function initWasm() {
  if (wasmReady) return;
  await init();
  try {
    await initThreadPool(navigator.hardwareConcurrency || 4);
    __markRayonReady();
  } catch (e) {
    console.warn('WASM thread pool unavailable, using single-threaded mode:', e);
  }
  wasmReady = true;
}

export async function loadRouter(cityFile: string, onProgress?: (progress: number) => void): Promise<Router> {
  const resp = await fetch(`${import.meta.env.BASE_URL}data/${cityFile}`);
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  const total = parseInt(resp.headers.get('content-length') || '0');
  let loaded = 0;

  // Decompress pipelined with download; track progress on compressed bytes
  const decompressedStream = resp.body!
    .pipeThrough(new TransformStream({
      transform(chunk, controller) {
        loaded += chunk.length;
        if (total > 0 && onProgress) onProgress(Math.round((loaded / total) * 100));
        controller.enqueue(chunk);
      }
    }))
    .pipeThrough(new DecompressionStream('gzip'));

  const dataBytes = new Uint8Array(await new Response(decompressedStream).arrayBuffer());

  return new TransitRouter(dataBytes);
}

export function freeSsspList(ssspList: SsspList | null | undefined) {
  if (!ssspList) return;
  for (const sssp of ssspList) {
    try {
      sssp.free();
    } catch (_) {
      // ignore
    }
  }
}

export function freeProfile(profile: Profile | null | undefined) {
  if (!profile) return;
  try {
    profile.free();
  } catch (_) {
    // ignore
  }
}

export function runQuery(router: Router, params: RunQueryParams): QueryResult {
  const { sourceNode, mode, departureTime, date, transferSlack, maxTime, prevSsspList, prevProfile } = params;

  // Free previous results before allocating new ones to avoid WASM OOM.
  freeSsspList(prevSsspList);
  freeProfile(prevProfile);
  const numNodes = router.num_nodes();
  const dateInt = parseInt(date.replace(/-/g, ''));

  if (mode === 'single') {
    const sssp = router.run_tdd_full_for_date(sourceNode, departureTime, dateInt, transferSlack, maxTime);
    const ssspList: SsspList = [sssp];
    const travelTimes = new Float32Array(numNodes);
    for (let i = 0; i < numNodes; i++) {
      const arr = router.node_arrival_time(sssp, i);
      travelTimes[i] = arr < 0xffffffff ? arr - departureTime : NaN;
    }
    return { travelTimes, ssspList, profile: null, sampleCounts: null, totalSamples: 1, departureTime };
  }

  // Sampled mode = analytic profile routing over a 1-hour window.
  const windowEnd = departureTime + 3600;
  const profile: Profile = router.run_profile_for_date(
    sourceNode, departureTime, windowEnd, dateInt, transferSlack, maxTime
  );
  const travelTimes = new Float32Array(numNodes);
  const counts = new Uint32Array(numNodes);
  for (let i = 0; i < numNodes; i++) {
    // Best-case travel time = smallest arrival_delta in the frontier.
    const bestArrDelta = profile.node_best_arrival_delta(i);
    travelTimes[i] = bestArrDelta < 0xffffffff ? bestArrDelta : NaN;
    // Reachable fraction, integer-scaled so the existing shader can read counts/total.
    const frac = profile.node_reachable_fraction(i, maxTime);
    counts[i] = Math.round(frac * PROFILE_FRACTION_SCALE);
  }
  return {
    travelTimes,
    ssspList: [],
    profile,
    sampleCounts: counts,
    totalSamples: PROFILE_FRACTION_SCALE,
    departureTime,
  };
}

export interface HoverPath {
  segments: PathSegment[];
  totalTime: number | null;
  departureTime: number;
  routeColor: string;
}

function getDominantRouteColor(router: Router, segments: PathSegment[]): string {
  const transitSegs = segments.filter(s => s.edgeType === 1);
  if (transitSegs.length === 0) return '#888888';
  const dominant = transitSegs.reduce((a, b) => (a.duration >= b.duration ? a : b));
  if (dominant.routeIdx >= 0xffffffff) return ROUTE_COLORS[0];
  const hex = router.route_color(dominant.routeIdx);
  if (hex) {
    const rgb = hexToRgb(hex);
    if (rgb) {
      const lum = (rgb[0] * 299 + rgb[1] * 587 + rgb[2] * 114) / 1000;
      if (lum > 0 && lum < 100) {
        const s = 100 / lum;
        return `rgb(${Math.min(255, Math.round(rgb[0] * s))},${Math.min(255, Math.round(rgb[1] * s))},${Math.min(255, Math.round(rgb[2] * s))})`;
      } else if (lum > 220) {
        const s = 220 / lum;
        return `rgb(${Math.round(rgb[0] * s)},${Math.round(rgb[1] * s)},${Math.round(rgb[2] * s)})`;
      }
      return hex;
    }
  }
  return ROUTE_COLORS[0];
}

// Abstracts per-node arrival/boarding lookups so single-SSSP and profile-entry paths
// can share the segment parser below.
interface PathLookup {
  arrivalTime(node: number): number;
  boardingTime(node: number): number;
}

function ssspLookup(router: Router, sssp: WasmSsspResult): PathLookup {
  return {
    arrivalTime: (n) => router.node_arrival_time(sssp, n),
    boardingTime: (n) => router.node_boarding_time(sssp, n),
  };
}

function profileLookup(router: Router, profile: Profile, homeDepDelta: number): PathLookup {
  return {
    arrivalTime: (n) => profile.node_arrival_for_home_dep(n, homeDepDelta),
    // profile_boarding_time is keyed by (alight_node, home_dep_delta) and gives the
    // vehicle-departure time at the boarding stop for the last transit leg alighting at n.
    // 0 means "no transit leg alights at this node along this journey" (= walk or start).
    boardingTime: (n) => router.profile_boarding_time(profile, n, homeDepDelta),
  };
}

// Preferred entry point when the result could be either single-SSSP or profile.
// Returns all Pareto-optimal paths for the sampled/profile case, or just the
// single path for single-departure mode.
export function getAnyHoverData(
  router: Router,
  ssspList: SsspList | null,
  profile: Profile | null,
  node: number,
): HoverPath[] {
  if (profile && (profile as any).__wbg_ptr !== 0) {
    return getProfileHoverData(router, profile, node);
  }
  return ssspList ? getHoverData(router, ssspList, node) : [];
}

export function getHoverData(router: Router, ssspList: SsspList, node: number): HoverPath[] {
  const allPaths: HoverPath[] = [];
  for (const sssp of ssspList) {
    if ((sssp as any).__wbg_ptr === 0) continue; // Skip freed wasm objects
    try {
      const depTime = router.sssp_departure_time(sssp);
      const arrival = router.node_arrival_time(sssp, node);
      if (arrival >= 0xffffffff) {
        allPaths.push({ segments: [], totalTime: null, departureTime: depTime, routeColor: '#888888' });
        continue;
      }
      const pathArray = router.reconstruct_path(sssp, node);
      const segments = parsePathSegments(router, ssspLookup(router, sssp), pathArray);
      allPaths.push({
        segments,
        totalTime: arrival - depTime,
        departureTime: depTime,
        routeColor: getDominantRouteColor(router, segments),
      });
    } catch (e) {
      // In case of any Wasm errors about null pointers, skip safely
      if (e instanceof Error && e.message && e.message.includes('null pointer')) continue;
      throw e;
    }
  }
  return allPaths;
}

// Iterate every Pareto-optimal frontier entry at `node` and reconstruct its path.
// Each entry is a distinct (home_dep, arrival) pair. Walk-only entry (if present)
// is at index 0 with home_dep_delta = WALK_ONLY.
export function getProfileHoverData(router: Router, profile: Profile, node: number): HoverPath[] {
  if ((profile as any).__wbg_ptr === 0) return [];
  const allPaths: HoverPath[] = [];
  const windowStart = profile.window_start();
  const len = profile.frontier_len(node);
  for (let i = 0; i < len; i++) {
    try {
      // [arr_delta, home_dep_delta, prev_node, route_index]
      const entry = profile.frontier_entry(node, i);
      const arrDelta = entry[0];
      const homeDepDelta = entry[1];
      const isWalkOnly = homeDepDelta === PROFILE_WALK_ONLY;
      // For walk-only entry, "departure" is any time in window — use windowStart as display anchor.
      const depTime = isWalkOnly ? windowStart : windowStart + homeDepDelta;
      const arrival = windowStart + arrDelta;
      const pathArray = profile.reconstruct_profile_path(node, i);
      const lookup = profileLookup(router, profile, homeDepDelta);
      const segments = parsePathSegments(router, lookup, pathArray);
      allPaths.push({
        segments,
        totalTime: arrival - depTime,
        departureTime: depTime,
        routeColor: getDominantRouteColor(router, segments),
      });
    } catch (e) {
      if (e instanceof Error && e.message && e.message.includes('null pointer')) continue;
      throw e;
    }
  }
  // Sort by departure time ascending so hover UI shows earliest-home-dep first.
  allPaths.sort((a, b) => a.departureTime - b.departureTime);
  return allPaths;
}

function parsePathSegments(router: Router, lookup: PathLookup, pathArray: Uint32Array): PathSegment[] {
  const segments: PathSegment[] = [];
  let i = 0;
  while (i < pathArray.length) {
    const startIdx = i;
    const edgeType = pathArray[i + 1];
    const routeIdx = pathArray[i + 2];
    while (i + 3 < pathArray.length && pathArray[i + 3 + 1] === edgeType && pathArray[i + 3 + 2] === routeIdx) {
      i += 3;
    }
    const endIdx = i;
    const startNode = pathArray[startIdx];
    const endNode = pathArray[endIdx];
    const startTime = lookup.arrivalTime(startNode);
    const endTime = lookup.arrivalTime(endNode);

    const coords: Array<[number, number]> = [];
    for (let j = startIdx; j <= endIdx; j += 3) {
      const n = pathArray[j];
      coords.push([router.node_lat(n), router.node_lon(n)]);
    }

    let boardStopName = '';
    let boardNode = startNode;
    if (edgeType === 1 && segments.length > 0) {
      const prev = segments[segments.length - 1];
      boardStopName = prev.endStopName;
      boardNode = prev.endNodeIdx;
    }

    let finalCoords = coords;
    if (edgeType === 1 && routeIdx < 0xffffffff) {
      // Collect all node indices in this transit segment
      const segNodes: number[] = [];
      if (boardNode !== pathArray[startIdx]) segNodes.push(boardNode);
      for (let j = startIdx; j <= endIdx; j += 3) segNodes.push(pathArray[j]);

      // Chain per-leg shapes for each consecutive stop pair
      const chainedCoords: Array<[number, number]> = [];
      for (let j = 0; j < segNodes.length - 1; j++) {
        const legShape = router.route_shape_between(routeIdx, segNodes[j], segNodes[j + 1]);
        const skip = (j > 0 && legShape.length >= 2) ? 2 : 0;
        for (let k = skip; k < legShape.length; k += 2) {
          chainedCoords.push([legShape[k], legShape[k + 1]]);
        }
      }

      if (chainedCoords.length >= 2) {
        finalCoords = chainedCoords;
      } else {
        // No GTFS shapes available — fall back to straight lines through segNodes.
        // segNodes includes the boarding node (unlike coords which starts at pathArray[startIdx]),
        // so this produces a visible polyline even when the city has no shape data.
        finalCoords = segNodes.map(n => [router.node_lat(n), router.node_lon(n)] as [number, number]);
      }
    }

    let waitTime = 0;
    if (edgeType === 1) {
      const boardingTime = lookup.boardingTime(startNode);
      if (boardingTime > 0 && segments.length > 0) {
        const prev = segments[segments.length - 1];
        const arrivalAtBoardStop = lookup.arrivalTime(prev.endNodeIdx);
        waitTime = boardingTime - arrivalAtBoardStop;
      } else if (boardingTime > 0) {
        const arrivalAtStop = lookup.arrivalTime(boardNode);
        waitTime = boardingTime - arrivalAtStop;
      }
    }

    let duration;
    if (edgeType === 1 && waitTime >= 0) {
      const boardingTime = lookup.boardingTime(startNode);
      duration = boardingTime > 0 ? endTime - boardingTime : endTime - startTime;
    } else {
      duration = endTime - startTime;
    }

    segments.push({
      edgeType,
      routeIdx,
      routeName: edgeType === 1 && routeIdx < 0xffffffff ? router.route_name(routeIdx) : '',
      startStopName: edgeType === 1 ? boardStopName : router.node_stop_name(startNode),
      endStopName: router.node_stop_name(endNode),
      endNodeIdx: endNode,
      duration,
      waitTime,
      coords: finalCoords,
    });
    i += 3;
  }
  return segments;
}
