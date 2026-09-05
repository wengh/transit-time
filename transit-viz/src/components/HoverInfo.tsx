import React, { useEffect, useRef, useState, useCallback, useId } from 'react';
import { useAppState } from '../state/AppContext';
import type { HoverPath } from '../utils/router';
import { currentDest, type HoverData } from '../state/reducer';
import { getMedianPath } from '../utils/hoverInfo';
import { formatTime } from '../utils/format';
import { formatDistance, haversineKm } from '../utils/geo';
import {
  animationStore,
  useAnimMode,
  useAnimTime,
  useAnimRenderedDeparture,
  useAnimPlaying,
  useAnimReady,
  useAnimCommittedPlayhead,
  useAnimPreviewPlayhead,
} from '../state/animationStore';
import PathSegmentList from './PathSegmentList';

// ─── chart data types ────────────────────────────────────────────────────────

interface ChartTip {
  tipX: number; // absolute departure time when you just catch this trip (seconds)
  tipY: number; // travel time if you just catch it (seconds)
  pathIdx: number; // index into allPaths for the representative path for this trip
  color: string;
}

interface ChartInfo {
  tips: ChartTip[];
  walkTime: number | null;
  walkPathIdx: number | null;
  windowStart: number;
  windowEnd: number;
  yMax: number;
}

// ─── chart computation ────────────────────────────────────────────────────────

function computeChartInfo(
  allPaths: HoverPath[],
  windowStart: number,
  windowEnd: number,
  maxTimeSec: number
): ChartInfo {
  let walkTime: number | null = null;
  let walkPathIdx: number | null = null;
  const rawTips: Array<ChartTip> = [];

  for (let i = 0; i < allPaths.length; i++) {
    const p = allPaths[i];
    if (p.totalTime === null) continue;

    const isWalkOnly = p.segments.length > 0 && p.segments.every((s) => s.edgeType === 0);
    if (isWalkOnly) {
      if (walkTime === null || p.totalTime < walkTime) {
        walkTime = p.totalTime;
        walkPathIdx = i;
      }
      continue;
    }

    const firstTransit = p.segments.find((s) => s.edgeType === 1);
    if (!firstTransit) continue;

    const w = firstTransit.waitTime;
    const tipX = p.departureTime + w;
    const tipY = p.totalTime - w;
    if (tipY < 0) continue;

    // No arrival-time dedup: Pareto dominance in the Rust profile router
    // already guarantees unique (arrival, home_departure) pairs. If two
    // entries collide here, that's a bug in the Rust filter — surface it
    // rather than masking it in the chart.
    rawTips.push({ tipX, tipY, pathIdx: i, color: p.routeColor });
  }

  const tips: ChartTip[] = rawTips.sort((a, b) => a.tipX - b.tipX);

  const yMax = maxTimeSec;
  return { tips, walkTime, walkPathIdx, windowStart, windowEnd, yMax };
}

// ─── chart drawing ────────────────────────────────────────────────────────────

const PAD = { top: 8, right: 8, bottom: 22, left: 34 };

interface ChartTheme {
  bg: string;
  unreachable: string;
  grid: string;
  axis: string;
  label: string;
  walkLine: string;
  walkLineSelected: string;
  selectionRing: string;
  playhead: string;
  playheadPreview: string;
}

const DARK_THEME: ChartTheme = {
  bg: '#1e1e1e',
  unreachable: 'rgba(80,80,100,0.22)',
  grid: '#2a2a2a',
  axis: '#3a3a3a',
  label: '#888',
  walkLine: '#555',
  walkLineSelected: '#ccc',
  selectionRing: '#ddd',
  playhead: '#60a5fa',
  playheadPreview: '#fbbf24',
};

const LIGHT_THEME: ChartTheme = {
  bg: '#ffffff',
  unreachable: 'rgba(100,100,120,0.1)',
  grid: '#e5e7eb',
  axis: '#d1d5db',
  label: '#6b7280',
  walkLine: '#9ca3af',
  walkLineSelected: '#374151',
  selectionRing: '#374151',
  playhead: '#2563eb',
  playheadPreview: '#d97706',
};

// Returns a counter that increments whenever `ref.current` is resized. List it
// as a useEffect dep to re-fire (e.g. redraw a canvas) on layout changes —
// window resize, panel expand/collapse, parent flex reflow, etc.
function useResizeTick(ref: React.RefObject<Element | null>): number {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const obs = new ResizeObserver(() => setTick((t) => t + 1));
    obs.observe(el);
    return () => obs.disconnect();
  }, [ref]);
  return tick;
}

// Subscribes to OS-level color-scheme changes so canvas raster contents (which
// don't auto-restyle like CSS) can be redrawn. Returns the current dark flag;
// updating it bumps any effect that lists it as a dep.
function usePrefersDark(): boolean {
  const [dark, setDark] = useState(() => window.matchMedia('(prefers-color-scheme: dark)').matches);
  useEffect(() => {
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const onChange = (e: MediaQueryListEvent) => setDark(e.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);
  return dark;
}

function yTickStep(yMaxSec: number): number {
  const maxMin = yMaxSec / 60;
  for (const step of [5, 10, 15, 20, 30, 60]) {
    if (maxMin / step <= 7) return step * 60;
  }
  return Math.ceil(maxMin / 7) * 60;
}

function drawChart(
  canvas: HTMLCanvasElement,
  info: ChartInfo,
  selectedIdx: number | null,
  theme: ChartTheme,
  committedTime: number,
  previewTime: number,
  hasData: boolean
): void {
  const rect = canvas.getBoundingClientRect();
  const size = Math.round(rect.width);
  const height = Math.round(rect.height);
  if (size === 0 || height === 0) return;
  // High-DPI: allocate the backing store at physical pixel density and scale
  // the drawing context, so 1px in our coords maps to 1 CSS px (= dpr device
  // pixels). Without this, lines and text are bilinearly upscaled by the
  // browser on retina displays — the classic "blurry canvas" look.
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(size * dpr);
  canvas.height = Math.round(height * dpr);
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

  const { tips, walkTime, walkPathIdx, windowStart, windowEnd, yMax } = info;
  const W = size,
    H = height;
  const { top: pT, right: pR, bottom: pB, left: pL } = PAD;
  // The left gutter is constant whether or not there's a destination: with no
  // y-axis it simply stays empty. Keeping pL fixed means the plot region — and
  // thus the time→x mapping — is identical in both states, so the playhead and
  // x-axis ticks never shift sideways when a destination is pinned or cleared.
  const plotW = W - pL - pR;
  const plotH = H - pT - pB;
  const clipY = walkTime !== null ? Math.min(walkTime, yMax) : yMax;

  const xToC = (t: number) => pL + ((t - windowStart) / (windowEnd - windowStart)) * plotW;
  const yToC = (y: number) => pT + plotH - (y / yMax) * plotH;

  // Background
  ctx.fillStyle = theme.bg;
  ctx.fillRect(0, 0, W, H);

  // Unreachable zones: only shade when there is no walk path (if walking works, nowhere
  // is truly unreachable). Use yMax (not walkTime) as the threshold so "transit slower
  // than walking" zones are not marked unreachable — the dashed walk line covers those.
  // Skipped with no destination, else the whole empty plot would shade grey.
  if (hasData && (walkTime === null || walkTime > yMax)) {
    ctx.fillStyle = theme.unreachable;
    const reachable: [number, number][] = [];
    for (const { tipX, tipY } of tips) {
      if (tipY > yMax) continue;
      reachable.push([tipX - (yMax - tipY), tipX]);
    }
    const shadeGrey = (t0: number, t1: number) => {
      if (t1 <= t0) return;
      const x0 = Math.max(pL, xToC(t0));
      const x1 = Math.min(pL + plotW, xToC(t1));
      if (x1 > x0) ctx.fillRect(x0, pT, x1 - x0, plotH);
    };
    let cursor = windowStart;
    for (const [rStart, rEnd] of reachable) {
      shadeGrey(cursor, rStart);
      cursor = Math.max(cursor, rEnd);
    }
    shadeGrey(cursor, windowEnd);
  }

  // Grid
  ctx.strokeStyle = theme.grid;
  ctx.lineWidth = 1;
  const windowDurMin = (windowEnd - windowStart) / 60;
  // Pick x-axis tick step. Density adapts to plot width: ~40px per HH:MM label
  // (5 chars at 9-11px font), ~28px for "+NN" offset labels. Both leave enough
  // gap to avoid touching at the typical font size, while still letting the
  // expanded full-width plot show many more ticks than the collapsed panel.
  const minTickSpacingPx = windowDurMin > 120 ? 40 : 28;
  const maxTicks = Math.max(2, Math.floor(plotW / minTickSpacingPx));
  let xStepMin = 720;
  for (const s of [1, 2, 5, 10, 15, 30, 60, 120, 180, 240, 360, 480, 720]) {
    if (windowDurMin / s <= maxTicks) {
      xStepMin = s;
      break;
    }
  }
  for (let min = 0; min <= windowDurMin; min += xStepMin) {
    const x = xToC(windowStart + min * 60);
    ctx.beginPath();
    ctx.moveTo(x, pT);
    ctx.lineTo(x, pT + plotH);
    ctx.stroke();
  }
  const step = yTickStep(yMax);
  if (hasData) {
    for (let y = 0; y <= yMax; y += step) {
      const cy = yToC(y);
      ctx.beginPath();
      ctx.moveTo(pL, cy);
      ctx.lineTo(pL + plotW, cy);
      ctx.stroke();
    }
  }

  // Axes — y-axis line only when there's a destination to scale it to.
  ctx.strokeStyle = theme.axis;
  ctx.lineWidth = 1;
  ctx.beginPath();
  if (hasData) {
    ctx.moveTo(pL, pT);
    ctx.lineTo(pL, pT + plotH);
    ctx.lineTo(pL + plotW, pT + plotH);
  } else {
    ctx.moveTo(pL, pT + plotH);
    ctx.lineTo(pL + plotW, pT + plotH);
  }
  ctx.stroke();

  // X-axis labels
  ctx.fillStyle = theme.label;
  // Base label size on the shorter dimension so a wide-but-short chart (when
  // the panel is expanded to full width) doesn't get giant 60px-ish labels.
  ctx.font = `${Math.max(9, Math.round(Math.min(W, H) / 28))}px sans-serif`;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'alphabetic';
  for (let min = 0; min <= windowDurMin; min += xStepMin) {
    const x = xToC(windowStart + min * 60);
    // Show absolute time (HH:MM) for windows > 2h, offset otherwise
    let label: string;
    if (windowDurMin > 120) {
      const totalSec = windowStart + min * 60;
      label = formatTime(totalSec);
    } else {
      label = `+${min}`;
    }
    ctx.fillText(label, x, H - 4);
  }

  // Y-axis labels (minutes) — only with a destination.
  if (hasData) {
    ctx.textAlign = 'right';
    ctx.textBaseline = 'middle';
    for (let y = 0; y <= yMax; y += step) {
      const cy = yToC(y);
      ctx.fillText(y === 0 ? '0' : `${Math.round(y / 60)}m`, pL - 3, cy);
    }
  }

  // Walk line (dashed gray, drawn behind transit lines)
  if (walkTime !== null) {
    const cy = yToC(walkTime);
    const isSelected = walkPathIdx !== null && selectedIdx === walkPathIdx;
    ctx.strokeStyle = isSelected ? theme.walkLineSelected : theme.walkLine;
    ctx.lineWidth = isSelected ? 2 : 1.5;
    ctx.setLineDash([4, 6]);
    ctx.beginPath();
    ctx.moveTo(pL, cy);
    ctx.lineTo(pL + plotW, cy);
    ctx.stroke();
    ctx.setLineDash([]);
  }

  // Transit trip segments (sawtooth / triangle shapes)
  for (let i = 0; i < tips.length; i++) {
    const { tipX, tipY, pathIdx, color } = tips[i];
    if (tipY > clipY) continue;

    const prevBoundX = i === 0 ? windowStart : tips[i - 1].tipX;
    let segStartX = prevBoundX;
    let segStartY = tipY + (tipX - segStartX);

    // Clip top to clipY — the diagonal starts where it crosses clipY
    if (segStartY > clipY) {
      segStartX = tipX - (clipY - tipY);
      segStartY = clipY;
    }
    if (segStartX > tipX) continue;

    const isSelected = selectedIdx === pathIdx;
    const DENSE_SLOPE_THRESHOLD = 4; // When slope exceeds this, switch to thinner line and hide tip dot
    const dense = (windowEnd - windowStart) / yMax > DENSE_SLOPE_THRESHOLD;
    ctx.strokeStyle = color;
    ctx.lineWidth = isSelected ? 3.5 : dense ? 1.5 : 2;

    // Diagonal from (segStartX, segStartY) down to tip — no horizontal cap
    ctx.beginPath();
    ctx.moveTo(xToC(segStartX), yToC(segStartY));
    ctx.lineTo(xToC(tipX), yToC(tipY));
    ctx.stroke();

    // Dot at tip (hidden in dense mode)
    if (!dense || isSelected) {
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.arc(xToC(tipX), yToC(tipY), isSelected ? 4.5 : 3, 0, Math.PI * 2);
      ctx.fill();
    }
  }

  // Selection highlight ring around the tip dot
  if (selectedIdx !== null) {
    const tip = tips.find((t) => t.pathIdx === selectedIdx);
    if (tip && tip.tipY <= yMax) {
      ctx.strokeStyle = theme.selectionRing;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.arc(xToC(tip.tipX), yToC(tip.tipY), 7, 0, Math.PI * 2);
      ctx.stroke();
    }
  }

  // Playhead lines: the chart doubles as a scrubber. The committed line (solid)
  // is the departure the map returns to when a hover ends; the preview line
  // (dashed amber) tracks the live hovered departure. Splitting them keeps a
  // transient hover from visually clobbering the pinned time. t < 0 means none.
  const drawPlayhead = (t: number, color: string, dashed: boolean) => {
    if (t < 0 || t < windowStart || t > windowEnd) return;
    const px = xToC(t);
    ctx.strokeStyle = color;
    ctx.lineWidth = 1.5;
    ctx.setLineDash(dashed ? [3, 3] : []);
    ctx.beginPath();
    ctx.moveTo(px, pT);
    ctx.lineTo(px, pT + plotH);
    ctx.stroke();
    ctx.setLineDash([]);
  };
  drawPlayhead(committedTime, theme.playhead, false);
  drawPlayhead(previewTime, theme.playheadPreview, true);
}

// ─── time ↔ x-position ↔ path index ──────────────────────────────────────────

/** Map a canvas x-pixel to the departure time it represents on the chart. */
function timeAtCanvasX(canvasX: number, canvasWidth: number, info: ChartInfo): number {
  const plotW = canvasWidth - PAD.left - PAD.right;
  const frac = (canvasX - PAD.left) / plotW;
  return info.windowStart + frac * (info.windowEnd - info.windowStart);
}

/** Which path in `allPaths` is optimal when departing at time `t`. */
function pathIdxAtTime(t: number, info: ChartInfo): number | null {
  const { tips, walkPathIdx, windowStart, yMax, walkTime } = info;
  const clipY = walkTime !== null ? Math.min(walkTime, yMax) : yMax;

  for (let i = 0; i < tips.length; i++) {
    const leftBound = i === 0 ? windowStart : tips[i - 1].tipX;
    const { tipX, tipY } = tips[i];
    if (t >= leftBound && t <= tipX) {
      // Entire trip is slower than walk/maxTime, or departure is in the grey zone
      if (tipY > clipY || t < tipX - (clipY - tipY)) return walkPathIdx;
      return tips[i].pathIdx;
    }
  }
  return walkPathIdx;
}

// ─── hint button ──────────────────────────────────────────────────────────────

function ChartHintButton(): React.ReactNode {
  const [open, setOpen] = useState(false);
  const id = useId();
  return (
    // `flex` (not just `block`) so the wrapper hugs the 18px button instead of
    // inheriting the parent's line-height, which adds phantom vertical space.
    <div className="relative flex">
      <button
        aria-label="How to read this chart"
        aria-expanded={open}
        aria-controls={id}
        onClick={() => setOpen((v) => !v)}
        className="flex-shrink-0 w-[18px] h-[18px] text-[11px] leading-[16px] cursor-pointer
          rounded-full p-0
          bg-transparent border border-zinc-600 text-zinc-500
          dark:border-zinc-600 dark:text-zinc-500"
      >
        ?
      </button>
      {open && (
        <div
          id={id}
          role="tooltip"
          className="absolute bottom-[22px] right-0 z-10
            bg-white dark:bg-zinc-800
            border border-zinc-300 dark:border-zinc-700
            rounded-md p-2 w-[220px] text-[11px] leading-relaxed
            text-zinc-700 dark:text-zinc-300
            shadow-[0_2px_8px_rgba(0,0,0,.15)] dark:shadow-[0_2px_8px_rgba(0,0,0,.4)]"
        >
          <strong className="block mb-1">How to read this chart</strong>
          <p className="m-0 mb-1">
            <strong>X-axis:</strong> departure time. <strong>Y-axis:</strong> travel time to this
            location.
          </p>
          <p className="m-0 mb-1">
            Each <strong>sawtooth curve</strong> is one transit trip — travel time rises as you
            depart later and miss the vehicle, then drops when you catch the next one.
          </p>
          <p className="m-0">
            <strong>Hover</strong> to highlight a departure. <strong>Click</strong> to pin it and
            see its route on the map.
          </p>
        </div>
      )}
    </div>
  );
}

// Toggles the desktop "wide mode" (see HoverInfo's `expanded` state). Sits on
// the chart's bottom-right corner beside the hint button. Extracted so both the
// full panel and the no-destination strip can mount it — the scrubber should be
// widenable whether or not a destination is picked.
function ChartExpandButton({
  expanded,
  onToggle,
}: {
  expanded: boolean;
  onToggle: () => void;
}): React.ReactNode {
  return (
    <button
      type="button"
      aria-label={expanded ? 'Shrink chart width' : 'Expand chart to full width'}
      aria-pressed={expanded}
      title={expanded ? 'Shrink chart width' : 'Expand chart to full width'}
      onClick={onToggle}
      className="hidden sm:block
        flex-shrink-0 w-[18px] h-[18px] text-[11px] leading-[16px] cursor-pointer
        rounded-full p-0
        bg-transparent border border-zinc-600 text-zinc-500
        dark:border-zinc-600 dark:text-zinc-500"
    >
      ⛶
    </button>
  );
}

// ─── component ────────────────────────────────────────────────────────────────

interface HoverInfoProps {
  isFront: boolean;
  onActivate: () => void;
}

// ── Shared helpers + components reused by the mobile bottom sheet ──────────

// Resolve the path to show in the detail panel. With no departure time chosen
// (average view) this is the representative/median path; with one chosen it is
// the path optimal for that departure — found by replaying the chart's
// time→path-index mapping. Unlike the old per-sample view, the leading wait is
// *kept*: when you pick a clock time, the wait until the vehicle arrives is
// real time you'd spend, so it belongs in the trip.
export function deriveDisplayPath(
  hoverData: HoverData,
  departureTime: number | null,
  windowStart: number,
  windowEnd: number,
  maxTimeSec: number
): HoverPath | null {
  const { allPaths, representativeIndex } = hoverData;
  if (departureTime === null) {
    return representativeIndex !== null && allPaths[representativeIndex]
      ? { ...allPaths[representativeIndex] }
      : getMedianPath(allPaths);
  }
  const info = computeChartInfo(allPaths, windowStart, windowEnd, maxTimeSec);
  const idx = pathIdxAtTime(departureTime, info);
  return idx !== null && allPaths[idx] ? { ...allPaths[idx] } : null;
}

/**
 * One-line summary above the chart, e.g. `avg 24 min / 100% reachable / 10 km`.
 * `distanceKm` is the straight-line origin→destination distance (null if unknown).
 */
export function deriveTitleText(
  hoverData: HoverData,
  departureTime: number | null,
  displayPath: HoverPath | null,
  distanceKm: number | null = null
): string {
  const parts: string[] = [];
  if (departureTime !== null) {
    if (displayPath?.totalTime != null) {
      const depStr = formatTime(displayPath.departureTime);
      parts.push(`${Math.round(displayPath.totalTime / 60)} min (depart ${depStr})`);
    } else {
      parts.push('Unreachable');
    }
  } else {
    const avgSec = hoverData.avgTravelTime;
    const frac = hoverData.reachableFraction ?? 0;
    if (avgSec === null || frac <= 0) {
      parts.push('Unreachable');
    } else {
      parts.push(`avg ${Math.round(avgSec / 60)} min`, `${Math.round(frac * 100)}% reachable`);
    }
  }
  if (distanceKm !== null) parts.push(formatDistance(distanceKm));
  return parts.join(' / ');
}

interface TripChartProps {
  // CSS aspect-ratio for the canvas. Default '1/1'; the mobile bottom drawer
  // passes '5/2' for a wide-short shape that fits a phone viewport. Ignored
  // when `height` is set.
  aspectRatio?: string;
  // Fixed CSS height (e.g. '280px'). When set, takes precedence over
  // `aspectRatio` — used by the desktop HoverInfo panel so toggling its
  // expand/collapse state changes only the chart's width, not its height.
  height?: string;
}

// Sawtooth chart canvas. No outer chrome — callers decide the wrapping
// container and padding. The chart doubles as a scrubber: clicking or dragging
// on it seeks the AnimationStore playhead, exactly like the Timeline bar. When
// the playhead is active it draws a vertical playhead line and highlights the
// path optimal for that departure time.
export function TripChart({ aspectRatio = '1/1', height }: TripChartProps = {}): React.ReactNode {
  const { state } = useAppState();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const chartInfoRef = useRef<ChartInfo | null>(null);
  const { maxTimeMin } = state;
  const hoverData = currentDest(state)?.hoverData ?? null;
  const hasData = hoverData !== null;
  const animMode = useAnimMode();
  const animTime = useAnimTime();
  const committedPlayhead = useAnimCommittedPlayhead();
  const previewPlayhead = useAnimPreviewPlayhead();
  const prefersDark = usePrefersDark();
  const sizeTick = useResizeTick(canvasRef);

  useEffect(() => {
    if (!canvasRef.current) return;
    // With no destination the chart still draws (an empty, y-axis-less strip)
    // and stays scrubbable — computeChartInfo over an empty path list still
    // yields a valid x-range from the window bounds.
    const info = computeChartInfo(
      hoverData?.allPaths ?? [],
      state.windowStart,
      state.windowEnd,
      maxTimeMin * 60
    );
    chartInfoRef.current = info;
    const highlightIdx = hasData && animMode === 'frame' ? pathIdxAtTime(animTime, info) : null;
    drawChart(
      canvasRef.current,
      info,
      highlightIdx,
      prefersDark ? DARK_THEME : LIGHT_THEME,
      committedPlayhead,
      previewPlayhead,
      hasData
    );
  }, [
    hoverData,
    hasData,
    maxTimeMin,
    state.windowStart,
    state.windowEnd,
    animMode,
    animTime,
    committedPlayhead,
    previewPlayhead,
    prefersDark,
    sizeTick,
  ]);

  // Map a pointer event to the departure time under it, or null if the chart
  // geometry isn't ready yet.
  const timeFromEvent = useCallback((e: React.PointerEvent<HTMLCanvasElement>): number | null => {
    if (!chartInfoRef.current) return null;
    const rect = e.currentTarget.getBoundingClientRect();
    return timeAtCanvasX(e.clientX - rect.left, rect.width, chartInfoRef.current);
  }, []);

  // pointerdown commits a seek and captures the pointer so a drag past the
  // canvas edge keeps scrubbing.
  const handlePointerDown = useCallback(
    (e: React.PointerEvent<HTMLCanvasElement>) => {
      if (!animationStore.isReady()) return;
      const t = timeFromEvent(e);
      if (t === null) return;
      e.currentTarget.setPointerCapture(e.pointerId);
      animationStore.seek(t);
    },
    [timeFromEvent]
  );

  // While captured (dragging) a move commits a seek; otherwise it's a hover —
  // a non-committing live preview that restores on pointerleave.
  const handlePointerMove = useCallback(
    (e: React.PointerEvent<HTMLCanvasElement>) => {
      const t = timeFromEvent(e);
      if (t === null) return;
      if (e.currentTarget.hasPointerCapture(e.pointerId)) animationStore.seek(t);
      else animationStore.setPreview(t);
    },
    [timeFromEvent]
  );

  const handlePointerLeave = useCallback(() => {
    animationStore.clearPreview();
  }, []);

  return (
    <canvas
      ref={canvasRef}
      style={{
        width: '100%',
        display: 'block',
        cursor: 'pointer',
        // Suppress the browser's touch panning so a drag-seek on a phone
        // scrubs the chart instead of scrolling the page.
        touchAction: 'none',
        ...(height ? { height } : { aspectRatio }),
      }}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerLeave={handlePointerLeave}
    />
  );
}

// ─── chart playback controls ────────────────────────────────────────────────

// Play/pause (and exit-to-average) cluster anchored to the bottom-left of the
// sawtooth plot. Once a destination is pinned the chart becomes the scrubber,
// so these controls live on it instead of in the standalone Timeline bar.
// Caller must place this inside a `relative` container.
export function ChartPlaybackControls(): React.ReactNode {
  const ready = useAnimReady();
  const playing = useAnimPlaying();
  const mode = useAnimMode();
  // Exact (un-snapped) scrub time — see useAnimTime. The readout shows the
  // precise hovered/seeked instant, while the map still renders snapped frames.
  const time = useAnimTime();
  if (!ready) return null;

  const btn =
    'flex-shrink-0 w-[18px] h-[18px] text-[10px] leading-none cursor-pointer ' +
    'rounded-full p-0 flex items-center justify-center ' +
    'bg-transparent border border-zinc-600 text-zinc-500 ' +
    'dark:border-zinc-600 dark:text-zinc-500';

  return (
    <div className="absolute bottom-[26px] left-9 z-[5] flex items-center gap-1">
      <button
        type="button"
        aria-label={playing ? 'Pause' : 'Play'}
        title={playing ? 'Pause (space)' : 'Play (space)'}
        onClick={() => animationStore.togglePlay()}
        className={btn}
      >
        {playing ? '❚❚' : '▶'}
      </button>
      {mode === 'frame' && (
        <button
          type="button"
          aria-label="Exit playback, show window average"
          title="Show window average"
          onClick={() => animationStore.exit()}
          className={btn}
        >
          ✕
        </button>
      )}
      <span
        className="px-1 rounded text-[11px] leading-none tabular-nums
          bg-zinc-900/70 dark:bg-zinc-900/70
          [@media(prefers-color-scheme:light)]:bg-white/70
          text-zinc-400 dark:text-zinc-400
          [@media(prefers-color-scheme:light)]:text-zinc-600"
      >
        {mode === 'frame' ? formatTime(time) : 'Avg'}
      </span>
    </div>
  );
}

/** Card chrome — wraps the whole panel when collapsed; per-section in wide mode. */
const CARD_CHROME =
  'bg-zinc-900 dark:bg-zinc-900 [@media(prefers-color-scheme:light)]:bg-white ' +
  'rounded-lg shadow-[0_2px_12px_rgba(0,0,0,0.5)]';

export default function HoverInfo({ isFront, onActivate }: HoverInfoProps): React.ReactNode {
  const { state } = useAppState();
  const [hidden, setHidden] = useState(false);
  /** Desktop "wide mode": panel spans the full viewport for a roomier sawtooth. */
  const [expanded, setExpanded] = useState(false);

  const ready = useAnimReady();
  const animMode = useAnimMode();
  const animDep = useAnimRenderedDeparture();
  const departureTime = animMode === 'frame' ? animDep : null;
  const hoverData = currentDest(state)?.hoverData ?? null;

  if (!hoverData && !ready) return null;

  if (hoverData && hidden) {
    return (
      <button
        id="hover-info"
        onClick={() => setHidden(false)}
        onPointerDown={() => {
          if (!isFront) onActivate();
        }}
        className={`absolute bottom-5 right-2.5 ${isFront ? 'z-[1001]' : 'z-[1000]'}
          bg-zinc-900 dark:bg-zinc-900
          [@media(prefers-color-scheme:light)]:bg-white
          px-3 py-1.5 rounded-lg shadow-[0_2px_12px_rgba(0,0,0,0.5)]
          text-[12px] text-zinc-400 dark:text-zinc-400
          [@media(prefers-color-scheme:light)]:text-zinc-500
          hover:text-zinc-200 dark:hover:text-zinc-200
          [@media(prefers-color-scheme:light)]:hover:text-zinc-700
          max-sm:bottom-auto max-sm:top-2.5 max-sm:left-auto max-sm:right-2.5
          cursor-pointer`}
      >
        Details ▴
      </button>
    );
  }

  // No destination: minimal always-on panel with controls and a short
  // scrubbable chart strip.
  if (!hoverData) {
    return (
      <div
        id="hover-info"
        onPointerDownCapture={() => {
          if (!isFront) onActivate();
        }}
        className={`absolute bottom-5 right-2.5 ${isFront ? 'z-[1001]' : 'z-[1000]'}
          ${CARD_CHROME} p-3
          w-[320px]
          ${expanded ? 'sm:left-2.5 sm:right-2.5 sm:w-auto' : ''}`}
      >
        <div className="relative">
          <div className="absolute bottom-[26px] right-2 z-[5] flex items-center gap-1">
            <ChartExpandButton expanded={expanded} onToggle={() => setExpanded((v) => !v)} />
            <ChartHintButton />
          </div>
          <ChartPlaybackControls />
          <TripChart height="96px" />
        </div>
      </div>
    );
  }

  const displayPath = deriveDisplayPath(
    hoverData,
    departureTime,
    state.windowStart,
    state.windowEnd,
    state.maxTimeMin * 60
  );
  const dest = currentDest(state);
  const distanceKm =
    dest && state.sourceLatLng ? haversineKm(state.sourceLatLng, dest.latLng) : null;
  const titleText = deriveTitleText(hoverData, departureTime, displayPath, distanceKm);

  return (
    <div
      id="hover-info"
      onPointerDownCapture={() => {
        if (!isFront) onActivate();
      }}
      className={`absolute bottom-5 right-2.5 ${isFront ? 'z-[1001]' : 'z-[1000]'}
        flex flex-col
        ${
          expanded
            ? // Wide mode: the panel itself is a transparent positioning shell.
              // Details and chart split into two detached cards stacked with a
              // gap, so the plot can claim full width while the trip text stays
              // a fixed 320px card floating above it.
              //
              // pointer-events-none lets the shell's empty regions (right of the
              // 320px details card, and the gap strip) fall through to the map
              // below — without it the full-width transparent shell silently
              // swallows hover events there. The two cards re-enable hit-testing
              // for themselves with pointer-events-auto.
              'sm:left-2.5 sm:right-2.5 gap-2 sm:pointer-events-none'
            : `${CARD_CHROME} p-3 min-w-[220px] max-w-[320px]`
        }
        max-sm:bottom-auto max-sm:top-2.5 max-sm:left-2.5 max-sm:right-2.5
        max-sm:max-w-none max-sm:max-h-[calc(100vh-90px)] max-sm:overflow-y-auto`}
    >
      <div
        id="hover-info-details"
        className={`overflow-y-auto max-h-[30vh] ${
          expanded ? `${CARD_CHROME} p-3 w-[320px] sm:pointer-events-auto` : ''
        }`}
      >
        {displayPath && displayPath.segments.length > 0 && <PathSegmentList path={displayPath} />}
        <div className="flex items-start justify-between gap-2 mt-1.5">
          <div
            className="font-semibold text-[13px] text-zinc-100 dark:text-zinc-100
            [@media(prefers-color-scheme:light)]:text-zinc-900"
          >
            {titleText}
          </div>
          <button
            onClick={() => setHidden(true)}
            className="sm:hidden text-[11px] text-zinc-500 hover:text-zinc-300
              [@media(prefers-color-scheme:light)]:hover:text-zinc-600
              cursor-pointer shrink-0 leading-none"
            title="Hide details"
          >
            ▾ hide
          </button>
        </div>
      </div>

      <div
        id="hover-info-chart"
        className={`relative flex-shrink-0 max-sm:[&_canvas]:[aspect-ratio:5/2]
          ${
            expanded
              ? // Own card: the canvas fills it edge-to-edge, so overflow-hidden
                // clips the square canvas corners to the card's rounded ones.
                `${CARD_CHROME} overflow-hidden sm:pointer-events-auto`
              : `border-t border-zinc-800 dark:border-zinc-800
                 [@media(prefers-color-scheme:light)]:border-zinc-200 pt-2 mt-1.5`
          }`}
      >
        {/* Bottom-right of the plot, sitting just above the x-axis. The
            chart's PAD.bottom is 22px (axis-label gutter), so a 26px bottom
            offset clears the axis line by ~4px. PAD.right is 8px → right-2
            aligns the group's right edge with the plot's right edge. */}
        <div className="absolute bottom-[26px] right-2 z-[5] flex items-center gap-1">
          <ChartExpandButton expanded={expanded} onToggle={() => setExpanded((v) => !v)} />
          <ChartHintButton />
        </div>
        <ChartPlaybackControls />
        {/* Fixed height (vs aspect-ratio) so toggling expand only changes the
            panel's width, not its height. */}
        <TripChart height="280px" />
      </div>
    </div>
  );
}
