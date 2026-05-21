import { getTravelTimesAt } from './router';

// LRU cache of per-departure isochrone frames. A "frame" is the Uint16Array
// returned by `travel_times_at` — one travel-time value per node, with 65535
// (u16::MAX) marking unreachable nodes.
//
// The cache bounds memory regardless of how long the departure window is:
// only the MAX_FRAMES most-recently-used frames are kept. A 50k-node city is
// ~100 KB/frame, so MAX_FRAMES=96 caps the cache near 10 MB.
//
// `inflightCount` lets the caller act as a prefetch governor: it should stop
// issuing speculative `fetch` calls once that count reaches its budget, so a
// fast scrub can't flood the single WASM worker with a backlog of requests
// the user has already scrubbed past.

const MAX_FRAMES = 96;

export class FrameCache {
  // Map iteration order is insertion order; re-inserting on read turns it
  // into an LRU recency list (front = oldest, back = newest).
  private cache = new Map<number, Uint16Array>();
  private inflight = new Map<number, Promise<Uint16Array | null>>();
  // Bumped by `invalidate`. A fetch started before the bump discards its
  // result instead of populating a cache that now belongs to a new query.
  private generation = 0;

  /** Synchronous cache hit, or null. Refreshes LRU recency on hit. */
  get(departure: number): Uint16Array | null {
    const v = this.cache.get(departure);
    if (v === undefined) return null;
    this.cache.delete(departure);
    this.cache.set(departure, v);
    return v;
  }

  has(departure: number): boolean {
    return this.cache.has(departure);
  }

  /** Number of WASM requests currently in flight (prefetch-governor input). */
  get inflightCount(): number {
    return this.inflight.size;
  }

  /**
   * Fetch a frame from the worker, populating the cache. Concurrent requests
   * for the same departure are coalesced. Resolves to null if the cache was
   * invalidated (new query) while the request was in flight.
   */
  fetch(departure: number): Promise<Uint16Array | null> {
    const hit = this.get(departure);
    if (hit) return Promise.resolve(hit);

    const existing = this.inflight.get(departure);
    if (existing) return existing;

    const gen = this.generation;
    const p = getTravelTimesAt(departure)
      .then((frame): Uint16Array | null => {
        this.inflight.delete(departure);
        if (gen !== this.generation) return null; // superseded by a new query
        this.put(departure, frame);
        return frame;
      })
      .catch((e) => {
        this.inflight.delete(departure);
        throw e;
      });
    this.inflight.set(departure, p);
    return p;
  }

  private put(departure: number, frame: Uint16Array): void {
    this.cache.set(departure, frame);
    while (this.cache.size > MAX_FRAMES) {
      const oldest = this.cache.keys().next().value;
      if (oldest === undefined) break;
      this.cache.delete(oldest);
    }
  }

  /**
   * Drop every cached and in-flight frame. Call on any change that makes the
   * worker's profile stale (new query, parameter change, city change).
   */
  invalidate(): void {
    this.cache.clear();
    this.inflight.clear();
    this.generation++;
  }
}
