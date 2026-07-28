import { useSyncExternalStore } from 'react';
import { getTravelTimesAt } from '../utils/router';

// Animation state lives outside React: the rAF loop pushes WebGL frames and
// moves the scrubber thumb at full rate; React-facing values are throttled
// via `useSyncExternalStore` so playback doesn't restorm renders.

/** Departure-grid step during autoplay (seconds). Scrub/hover use exact time. */
export const FRAME_STEP = 15;
const PLAYBACK_DURATION_MS = 25000;
/** React text-label refresh interval. Thumb and WebGL run at full rAF rate. */
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
  /** Live playhead, mutated every rAF tick. Read by `onRaf` subscribers (thumb). */
  private currentTime = 0;
  /** Throttled mirror of `currentTime` for React text. */
  private throttledTime = 0;
  /** Departure of the frame actually drawn — what the readout reports. */
  private renderedDeparture = 0;

  // ── Internal ──
  private frameRenderer: FrameRenderer | null = null;
  private lastRenderedDep = -1;
  // Single-flight gate for primary frame fetches. A fast scrub emits a fresh
  // exact-second target on every pointermove; without this they would all queue
  // on the serial worker. The latest target requested while busy is parked in
  // `pendingPrimary` and chased on resolve.
  //
  // This is a correctness gate, not just a throttle: the worker hands back a
  // `Uint16Array` view onto WASM memory that the *next* `travelTimesAt` call
  // overwrites in place. Exactly one request may be outstanding at a time, or a
  // response can be painted after its bytes have already been clobbered. So
  // `primaryInflight` is owned by the outstanding request and cleared only by
  // its own resolution — never by `reset`/`setWindow`, which use `epoch`.
  private primaryInflight = false;
  private pendingPrimary = -1;
  /** Departure currently in flight (-1 = none); collapses rAF bursts on one grid frame. */
  private inflightDep = -1;
  /**
   * Bumped whenever the worker's profile is replaced. A response tagged with a
   * stale epoch is discarded rather than painted: its departure is meaningless
   * against the new profile, and matching `depForCurrent()` by luck would draw
   * the wrong city's frame.
   */
  private epoch = 0;
  /** Saved {mode,currentTime} during sawtooth-chart hover; restored on exit. */
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

  /** Snap a continuous departure time to the nearest FRAME_STEP grid point. */
  snapToFrame(t: number): number {
    const last = this.lastFrameTime();
    const snapped = this.windowStart + Math.round((t - this.windowStart) / FRAME_STEP) * FRAME_STEP;
    return clamp(snapped, this.windowStart, last);
  }

  /**
   * Departure of the frame to render for the current playhead. Playback snaps
   * to the FRAME_STEP grid so the map redraws only on boundary crossings; a
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
    this.invalidateInflight();
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
    this.invalidateInflight();
    this.previewSaved = null;
    this.notifyReact();
  }

  /**
   * Retire every frame request belonging to the outgoing profile.
   *
   * Deliberately leaves `primaryInflight` set: a request already handed to the
   * worker still owns the shared WASM buffer until it resolves, so clearing the
   * gate here would let a second request start concurrently and overwrite the
   * bytes the first one is about to hand us. `inflightDep` *is* cleared, since
   * it is only a dedup key — without that, a new-profile request for the same
   * departure would be swallowed as a duplicate of the doomed old one.
   */
  private invalidateInflight(): void {
    this.epoch++;
    this.lastRenderedDep = -1;
    this.pendingPrimary = -1;
    this.inflightDep = -1;
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

  /** Step the playhead by N×FRAME_STEP from the exact current time (arrow keys). */
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
    // Stale-while-revalidate: previously rendered frame stays on screen until
    // the worker returns this one.
    this.requestFrame(dep);
  }

  private requestFrame(dep: number): void {
    if (dep === this.inflightDep) return;
    if (this.primaryInflight) {
      this.pendingPrimary = dep;
      return;
    }
    this.primaryInflight = true;
    this.inflightDep = dep;
    const epoch = this.epoch;
    getTravelTimesAt(dep)
      .then((frame) => this.onPrimaryResolved(epoch, dep, frame))
      .catch(() => this.onPrimaryResolved(epoch, dep, null));
  }

  private onPrimaryResolved(epoch: number, dep: number, frame: Uint16Array | null): void {
    // The request is done with the shared WASM buffer either way, so the gate
    // reopens even for a stale epoch — that response is simply not painted.
    this.primaryInflight = false;
    this.inflightDep = -1;
    // Paint only if this response is still relevant: same profile, still in
    // frame mode, and the playhead hasn't moved off this departure. A null
    // frame means the worker errored — drop silently.
    const current = epoch === this.epoch;
    if (
      frame &&
      current &&
      this.mode === 'frame' &&
      this.frameRenderer &&
      this.depForCurrent() === dep
    ) {
      this.lastRenderedDep = dep;
      this.renderedDeparture = dep;
      this.frameRenderer(frame);
      this.notifyReact();
    }
    // `pendingPrimary` is cleared by `invalidateInflight`, so anything parked
    // here belongs to the current epoch even when this response did not.
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

  // Throttle React time updates; trailing-edge timer delivers the final tick.
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

  // Sawtooth chart shows a committed (solid) and preview (dashed) line.
  // Returned as scalars so `useSyncExternalStore` sees stable snapshots; -1
  // means "no line".

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

/** Throttled (~30 fps) playhead time — for text labels and the chart highlight. */
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
