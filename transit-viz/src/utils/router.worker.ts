import init, { initThreadPool, TransitRouter, WasmProfileRouting, __markRayonReady } from '../../pkg/transit_router';

let router: TransitRouter | null = null;
let profile: WasmProfileRouting | null = null;
let wasmReady = false;

const PROFILE_FRACTION_SCALE = 0xffff;

// ── Message types ──────────────────────────────────────────────────────────

export type WorkerRequest =
  | { id: number; type: 'initWasm' }
  | { id: number; type: 'loadRouter'; cityFile: string }
  | { id: number; type: 'runQuery'; params: RunQueryWorkerParams }
  | { id: number; type: 'getHoverData'; node: number }
  | { id: number; type: 'snapToNode'; lat: number; lon: number }
  | { id: number; type: 'numPatternsForDate'; date: number }
  | { id: number; type: 'freeProfile' };

export interface RunQueryWorkerParams {
  sourceNode: number;
  departureTime: number;
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
  await init();
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

  const decompressedStream = resp.body!
    .pipeThrough(new TransformStream({
      transform(chunk, controller) {
        loaded += chunk.length;
        if (total > 0) {
          postMessage({ id, type: 'loadProgress', progress: Math.round((loaded / total) * 100) } satisfies WorkerResponse);
        }
        controller.enqueue(chunk);
      }
    }))
    .pipeThrough(new DecompressionStream('gzip'));

  const dataBytes = new Uint8Array(await new Response(decompressedStream).arrayBuffer());
  router = new TransitRouter(dataBytes);

  const allCoords = router.all_node_coords();
  const nodeCoords = new Float32Array(allCoords);
  // Collect route colors once
  const numPatterns = router.num_patterns();
  const routeColors: string[] = [];
  for (let i = 0; i < numPatterns; i++) {
    routeColors.push(router.route_color(i));
  }
  return {
    nodeCoords,
    nodeCount: router.num_nodes(),
    stopCount: router.num_stops(),
    routeColors,
  };
}

function handleRunQuery(id: number, params: RunQueryWorkerParams) {
  if (!router) throw new Error('Router not loaded');
  freeCurrentProfile();

  const numNodes = router.num_nodes();
  const dateInt = parseInt(params.date.replace(/-/g, ''));
  const windowEnd = params.departureTime + 3600;

  profile = router.compute_profile(
    params.sourceNode,
    params.departureTime,
    windowEnd,
    dateInt,
    params.transferSlack,
    params.maxTime,
    (done: number, total: number) => {
      postMessage({ id, type: 'progress', done, total } satisfies WorkerResponse);
    },
  );

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
    departureTime: params.departureTime,
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

function handleGetHoverData(node: number) {
  if (!router || !profile) return [];
  if ((profile as any).__wbg_ptr === 0) return [];
  const json = profile.optimal_paths(router, node);
  const views: RustPathView[] = JSON.parse(json);
  return views.map((p) => {
    const segments = p.segments.map((seg) => {
      const nodes = new Uint32Array(seg.nodeSequence);
      const flat = router!.segment_shape(
        seg.kind === 'transit' ? seg.routeIndex ?? undefined : undefined,
        nodes,
      );
      const coords: Array<[number, number]> = [];
      for (let i = 0; i + 1 < flat.length; i += 2) {
        coords.push([flat[i], flat[i + 1]]);
      }
      return {
        edgeType: seg.kind === 'transit' ? 1 : 0,
        routeIdx: seg.routeIndex ?? 0xffffffff,
        routeName: seg.routeName ?? '',
        startStopName: seg.startStopName,
        endStopName: seg.endStopName,
        endNodeIdx: seg.nodeSequence[seg.nodeSequence.length - 1] ?? -1,
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
  });
}

function freeCurrentProfile() {
  if (!profile) return;
  try { profile.free(); } catch { /* ignore */ }
  profile = null;
}

// ── Message dispatcher ─────────────────────────────────────────────────────

self.onmessage = async (e: MessageEvent<WorkerRequest>) => {
  const { id, type } = e.data;
  try {
    let value: any;
    switch (type) {
      case 'initWasm':
        await handleInitWasm();
        value = null;
        break;
      case 'loadRouter':
        value = await handleLoadRouter(id, e.data.cityFile);
        break;
      case 'runQuery':
        value = handleRunQuery(id, e.data.params);
        break;
      case 'getHoverData':
        value = handleGetHoverData(e.data.node);
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
    postMessage({ id, type: 'result', value } satisfies WorkerResponse);
  } catch (err: any) {
    postMessage({ id, type: 'error', message: String(err) } satisfies WorkerResponse);
  }
};
