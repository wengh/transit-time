import React, { useCallback, useEffect, useRef } from 'react';
import {
  animationStore,
  useAnimMode,
  useAnimPlaying,
  useAnimReady,
  useAnimRenderedDeparture,
} from '../state/animationStore';
import { formatTime } from '../utils/format';

// Frames skipped by a Shift+Arrow jump (30 min at FRAME_STEP=300).
const JUMP_FRAMES = 6;

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

// Always-visible playback bar that appears once a query has produced a
// profile. It is the scrubber half of the unified timeline — the sawtooth
// chart is the other half, and both seek the same AnimationStore playhead.
//
// The thumb is positioned imperatively (style.left) from an `onRaf` callback
// so a 60 Hz play loop never re-renders React. Only discrete state — mode,
// playing, ready, the rendered-frame readout — flows through hooks.
export default function Timeline(): React.ReactNode {
  const ready = useAnimReady();
  const mode = useAnimMode();
  const playing = useAnimPlaying();
  const renderedDep = useAnimRenderedDeparture();

  const trackRef = useRef<HTMLDivElement>(null);
  const thumbRef = useRef<HTMLDivElement>(null);

  // Push the live playhead position onto the thumb without a React render.
  const placeThumb = useCallback(() => {
    const thumb = thumbRef.current;
    if (!thumb) return;
    const ws = animationStore.getWindowStart();
    const we = animationStore.getWindowEnd();
    const span = we - ws;
    if (span <= 0) return;
    const frac = clamp((animationStore.getLiveTime() - ws) / span, 0, 1);
    thumb.style.left = `${frac * 100}%`;
  }, []);

  // Full-rate (60 Hz) subscription: keeps the thumb glued to the playhead
  // during playback and drag-seeks.
  useEffect(() => animationStore.onRaf(placeThumb), [placeThumb]);
  // Re-place after every discrete React update (window armed, seek, mode flip).
  useEffect(placeThumb);

  // Global keyboard transport. Mounted once; guarded so typing in a form
  // field never hijacks the spacebar or arrow keys.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (!animationStore.isReady()) return;
      const tag = (e.target as HTMLElement).tagName;
      if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA') return;

      switch (e.key) {
        case ' ':
          e.preventDefault();
          animationStore.togglePlay();
          break;
        case 'ArrowLeft':
          e.preventDefault();
          animationStore.stepFrames(e.shiftKey ? -JUMP_FRAMES : -1);
          break;
        case 'ArrowRight':
          e.preventDefault();
          animationStore.stepFrames(e.shiftKey ? JUMP_FRAMES : 1);
          break;
        case 'Home':
          e.preventDefault();
          animationStore.jumpToStart();
          break;
        case 'End':
          e.preventDefault();
          animationStore.jumpToEnd();
          break;
      }
    }
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, []);

  // Map a pointer's clientX over the track to a departure time and seek there.
  const seekToClientX = useCallback((clientX: number) => {
    const track = trackRef.current;
    if (!track) return;
    const rect = track.getBoundingClientRect();
    const ws = animationStore.getWindowStart();
    const we = animationStore.getWindowEnd();
    const frac = clamp((clientX - rect.left) / rect.width, 0, 1);
    animationStore.seek(ws + frac * (we - ws));
  }, []);

  const handlePointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      e.currentTarget.setPointerCapture(e.pointerId);
      seekToClientX(e.clientX);
    },
    [seekToClientX]
  );

  const handlePointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (e.currentTarget.hasPointerCapture(e.pointerId)) seekToClientX(e.clientX);
    },
    [seekToClientX]
  );

  if (!ready) return null;

  const windowStart = animationStore.getWindowStart();
  const windowEnd = animationStore.getWindowEnd();
  const frameActive = mode === 'frame';

  return (
    <div
      className="absolute z-[1000] bottom-5 left-1/2 -translate-x-1/2
        max-sm:bottom-2
        w-[min(560px,calc(100vw-1.25rem))]
        flex items-center gap-3 px-3 py-2 rounded-lg
        bg-zinc-900/95 dark:bg-zinc-900/95
        [@media(prefers-color-scheme:light)]:bg-white/95
        shadow-[0_2px_12px_rgba(0,0,0,0.5)]
        border border-zinc-700 dark:border-zinc-700
        [@media(prefers-color-scheme:light)]:border-zinc-200"
    >
      <button
        onClick={() => animationStore.togglePlay()}
        aria-label={playing ? 'Pause' : 'Play'}
        title={playing ? 'Pause (space)' : 'Play (space)'}
        className="shrink-0 w-7 h-7 flex items-center justify-center rounded
          text-zinc-200 dark:text-zinc-200
          [@media(prefers-color-scheme:light)]:text-zinc-700
          hover:bg-zinc-700 dark:hover:bg-zinc-700
          [@media(prefers-color-scheme:light)]:hover:bg-zinc-100
          cursor-pointer text-sm leading-none"
      >
        {playing ? '❚❚' : '▶'}
      </button>

      <div
        ref={trackRef}
        role="slider"
        tabIndex={0}
        aria-label="Departure time"
        aria-valuemin={windowStart}
        aria-valuemax={windowEnd}
        aria-valuenow={renderedDep}
        aria-valuetext={formatTime(renderedDep)}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        className="relative flex-1 h-5 cursor-pointer touch-none select-none"
      >
        <div
          className="absolute top-1/2 left-0 right-0 h-[3px] -translate-y-1/2 rounded
            bg-zinc-600 dark:bg-zinc-600
            [@media(prefers-color-scheme:light)]:bg-zinc-300 pointer-events-none"
        />
        <div
          ref={thumbRef}
          className="absolute top-1/2 w-[13px] h-[13px] rounded-full
            -translate-x-1/2 -translate-y-1/2 pointer-events-none
            bg-blue-500 border-2 border-white shadow"
          style={{ left: '0%' }}
        />
      </div>

      <span
        className="shrink-0 tabular-nums text-[13px] w-[44px] text-center
          text-zinc-200 dark:text-zinc-200
          [@media(prefers-color-scheme:light)]:text-zinc-700"
      >
        {frameActive ? formatTime(renderedDep) : 'Avg'}
      </span>

      {frameActive && (
        <button
          onClick={() => animationStore.exit()}
          aria-label="Exit playback, show window average"
          title="Show window average"
          className="shrink-0 w-7 h-7 flex items-center justify-center rounded
            text-zinc-400 dark:text-zinc-400
            [@media(prefers-color-scheme:light)]:text-zinc-500
            hover:bg-zinc-700 dark:hover:bg-zinc-700
            [@media(prefers-color-scheme:light)]:hover:bg-zinc-100
            hover:text-zinc-200 dark:hover:text-zinc-200
            [@media(prefers-color-scheme:light)]:hover:text-zinc-700
            cursor-pointer text-sm leading-none"
        >
          ✕
        </button>
      )}
    </div>
  );
}
