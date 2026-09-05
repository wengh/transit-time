import init, {
  initThreadPool,
  TransitRouter,
  WasmProfileRouting,
  __markRayonReady,
} from '../../pkg/transit_router';

let router: TransitRouter | null = null;
let profile: WasmProfileRouting | null = null;
let wasmReady = false;
/// Backs the zero-copy `Uint16Array` view returned by `handleTravelTimesAt`.
let wasmMemory: WebAssembly.Memory | null = null;

const PROFILE_FRACTION_SCALE = 0xffff;

// ── Message types ──────────────────────────────────────────────────────────

export type WorkerRequest =
  | { id: number; type: 'initWasm' }
  | { id: number; type: 'loadRouter'; cityFile: string }
  | { id: number; type: 'runQuery'; params: RunQueryWorkerParams; cancelBuf: SharedArrayBuffer }
  | { id: number; type: 'getHoverData'; node: number }
  | { id: number; type: 'travelTimesAt'; departure: number }
  | { id: number; type: 'snapToNode'; lat: number; lon: number }
  | { id: number; type: 'numPatternsForDate'; date: number }
  | { id: number; type: 'freeProfile' };

export interface RunQueryWorkerParams {
  sourceNode: number;
  windowStart: number;
  windowEnd: number;
  date: string;
  transferSlack: number;
  maxTime: number;
}

export type WorkerResponse =
  | { id: number; type: 'result'; value: any }
  | { id: number; type: 'error'; message: string }
  | { id: number; type: 'progress'; done: number; total: number }
  | { id: number; type: 'loadProgress'; progress: number };

// ── Handlers ───────────────────────────────────────────────────────────────

async function handleInitWasm() {
  if (wasmReady) return;
  const wasm = await init();
  wasmMemory = wasm.memory;
  try {
    await initThreadPool(navigator.hardwareConcurrency || 4);
    __markRayonReady();
  } catch (e) {
    console.warn('WASM thread pool unavailable, using single-threaded mode:', e);
  }
  wasmReady = true;
}

async function handleLoadRouter(id: number, cityFile: string) {
  const resp = await fetch(`${import.meta.env.BASE_URL}data/${cityFile}`);
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  const total = parseInt(resp.headers.get('content-length') || '0');
  let loaded = 0;

  const decompressedStream = resp
    .body!.pipeThrough(
      new TransformStream({
        transform(chunk, controller) {
          loaded += chunk.length;
          if (total > 0) {
            postMessage({
              id,
              type: 'loadProgress',
              progress: Math.round((loaded / total) * 100),
            } satisfies WorkerResponse);
          }
          controller.enqueue(chunk);
        },
      })
    )
    .pipeThrough(new DecompressionStream('gzip'));

  const dataBytes = new Uint8Array(await new Response(decompressedStream).arrayBuffer());
  router = new TransitRouter(dataBytes);

  warmupTierUp(router);

  const allCoords = router.all_node_coords();
  const nodeCoords = new Float32Array(allCoords);
  // Collect route colors once
  const numRoutes = router.num_routes();
  const routeColors: string[] = [];
  for (let i = 0; i < numRoutes; i++) {
    routeColors.push(router.route_color(i));
  }
  return {
    nodeCoords,
    nodeCount: router.num_nodes(),
    stopCount: router.num_stops(),
    routeColors,
  };
}

// Run a tiny synthetic profile query so V8 marks the hot WASM functions
// (compute_with_index, relax, Frontier ops, Pareto merge) hot enough that
// TurboFan re-compiles them before the user's first real query lands.
// Without this the first real query runs entirely under Liftoff and is
// ~3x slower; V8 doesn't OSR long-running WASM calls.
function warmupTierUp(r: TransitRouter) {
  const now = new Date();
  const date = now.getFullYear() * 10000 + (now.getMonth() + 1) * 100 + now.getDate();
  const start = 12 * 3600;
  // Stops are SFC-sorted, so the median-index stop is roughly central
  // geographically — much more likely to have nearby transit than node 0
  // (which lands at a corner of the bounding box).
  const sourceNode = r.stop_node(Math.floor(r.num_stops() / 2));
  const lat = r.node_lat(sourceNode);
  const lon = r.node_lon(sourceNode);
  const WARMUP_BUDGET_MS = 200;
  const t0 = performance.now();
  const p = r.compute_profile(
    sourceNode,
    start, // window_start
    start + 15 * 60, // window_end
    date,
    60, // transfer_slack
    60 * 60, // max_time
    () => performance.now() - t0 > WARMUP_BUDGET_MS,
    true // is_warmup
  );
  const elapsed = performance.now() - t0;
  console.log(
    'Warmed up WASM tier-up with synthetic query from node',
    sourceNode,
    `(${lat.toFixed(5)}, ${lon.toFixed(5)})`,
    p
      ? `— got ${p.reachable_fractions().filter((f) => f > 0).length} reachable nodes`
      : '— cancelled (budget exceeded)',
    `in ${elapsed.toFixed(0)}ms`
  );
  p?.free();
}

let cancelFlag: Int32Array | null = null;

function handleRunQuery(id: number, params: RunQueryWorkerParams) {
  if (!router) throw new Error('Router not loaded');
  freeCurrentProfile();

  const numNodes = router.num_nodes();
  const dateInt = parseInt(params.date.replace(/-/g, ''));

  // compute_profile now returns null when the progress callback requested
  // cancellation (or any internal cancellation path fires).
  profile =
    router.compute_profile(
      params.sourceNode,
      params.windowStart,
      params.windowEnd,
      dateInt,
      params.transferSlack,
      params.maxTime,
      (done: number, total: number) => {
        postMessage({ id, type: 'progress', done, total } satisfies WorkerResponse);
        return cancelFlag ? Atomics.load(cancelFlag, 0) !== 0 : false;
      },
      false // is_warmup
    ) ?? null;

  if (!profile) {
    freeCurrentProfile();
    throw new Error('cancelled');
  }

  const meanTravel = profile.mean_travel_times();
  const fractions = profile.reachable_fractions();
  const travelTimes = new Float32Array(numNodes);
  const sampleCounts = new Uint32Array(numNodes);
  for (let i = 0; i < numNodes; i++) {
    travelTimes[i] = fractions[i] > 0 ? meanTravel[i] : NaN;
    sampleCounts[i] = fractions[i];
  }
  return {
    travelTimes,
    sampleCounts,
    totalSamples: PROFILE_FRACTION_SCALE,
    departureTime: params.windowStart,
    numThreads: profile.num_threads(),
  };
}

// Matches the Rust PathView JSON shape
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
  display: { segmentLines: string[][]; totalTimeLine: string };
  dominantRouteColorHex: string | null;
}

// Segment shapes for the current profile, keyed by route + node sequence.
// A hover response used to carry every path's shape as fresh nested
// `[lat, lon]` arrays — dozens of paths sharing the same trunk segments — and
// building plus structured-cloning that took 100–300 ms per hover. Shapes are
// now flat `Float32Array`s (a memcpy to clone) and identical segments across
// paths point at the *same* array, which the structured clone serialises
// once. The cache lives as long as the profile: the same source produces the
// same trunk segments hover after hover.
let shapeCache = new Map<string, Float32Array>();

function segmentShape(kind: 'walk' | 'transit', routeIndex: number | null, nodeSequence: number[]) {
  const key = (kind === 'transit' ? routeIndex : 'w') + ':' + nodeSequence.join(',');
  let shape = shapeCache.get(key);
  if (!shape) {
    shape = router!.segment_shape(
      kind === 'transit' ? (routeIndex ?? undefined) : undefined,
      new Uint32Array(nodeSequence)
    );
    shapeCache.set(key, shape);
  }
  return shape;
}

function handleGetHoverData(node: number) {
  if (!router || !profile) return { paths: [], representativeIndex: null };
  if ((profile as any).__wbg_ptr === 0) return { paths: [], representativeIndex: null };
  const json = profile.optimal_paths(router, node);
  const data: { paths: RustPathView[]; representativeIndex: number | null } = JSON.parse(json);
  const paths = data.paths.map((p) => {
    const segments = p.segments.map((seg) => ({
      edgeType: seg.kind === 'transit' ? 1 : 0,
      routeIdx: seg.routeIndex ?? 0xffffffff,
      routeName: seg.routeName ?? '',
      startStopName: seg.startStopName,
      endStopName: seg.endStopName,
      endNodeIdx: seg.nodeSequence[seg.nodeSequence.length - 1] ?? -1,
      duration: seg.endTime - seg.startTime,
      waitTime: seg.waitTime,
      coords: segmentShape(seg.kind, seg.routeIndex, seg.nodeSequence),
    }));
    return {
      segments,
      totalTime: p.totalTime,
      departureTime: p.homeDeparture,
      routeColor: p.dominantRouteColorHex ?? '#888888',
      display: p.display,
    };
  });
  return { paths, representativeIndex: data.representativeIndex };
}

// Returns a Uint16Array view directly onto WASM linear memory — no copy.
// The buffer is overwritten on the next call, so callers must finish reading
// (e.g. upload to GL) before awaiting the next frame. Safe under the worker's
// request/response RPC model; would race under any concurrent dispatch.
function handleTravelTimesAt(departure: number): Uint16Array {
  if (!profile) throw new Error('No profile loaded');
  if ((profile as any).__wbg_ptr === 0) throw new Error('Profile freed');
  if (!wasmMemory) throw new Error('WASM not initialised');
  profile.travel_times_at_into(departure);
  const ptr = profile.travel_times_buffer_ptr();
  const n = profile.num_nodes();
  return new Uint16Array(wasmMemory.buffer, ptr, n);
}

function freeCurrentProfile() {
  shapeCache = new Map();
  if (!profile) return;
  try {
    profile.free();
  } catch {
    /* ignore */
  }
  profile = null;
}

// ── Message dispatcher ─────────────────────────────────────────────────────

self.onmessage = async (e: MessageEvent<WorkerRequest>) => {
  const { id, type } = e.data;
  try {
    let value: any;
    // Transferables for the response — zero-copy hand-off of large buffers.
    const transfer: Transferable[] = [];
    switch (type) {
      case 'initWasm':
        await handleInitWasm();
        value = null;
        break;
      case 'loadRouter':
        value = await handleLoadRouter(id, e.data.cityFile);
        break;
      case 'runQuery':
        cancelFlag = new Int32Array(e.data.cancelBuf);
        value = handleRunQuery(id, e.data.params);
        break;
      case 'getHoverData':
        value = handleGetHoverData(e.data.node);
        break;
      case 'travelTimesAt':
        // SAB-backed view; do NOT add to `transfer` — SABs throw on transfer.
        value = handleTravelTimesAt(e.data.departure);
        break;
      case 'snapToNode':
        value = router?.snap_to_node(e.data.lat, e.data.lon) ?? null;
        break;
      case 'numPatternsForDate':
        value = router?.num_patterns_for_date(e.data.date) ?? 0;
        break;
      case 'freeProfile':
        freeCurrentProfile();
        value = null;
        break;
    }
    postMessage({ id, type: 'result', value } satisfies WorkerResponse, { transfer });
  } catch (err: any) {
    postMessage({ id, type: 'error', message: String(err) } satisfies WorkerResponse);
  }
};
