import { useSyncExternalStore } from 'react';
import { getTravelTimesAt } from '../utils/router';

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
//    `useSyncExternalStore`, but time notifications are throttled to ~30 Hz
//    so a play loop can't trigger a render storm.
//  • Discrete events (play/pause/enter/exit) notify React immediately.
//
// This file is the single source of truth for the playhead; the app reducer
// holds no per-frame state.

// Departure-time granularity of an autoplay frame, in seconds. During playback
// the isochrone redraws only when the playhead crosses a 5-minute boundary; a
// manual scrub or hover instead renders the exact playhead time, rounded to the
// nearest second.
export const FRAME_STEP = 15;

// Wall-clock duration of a full-window playback pass.
const PLAYBACK_DURATION_MS = 25000;

// React time-label refresh interval (~30 fps). Governs only text; the thumb
// and WebGL frame are driven at the full rAF rate.
const TIME_THROTTLE_MS = 33;

export type AnimMode = 'average' | 'frame';

type FrameRenderer = (frame: Uint16Array) => void;

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

class AnimationStore {
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
  // Single-flight gate for the primary (playhead) frame fetch. A manual scrub
  // emits a distinct exact-second departure on every pointer move; without this
  // they would all queue on the serial WASM worker. Only one fetch runs at a
  // time — the latest target requested while busy is parked in `pendingPrimary`
  // and chased on resolve, so a fast drag costs ~2 fetches, not ~100.
  private primaryInflight = false;
  private pendingPrimary = -1;
  // Departure currently being fetched from the worker (-1 = none). Lets a
  // burst of rAF ticks parked on the same grid frame collapse to one request
  // instead of re-issuing it every tick.
  private inflightDep = -1;
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

  /**
   * Departure of the frame to render for the current playhead. Playback snaps
   * to the 5-minute grid so the map redraws only on boundary crossings; a
   * manual scrub or hover renders the exact playhead time, rounded to the
   * nearest second.
   */
  private depForCurrent(): number {
    return this.playing ? this.snapToFrame(this.currentTime) : Math.round(this.currentTime);
  }

  // ── Lifecycle ──

  /** Called when a query completes: a fresh profile is now in the worker. */
  setWindow(windowStart: number, windowEnd: number): void {
    this.windowStart = windowStart;
    this.windowEnd = windowEnd;
    this.currentTime = windowStart;
    this.throttledTime = windowStart;
    this.renderedDeparture = windowStart;
    this.lastRenderedDep = -1;
    this.primaryInflight = false;
    this.pendingPrimary = -1;
    this.inflightDep = -1;
    this.previewSaved = null;
    this.ready = true;
    this.mode = 'average';
    this.stopRaf();
    this.playing = false;
    this.notifyReact();
  }

  /** Called when the profile becomes unavailable (city change, new query). */
  reset(): void {
    this.stopRaf();
    this.ready = false;
    this.playing = false;
    this.mode = 'average';
    this.lastRenderedDep = -1;
    this.primaryInflight = false;
    this.pendingPrimary = -1;
    this.inflightDep = -1;
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
    this.currentTime = clamp(t, this.windowStart, this.windowEnd);
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
    this.currentTime = clamp(t, this.windowStart, this.windowEnd);
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

  /** Step the playhead by N×5min from the exact current time (arrow keys). */
  stepFrames(n: number): void {
    this.seek(this.currentTime + n * FRAME_STEP);
  }

  jumpToStart(): void {
    this.seek(this.windowStart);
  }

  jumpToEnd(): void {
    this.seek(this.windowEnd);
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
    const dep = this.depForCurrent();
    if (dep === this.lastRenderedDep && !force) return;
    // Every frame is fetched on demand from the worker. Stale-while-revalidate:
    // the previously rendered frame stays on screen (or the average view, if
    // nothing has rendered yet) until the worker returns this departure.
    this.requestFrame(dep);
  }

  private requestFrame(dep: number): void {
    // Already fetching this exact departure — a burst of rAF ticks parked on
    // the same grid frame collapses to one worker request.
    if (dep === this.inflightDep) return;
    // Single-flight: while a primary fetch runs, just remember the latest
    // target. A fast manual scrub emits dozens of distinct exact-second
    // departures; queuing them all would back up the serial worker. When the
    // in-flight fetch resolves we jump straight to wherever the scrub ended up.
    if (this.primaryInflight) {
      this.pendingPrimary = dep;
      return;
    }
    this.primaryInflight = true;
    this.inflightDep = dep;
    getTravelTimesAt(dep)
      .then((frame) => this.onPrimaryResolved(dep, frame))
      .catch(() => this.onPrimaryResolved(dep, null));
  }

  private onPrimaryResolved(dep: number, frame: Uint16Array | null): void {
    this.primaryInflight = false;
    this.inflightDep = -1;
    // Paint only if the playhead is still on this frame and still in frame mode
    // (null frame = worker error, or the profile was replaced — drop silently).
    if (frame && this.mode === 'frame' && this.frameRenderer && this.depForCurrent() === dep) {
      this.lastRenderedDep = dep;
      this.renderedDeparture = dep;
      this.frameRenderer(frame);
      this.notifyReact();
    }
    // Chase wherever the scrub moved while the worker was busy. Skip a target
    // already on screen so a scrub that lands back on the current frame
    // doesn't trigger a redundant refetch.
    const next = this.pendingPrimary;
    this.pendingPrimary = -1;
    if (next >= 0 && next !== this.lastRenderedDep && this.mode === 'frame') {
      this.requestFrame(next);
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
