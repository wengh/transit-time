import { useSyncExternalStore } from 'react';
import { FrameCache } from '../utils/frameCache';

// ── Hybrid animation store ──────────────────────────────────────────────────
//
// The isochrone animation updates the playhead every requestAnimationFrame
// tick (~60 Hz). Routing that through React Context would re-render the whole
// tree 60×/s, so this store sits *outside* React:
//
//  • The rAF loop mutates `currentTime` and pushes frames straight to WebGL
//    via a registered renderer callback — zero React involvement.
//  • The scrubber thumb is moved imperatively by raw subscribers (`onRaf`),
//    also at the full frame rate.
//  • React-rendered text (time label, chart highlight) subscribes through
//    `useSyncExternalStore`, but time notifications are throttled to ~12 Hz
//    so a play loop can't trigger a render storm.
//  • Discrete events (play/pause/enter/exit) notify React immediately.
//
// This file is the single source of truth for the playhead; the app reducer
// holds no per-frame state.

// Departure-time granularity of a frame, in seconds. The isochrone redraws
// when the playhead crosses a 5-minute boundary; the thumb still moves
// continuously between them.
export const FRAME_STEP = 300;

// Wall-clock duration of a full-window playback pass.
const PLAYBACK_DURATION_MS = 25000;

// React time-label refresh interval (~12 fps). Governs only text; the thumb
// and WebGL frame are driven at the full rAF rate.
const TIME_THROTTLE_MS = 80;

// Speculative frames to prefetch ahead of the playhead.
const PREFETCH_AHEAD = 6;

// Prefetch governor: never let more than this many WASM requests be in flight
// at once, so a fast scrub can't build an unbounded request backlog.
const MAX_INFLIGHT = 2;

export type AnimMode = 'average' | 'frame';

type FrameRenderer = (frame: Uint16Array) => void;

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

class AnimationStore {
  private cache = new FrameCache();

  // ── Public-ish state (read via getters / hooks) ──
  private mode: AnimMode = 'average';
  private playing = false;
  private ready = false;
  private windowStart = 0;
  private windowEnd = 0;
  // The live playhead, mutated every rAF tick. Read imperatively by `onRaf`
  // subscribers (thumb) at full rate.
  private currentTime = 0;
  // Throttled mirror of `currentTime` for React text — only this is what
  // `useAnimTime` returns, so React renders at ~12 fps not 60.
  private throttledTime = 0;
  // Departure of the frame actually drawn on the map. The readout shows this
  // (not the scrubber target) so the label never claims a time the map isn't
  // yet showing under stale-while-revalidate.
  private renderedDeparture = 0;

  // ── Internal ──
  private frameRenderer: FrameRenderer | null = null;
  private lastRenderedDep = -1;
  // Hover-preview snapshot. Non-null while the sawtooth chart is being hovered
  // (not dragged): holds the committed {mode,currentTime} to restore on exit.
  private previewSaved: { mode: AnimMode; time: number } | null = null;
  private rafId = 0;
  private lastTickTime = 0;
  private throttleTimer: ReturnType<typeof setTimeout> | null = null;
  private lastReactNotify = 0;
  private reactSubs = new Set<() => void>();
  private rafSubs = new Set<() => void>();

  // ── Frame-grid math ──

  private lastFrameTime(): number {
    const span = this.windowEnd - this.windowStart;
    if (span <= 0) return this.windowStart;
    return this.windowStart + Math.floor(span / FRAME_STEP) * FRAME_STEP;
  }

  /** Snap a continuous departure time to the nearest 5-minute frame grid. */
  snapToFrame(t: number): number {
    const last = this.lastFrameTime();
    const snapped = this.windowStart + Math.round((t - this.windowStart) / FRAME_STEP) * FRAME_STEP;
    return clamp(snapped, this.windowStart, last);
  }

  // ── Lifecycle ──

  /** Called when a query completes: a fresh profile is now in the worker. */
  setWindow(windowStart: number, windowEnd: number): void {
    this.cache.invalidate();
    this.windowStart = windowStart;
    this.windowEnd = windowEnd;
    this.currentTime = windowStart;
    this.throttledTime = windowStart;
    this.renderedDeparture = windowStart;
    this.lastRenderedDep = -1;
    this.previewSaved = null;
    this.ready = true;
    this.mode = 'average';
    this.stopRaf();
    this.playing = false;
    this.notifyReact();
  }

  /** Called when the profile becomes unavailable (city change, new query). */
  reset(): void {
    this.cache.invalidate();
    this.stopRaf();
    this.ready = false;
    this.playing = false;
    this.mode = 'average';
    this.lastRenderedDep = -1;
    this.previewSaved = null;
    this.notifyReact();
  }

  // ── Controls ──

  exit(): void {
    if (this.mode === 'average') return;
    this.previewSaved = null;
    this.stopRaf();
    this.playing = false;
    this.mode = 'average';
    this.notifyReact();
  }

  play(): void {
    if (!this.ready) return;
    this.previewSaved = null;
    this.mode = 'frame';
    // Restart from the window start if the playhead is parked at the end.
    if (this.currentTime >= this.lastFrameTime()) {
      this.currentTime = this.windowStart;
    }
    this.playing = true;
    this.renderCurrentFrame(true);
    this.lastTickTime = performance.now();
    this.rafId = requestAnimationFrame(this.tick);
    this.notifyReact();
  }

  pause(): void {
    if (!this.playing) return;
    this.previewSaved = null;
    this.stopRaf();
    this.playing = false;
    this.throttledTime = this.currentTime;
    this.notifyReact();
  }

  togglePlay(): void {
    if (this.playing) this.pause();
    else this.play();
  }

  /** Commit the playhead to a departure time (click-seek, keyboard). */
  seek(t: number): void {
    if (!this.ready) return;
    this.previewSaved = null;
    this.mode = 'frame';
    this.currentTime = clamp(t, this.windowStart, this.lastFrameTime());
    this.throttledTime = this.currentTime;
    this.renderCurrentFrame(false);
    this.notifyRaf();
    this.notifyReact();
  }

  /**
   * Preview a departure time without committing it — used by hover on the
   * sawtooth chart. The first call snapshots the committed playhead; later
   * calls just move the preview. `clearPreview` restores the snapshot. While
   * playing, hover preview is ignored (the playhead is already moving).
   */
  setPreview(t: number): void {
    if (!this.ready || this.playing) return;
    if (this.previewSaved === null) {
      this.previewSaved = { mode: this.mode, time: this.currentTime };
    }
    this.mode = 'frame';
    this.currentTime = clamp(t, this.windowStart, this.lastFrameTime());
    this.throttledTime = this.currentTime;
    this.renderCurrentFrame(false);
    this.notifyReact();
  }

  /** Restore the committed playhead after a hover preview ends. */
  clearPreview(): void {
    if (this.previewSaved === null) return;
    const { mode, time } = this.previewSaved;
    this.previewSaved = null;
    this.mode = mode;
    this.currentTime = time;
    this.throttledTime = time;
    if (mode === 'frame') this.renderCurrentFrame(true);
    this.notifyReact();
  }

  /** Step the playhead by N frames (arrow keys). */
  stepFrames(n: number): void {
    this.seek(this.snapToFrame(this.currentTime) + n * FRAME_STEP);
  }

  jumpToStart(): void {
    this.seek(this.windowStart);
  }

  jumpToEnd(): void {
    this.seek(this.lastFrameTime());
  }

  // ── Map integration ──

  setFrameRenderer(fn: FrameRenderer | null): void {
    this.frameRenderer = fn;
  }

  /** Force a redraw of the current frame — e.g. after a map pan/zoom. */
  rerenderCurrentFrame(): void {
    if (this.mode === 'frame') this.renderCurrentFrame(true);
  }

  // ── rAF loop ──

  private tick = (now: number): void => {
    if (!this.playing) return;
    const dt = now - this.lastTickTime;
    this.lastTickTime = now;

    const rate = (this.windowEnd - this.windowStart) / PLAYBACK_DURATION_MS;
    const end = this.lastFrameTime();
    let t = this.currentTime + rate * dt;

    let reachedEnd = false;
    if (t >= end) {
      t = end;
      reachedEnd = true;
    }
    this.currentTime = t;

    this.renderCurrentFrame(false);
    this.notifyRaf();
    this.throttledNotify();

    if (reachedEnd) {
      this.pause();
      return;
    }
    this.rafId = requestAnimationFrame(this.tick);
  };

  private stopRaf(): void {
    if (this.rafId) {
      cancelAnimationFrame(this.rafId);
      this.rafId = 0;
    }
  }

  // ── Frame rendering ──

  private renderCurrentFrame(force: boolean): void {
    if (this.mode !== 'frame' || !this.frameRenderer) return;
    const dep = this.snapToFrame(this.currentTime);
    if (dep === this.lastRenderedDep && !force) return;

    const frame = this.cache.get(dep);
    if (frame) {
      this.lastRenderedDep = dep;
      this.renderedDeparture = dep;
      this.frameRenderer(frame);
    } else {
      // Stale-while-revalidate: the previously rendered frame stays on screen
      // (or the average view, if nothing has rendered yet) while we fetch.
      this.requestFrame(dep);
    }
    this.prefetch(dep);
  }

  private requestFrame(dep: number): void {
    this.cache
      .fetch(dep)
      .then((frame) => {
        if (!frame || this.mode !== 'frame' || !this.frameRenderer) return;
        // Only paint if the user is still on this frame.
        if (this.snapToFrame(this.currentTime) !== dep) return;
        this.lastRenderedDep = dep;
        this.renderedDeparture = dep;
        this.frameRenderer(frame);
        this.notifyReact();
      })
      .catch(() => {
        /* superseded query or worker error — drop silently */
      });
  }

  private prefetch(dep: number): void {
    const end = this.lastFrameTime();
    for (let k = 1; k <= PREFETCH_AHEAD; k++) {
      if (this.cache.inflightCount >= MAX_INFLIGHT) break;
      const next = dep + k * FRAME_STEP;
      if (next > end) break;
      if (this.cache.has(next)) continue;
      void this.cache.fetch(next).catch(() => {});
    }
  }

  // ── Subscriptions ──

  private notifyReact(): void {
    for (const cb of this.reactSubs) cb();
  }

  private notifyRaf(): void {
    for (const cb of this.rafSubs) cb();
  }

  // Throttle React time updates to ~12 fps. Trailing-edge timer guarantees the
  // final position is delivered even if ticks stop between throttle windows.
  private throttledNotify(): void {
    const now = performance.now();
    const elapsed = now - this.lastReactNotify;
    if (elapsed >= TIME_THROTTLE_MS) {
      this.lastReactNotify = now;
      this.throttledTime = this.currentTime;
      this.renderedDeparture = this.snapToFrame(this.currentTime);
      this.notifyReact();
    } else if (!this.throttleTimer) {
      this.throttleTimer = setTimeout(() => {
        this.throttleTimer = null;
        this.lastReactNotify = performance.now();
        this.throttledTime = this.currentTime;
        this.notifyReact();
      }, TIME_THROTTLE_MS - elapsed);
    }
  }

  /** Register a full-rate (60 fps) listener — used by the scrubber thumb. */
  onRaf(cb: () => void): () => void {
    this.rafSubs.add(cb);
    return () => this.rafSubs.delete(cb);
  }

  // Stable bound subscribe/getters for useSyncExternalStore.
  subscribe = (cb: () => void): (() => void) => {
    this.reactSubs.add(cb);
    return () => this.reactSubs.delete(cb);
  };

  getMode = (): AnimMode => this.mode;
  isPlaying = (): boolean => this.playing;
  isReady = (): boolean => this.ready;
  getTime = (): number => this.throttledTime;
  getLiveTime = (): number => this.currentTime;
  getRenderedDeparture = (): number => this.renderedDeparture;
  getWindowStart = (): number => this.windowStart;
  getWindowEnd = (): number => this.windowEnd;

  // ── Playhead, split into committed vs. preview ──
  //
  // The sawtooth chart draws two vertical lines: a solid one for the committed
  // departure (where the map will return on hover-end) and a dashed one for the
  // live hover preview. Both are exposed as plain numbers (not an object) so
  // `useSyncExternalStore` sees a stable snapshot — a fresh object every call
  // would loop forever. A value of -1 means "no line".

  /** Committed playhead: the departure that survives a hover-preview exit. */
  getCommittedPlayhead = (): number => {
    if (this.previewSaved !== null) {
      return this.previewSaved.mode === 'frame' ? this.previewSaved.time : -1;
    }
    return this.mode === 'frame' ? this.throttledTime : -1;
  };

  /** Preview playhead: the live hovered departure, or -1 when not hovering. */
  getPreviewPlayhead = (): number => (this.previewSaved !== null ? this.throttledTime : -1);
}

export const animationStore = new AnimationStore();

// ── React hooks ─────────────────────────────────────────────────────────────

export function useAnimMode(): AnimMode {
  return useSyncExternalStore(animationStore.subscribe, animationStore.getMode);
}

export function useAnimPlaying(): boolean {
  return useSyncExternalStore(animationStore.subscribe, animationStore.isPlaying);
}

export function useAnimReady(): boolean {
  return useSyncExternalStore(animationStore.subscribe, animationStore.isReady);
}

/** Throttled (~12 fps) playhead time — for text labels and the chart highlight. */
export function useAnimTime(): number {
  return useSyncExternalStore(animationStore.subscribe, animationStore.getTime);
}

/** Departure of the frame currently drawn on the map. */
export function useAnimRenderedDeparture(): number {
  return useSyncExternalStore(animationStore.subscribe, animationStore.getRenderedDeparture);
}

/** Committed playhead departure (solid chart line), or -1 when in average mode. */
export function useAnimCommittedPlayhead(): number {
  return useSyncExternalStore(animationStore.subscribe, animationStore.getCommittedPlayhead);
}

/** Live hover-preview departure (dashed chart line), or -1 when not hovering. */
export function useAnimPreviewPlayhead(): number {
  return useSyncExternalStore(animationStore.subscribe, animationStore.getPreviewPlayhead);
}
