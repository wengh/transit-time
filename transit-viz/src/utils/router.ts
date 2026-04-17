import init, { initThreadPool, TransitRouter, WasmSsspResult, WasmProfileRouting, __markRayonReady } from '../../pkg/transit_router';

let wasmReady = false;

export type Router = TransitRouter;
export type SsspList = WasmSsspResult[];
export type Profile = WasmProfileRouting;

// Scale factor mapping profile fraction ∈ [0,1] into the integer (sampleCounts, totalSamples) pair
// the existing webgl shader consumes. Chosen so rounding error is < 0.1% of a full window.
const PROFILE_FRACTION_SCALE = 1024;

/// Legacy per-segment shape consumed by HoverInfo.tsx and the map polyline
/// renderer. edgeType: 0 = walk, 1 = transit.
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
  profile: Profile | null;     // sampled mode: the WasmProfileRouting; single mode: null
  sampleCounts: Uint32Array | null; // null in single mode; counts[i]/totalSamples = reachable fraction
  totalSamples: number;             // 1 in single; PROFILE_FRACTION_SCALE in sampled (profile-backed)
  departureTime: number;            // window start (sampled) or the exact departure (single)
}

export interface RunQueryParams {
  sourceNode: number;
  mode: 'single' | 'sampled';
  departureTime: number;
  date: string;
  nSamples: number;
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
    try { sssp.free(); } catch { /* ignore */ }
  }
}

export function freeProfile(profile: Profile | null | undefined) {
  if (!profile) return;
  try { profile.free(); } catch { /* ignore */ }
}

export function runQuery(router: Router, params: RunQueryParams): QueryResult {
  const { sourceNode, mode, departureTime, date, transferSlack, maxTime, prevSsspList, prevProfile } = params;

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
  const profile: Profile = router.compute_profile(
    sourceNode, departureTime, windowEnd, dateInt, transferSlack, maxTime
  );
  // Pull per-node isochrone arrays in one WASM call each.
  const minTravel = profile.min_travel_times();
  const fractions = profile.reachable_fractions();
  const travelTimes = new Float32Array(numNodes);
  const counts = new Uint32Array(numNodes);
  for (let i = 0; i < numNodes; i++) {
    travelTimes[i] = minTravel[i] < 0xffffffff ? minTravel[i] : NaN;
    counts[i] = Math.round(fractions[i] * PROFILE_FRACTION_SCALE);
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

// ============================================================================
// Profile mode: single call into WASM returns fully-structured paths.
// ============================================================================

// Matches the Rust `Path` / `PathSegment` structs in profile.rs (serde
// camelCase).
interface RustPathSegment {
  kind: 'walk' | 'transit';
  startTime: number;
  endTime: number;
  waitTime: number;
  startStopName: string;
  endStopName: string;
  routeIndex: number | null;
  routeName: string | null;
  nodeSequence: number[];
}
interface RustPath {
  homeDeparture: number;
  arrivalTime: number;
  totalTime: number;
  segments: RustPathSegment[];
  dominantRouteColorHex: string | null;
}

// Convert the Rust Path JSON into the legacy HoverPath shape used by
// HoverInfo.tsx + MapView.tsx. Shapes are fetched on demand via segment_shape.
function rustPathToHoverPath(router: Router, p: RustPath): HoverPath {
  const segments: PathSegment[] = p.segments.map((seg) => {
    const edgeType = seg.kind === 'transit' ? 1 : 0;
    const routeIdx = seg.routeIndex ?? 0xffffffff;
    const nodes = new Uint32Array(seg.nodeSequence);
    const flat = router.segment_shape(
      seg.kind === 'transit' ? seg.routeIndex ?? undefined : undefined,
      nodes,
    );
    const coords: Array<[number, number]> = [];
    for (let i = 0; i + 1 < flat.length; i += 2) {
      coords.push([flat[i], flat[i + 1]]);
    }
    const endNodeIdx = seg.nodeSequence[seg.nodeSequence.length - 1] ?? -1;
    return {
      edgeType,
      routeIdx,
      routeName: seg.routeName ?? '',
      startStopName: seg.startStopName,
      endStopName: seg.endStopName,
      endNodeIdx,
      duration: seg.endTime - seg.startTime,
      waitTime: seg.waitTime,
      coords,
    };
  });
  return {
    segments,
    totalTime: p.totalTime,
    departureTime: p.homeDeparture,
    routeColor: p.dominantRouteColorHex ?? '#888888',
  };
}

export function getProfileHoverData(router: Router, profile: Profile, node: number): HoverPath[] {
  if ((profile as unknown as { __wbg_ptr: number }).__wbg_ptr === 0) return [];
  const json = profile.optimal_paths(router, node);
  const paths: RustPath[] = JSON.parse(json);
  return paths.map((p) => rustPathToHoverPath(router, p));
}

// ============================================================================
// Single-departure mode: unchanged stitching (SSSP → path triples → segments).
// ============================================================================

export function getAnyHoverData(
  router: Router,
  ssspList: SsspList | null,
  profile: Profile | null,
  node: number,
): HoverPath[] {
  if (profile && (profile as unknown as { __wbg_ptr: number }).__wbg_ptr !== 0) {
    return getProfileHoverData(router, profile, node);
  }
  return ssspList ? getHoverData(router, ssspList, node) : [];
}

export function getHoverData(router: Router, ssspList: SsspList, node: number): HoverPath[] {
  const allPaths: HoverPath[] = [];
  for (const sssp of ssspList) {
    if ((sssp as unknown as { __wbg_ptr: number }).__wbg_ptr === 0) continue;
    try {
      const depTime = router.sssp_departure_time(sssp);
      const arrival = router.node_arrival_time(sssp, node);
      if (arrival >= 0xffffffff) {
        allPaths.push({ segments: [], totalTime: null, departureTime: depTime, routeColor: '#888888' });
        continue;
      }
      const pathArray = router.reconstruct_path(sssp, node);
      const segments = parseSsspPathSegments(router, sssp, pathArray);
      allPaths.push({
        segments,
        totalTime: arrival - depTime,
        departureTime: depTime,
        routeColor: getDominantSsspRouteColor(router, segments),
      });
    } catch (e) {
      if (e instanceof Error && e.message && e.message.includes('null pointer')) continue;
      throw e;
    }
  }
  return allPaths;
}

function getDominantSsspRouteColor(router: Router, segments: PathSegment[]): string {
  const transitSegs = segments.filter(s => s.edgeType === 1);
  if (transitSegs.length === 0) return '#888888';
  const dominant = transitSegs.reduce((a, b) => (a.duration >= b.duration ? a : b));
  if (dominant.routeIdx >= 0xffffffff) return '#888888';
  const hex = router.route_color(dominant.routeIdx);
  return adjustColorVisibility(hex);
}

function adjustColorVisibility(hex: string): string {
  if (!hex || hex.length < 7) return '#888888';
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  if (Number.isNaN(r + g + b)) return hex;
  const lum = (r * 299 + g * 587 + b * 114) / 1000;
  if (lum > 0 && lum < 100) {
    const s = 100 / lum;
    return `rgb(${Math.min(255, Math.round(r * s))},${Math.min(255, Math.round(g * s))},${Math.min(255, Math.round(b * s))})`;
  } else if (lum > 220) {
    const s = 220 / lum;
    return `rgb(${Math.round(r * s)},${Math.round(g * s)},${Math.round(b * s)})`;
  }
  return hex;
}

function parseSsspPathSegments(router: Router, sssp: WasmSsspResult, pathArray: Uint32Array): PathSegment[] {
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
    const startTime = router.node_arrival_time(sssp, startNode);
    const endTime = router.node_arrival_time(sssp, endNode);

    let boardStopName = '';
    let boardNode = startNode;
    if (edgeType === 1 && segments.length > 0) {
      const prev = segments[segments.length - 1];
      boardStopName = prev.endStopName;
      boardNode = prev.endNodeIdx;
    }

    // Build node sequence for shape retrieval.
    const segNodes: number[] = [];
    if (edgeType === 1 && boardNode !== startNode) segNodes.push(boardNode);
    for (let j = startIdx; j <= endIdx; j += 3) segNodes.push(pathArray[j]);

    const routeIndexForShape = edgeType === 1 && routeIdx < 0xffffffff ? routeIdx : undefined;
    const flat = router.segment_shape(routeIndexForShape, new Uint32Array(segNodes));
    const coords: Array<[number, number]> = [];
    for (let j = 0; j + 1 < flat.length; j += 2) {
      coords.push([flat[j], flat[j + 1]]);
    }

    let waitTime = 0;
    if (edgeType === 1) {
      const boardingTime = router.node_boarding_time(sssp, startNode);
      if (boardingTime > 0 && segments.length > 0) {
        const prev = segments[segments.length - 1];
        const arrivalAtBoardStop = router.node_arrival_time(sssp, prev.endNodeIdx);
        waitTime = boardingTime - arrivalAtBoardStop;
      } else if (boardingTime > 0) {
        const arrivalAtStop = router.node_arrival_time(sssp, boardNode);
        waitTime = boardingTime - arrivalAtStop;
      }
    }

    let duration;
    if (edgeType === 1 && waitTime >= 0) {
      const boardingTime = router.node_boarding_time(sssp, startNode);
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
      coords,
    });
    i += 3;
  }
  return segments;
}
