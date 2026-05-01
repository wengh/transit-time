import type { WorkerResponse } from './router.worker';

// Rust-side display strings for a path (`PathDisplay`). Produced in Rust once
// per hover and consumed verbatim by HoverInfo — keeps the text the single
// source of truth so Rust tests can assert on it.
export interface PathDisplay {
  segmentLines: string[][];
  totalTimeLine: string;
}

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
  sampleCounts: Uint32Array;
  totalSamples: number;
  departureTime: number; // windowStart, kept for downstream consumers
  numThreads: number;
}

export interface RunQueryParams {
  sourceNode: number;
  windowStart: number;
  windowEnd: number;
  date: string;
  transferSlack: number;
  maxTime: number;
}

export interface HoverPath {
  segments: PathSegment[];
  totalTime: number | null;
  departureTime: number;
  routeColor: string;
  display: PathDisplay | null;
}

// ============================================================================
// Worker proxy — all WASM interaction happens in a dedicated web worker.
// The main thread communicates via postMessage and receives results + progress.
// ============================================================================

let worker: Worker | null = null;
let nextId = 0;
const pending = new Map<number, {
  resolve: (v: any) => void;
  reject: (e: Error) => void;
  onProgress?: (done: number, total: number) => void;
  onLoadProgress?: (pct: number) => void;
}>();

function getWorker(): Worker {
  if (!worker) {
    worker = new Worker(new URL('./router.worker.ts', import.meta.url), { type: 'module' });
    worker.onmessage = (e: MessageEvent<WorkerResponse>) => {
      const msg = e.data;
      const p = pending.get(msg.id);
      if (!p) return;
      if (msg.type === 'progress') {
        p.onProgress?.(msg.done, msg.total);
        return;
      }
      if (msg.type === 'loadProgress') {
        p.onLoadProgress?.(msg.progress);
        return;
      }
      pending.delete(msg.id);
      if (msg.type === 'error') {
        p.reject(new Error(msg.message));
      } else {
        p.resolve(msg.value);
      }
    };
  }
  return worker;
}

function call<T>(
  msg: Record<string, any>,
  opts?: { onProgress?: (done: number, total: number) => void; onLoadProgress?: (pct: number) => void },
): Promise<T> {
  const id = nextId++;
  const w = getWorker();
  return new Promise<T>((resolve, reject) => {
    pending.set(id, { resolve, reject, onProgress: opts?.onProgress, onLoadProgress: opts?.onLoadProgress });
    w.postMessage({ ...msg, id });
  });
}

// ── Public API ─────────────────────────────────────────────────────────────

export async function initWasm(): Promise<void> {
  await call({ type: 'initWasm' });
}

export async function loadRouter(
  cityFile: string,
  onProgress?: (pct: number) => void,
): Promise<{ nodeCoords: Float32Array; nodeCount: number; stopCount: number; routeColors: string[] }> {
  return call({ type: 'loadRouter', cityFile }, { onLoadProgress: onProgress });
}

export function freeProfile(): Promise<void> {
  return call({ type: 'freeProfile' });
}

// Cancel flag for the in-flight query. Main thread sets [0]=1 to request
// cancellation; the worker's progress callback reads it via Atomics.load.
let activeCancelBuf: Int32Array | null = null;

export async function runQuery(
  params: RunQueryParams,
  onProgress?: (done: number, total: number) => void,
): Promise<QueryResult> {
  // Signal cancellation to any in-flight query.
  if (activeCancelBuf) Atomics.store(activeCancelBuf, 0, 1);
  const sab = new SharedArrayBuffer(4);
  activeCancelBuf = new Int32Array(sab);
  return call({ type: 'runQuery', params, cancelBuf: sab }, { onProgress });
}

export async function getProfileHoverData(node: number): Promise<HoverPath[]> {
  return call({ type: 'getHoverData', node });
}

export async function snapToNode(lat: number, lon: number): Promise<number | null> {
  return call({ type: 'snapToNode', lat, lon });
}

export async function numPatternsForDate(date: number): Promise<number> {
  return call({ type: 'numPatternsForDate', date });
}
