import init, { initThreadPool, TransitRouter, WasmSsspResult, WasmProfileRouting, __markRayonReady } from '../../pkg/transit_router';

// Rust-side display strings for a path (`PathDisplay`). Produced in Rust once
// per hover and consumed verbatim by HoverInfo — keeps the text the single
// source of truth so Rust tests can assert on it.
export interface PathDisplay {
  segmentLines: string[][];
  totalTimeLine: string;
}

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
  const meanTravel = profile.mean_travel_times();
  const fractions = profile.reachable_fractions();
  const travelTimes = new Float32Array(numNodes);
  const counts = new Uint32Array(numNodes);
  for (let i = 0; i < numNodes; i++) {
    travelTimes[i] = meanTravel[i] < 0xffffffff ? meanTravel[i] : NaN;
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
  display: PathDisplay | null;
}

// ============================================================================
// Profile mode: single call into WASM returns fully-structured paths.
// ============================================================================

// Matches the Rust `Path` / `PathSegment` structs in profile.rs (serde
// camelCase). The wrapping `PathView` flattens `Path` fields at the top level
// and adds `display` + `dominantRouteColorHex` as pure-function views.
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
interface RustPathView {
  homeDeparture: number;
  arrivalTime: number;
  totalTime: number;
  segments: RustPathSegment[];
  display: PathDisplay;
  dominantRouteColorHex: string | null;
}

// Convert the Rust PathView JSON into the legacy HoverPath shape used by
// HoverInfo.tsx + MapView.tsx. Shapes are fetched on demand via segment_shape.
function rustPathToHoverPath(router: Router, p: RustPathView): HoverPath {
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
    display: p.display,
  };
}

export function getProfileHoverData(router: Router, profile: Profile, node: number): HoverPath[] {
  if ((profile as unknown as { __wbg_ptr: number }).__wbg_ptr === 0) return [];
  const json = profile.optimal_paths(router, node);
  const views: RustPathView[] = JSON.parse(json);
  return views.map((p) => rustPathToHoverPath(router, p));
}

// Single-departure mode now goes through the same Rust `PathView` pipeline as
// profile mode — one call into WASM returns the fully-formatted path.
export function getSsspHoverData(router: Router, ssspList: SsspList, node: number): HoverPath[] {
  const out: HoverPath[] = [];
  for (const sssp of ssspList) {
    if ((sssp as unknown as { __wbg_ptr: number }).__wbg_ptr === 0) continue;
    try {
      const depTime = router.sssp_departure_time(sssp);
      const json = router.sssp_optimal_path(sssp, node);
      const view: RustPathView | null = JSON.parse(json);
      if (!view) {
        out.push({
          segments: [],
          totalTime: null,
          departureTime: depTime,
          routeColor: '#888888',
          display: null,
        });
        continue;
      }
      out.push(rustPathToHoverPath(router, view));
    } catch (e) {
      if (e instanceof Error && e.message && e.message.includes('null pointer')) continue;
      throw e;
    }
  }
  return out;
}

export function getAnyHoverData(
  router: Router,
  ssspList: SsspList | null,
  profile: Profile | null,
  node: number,
): HoverPath[] {
  if (profile && (profile as unknown as { __wbg_ptr: number }).__wbg_ptr !== 0) {
    return getProfileHoverData(router, profile, node);
  }
  return ssspList ? getSsspHoverData(router, ssspList, node) : [];
}
