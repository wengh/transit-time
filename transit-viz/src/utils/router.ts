import init, { initThreadPool, TransitRouter, WasmSsspResult, __markRayonReady } from '../../pkg/transit_router';
import { ROUTE_COLORS, hexToRgb } from './colors';

let wasmReady = false;

export type Router = TransitRouter;
export type SsspList = WasmSsspResult[];

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
  travelTimes: Float64Array;
  ssspList: SsspList;
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

export function runQuery(router: Router, params: RunQueryParams): QueryResult {
  const { sourceNode, mode, departureTime, date, nSamples, transferSlack, maxTime, prevSsspList } = params;

  // Free previous results before allocating new ones to avoid WASM OOM
  freeSsspList(prevSsspList);
  const numNodes = router.num_nodes();

  if (mode === 'single') {
    const sssp = router.run_tdd_full_for_date(sourceNode, departureTime, parseInt(date.replace(/-/g, '')), transferSlack, maxTime);
    const ssspList: SsspList = [sssp];
    const travelTimes = new Float64Array(numNodes);
    for (let i = 0; i < numNodes; i++) {
      const arr = router.node_arrival_time(sssp, i);
      travelTimes[i] = arr < 0xffffffff ? arr - departureTime : NaN;
    }
    return { travelTimes, ssspList };
  } else {
    const windowEnd = departureTime + 3600;
    const dateInt = parseInt(date.replace(/-/g, ''));
    const startSssp = router.run_tdd_full_for_date(sourceNode, departureTime, dateInt, transferSlack, maxTime);
    const endSssp = router.run_tdd_full_for_date(sourceNode, windowEnd, dateInt, transferSlack, maxTime);
    const sampledList = router.run_tdd_sampled_full_for_date(
      sourceNode, departureTime, windowEnd, nSamples, dateInt, transferSlack, maxTime
    );
    // Merge, deduplicating by exact departure time (keep first occurrence = boundary takes priority)
    const seenTimes = new Set<number>();
    const ssspList: SsspList = [];
    for (const sssp of [startSssp, ...sampledList, endSssp]) {
      const t = router.sssp_departure_time(sssp);
      if (!seenTimes.has(t)) {
        seenTimes.add(t);
        ssspList.push(sssp);
      } else {
        sssp.free();
      }
    }

    const sumTimes = new Float64Array(numNodes);
    const counts = new Uint32Array(numNodes);
    for (const sssp of ssspList) {
      const t = router.sssp_departure_time(sssp);
      for (let i = 0; i < numNodes; i++) {
        const arr = router.node_arrival_time(sssp, i);
        if (arr < 0xffffffff) {
          sumTimes[i] += arr - t;
          counts[i]++;
        }
      }
    }
    const travelTimes = new Float64Array(numNodes);
    for (let i = 0; i < numNodes; i++) {
      travelTimes[i] = counts[i] > 0 ? sumTimes[i] / counts[i] : NaN;
    }
    return { travelTimes, ssspList };
  }
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
      const segments = parsePathSegments(router, sssp, pathArray);
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

function parsePathSegments(router: Router, sssp: WasmSsspResult, pathArray: Uint32Array): PathSegment[] {
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
      const shapeCoords = router.route_shape_between(routeIdx, boardNode, endNode);
      if (shapeCoords.length >= 4) {
        finalCoords = [];
        for (let k = 0; k < shapeCoords.length; k += 2) {
          finalCoords.push([shapeCoords[k], shapeCoords[k + 1]]);
        }
      } else if (segments.length > 0) {
        const prev = segments[segments.length - 1];
        if (prev.coords.length > 0) {
          coords.unshift(prev.coords[prev.coords.length - 1]);
        }
      }
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
      coords: finalCoords,
    });
    i += 3;
  }
  return segments;
}
