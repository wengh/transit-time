import React, { useEffect, useRef, useState, useCallback, useId } from 'react';
import { useAppState } from '../state/AppContext';
import type { HoverPath } from '../utils/router';
import { getMedianPath } from '../utils/hoverInfo';

// ─── chart data types ────────────────────────────────────────────────────────

interface ChartTip {
  tipX: number;    // absolute departure time when you just catch this trip (seconds)
  tipY: number;    // travel time if you just catch it (seconds)
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
  maxTimeSec: number,
): ChartInfo {
  let walkTime: number | null = null;
  let walkPathIdx: number | null = null;
  const rawTips: Array<ChartTip> = [];

  for (let i = 0; i < allPaths.length; i++) {
    const p = allPaths[i];
    if (p.totalTime === null) continue;

    const isWalkOnly = p.segments.length > 0 && p.segments.every(s => s.edgeType === 0);
    if (isWalkOnly) {
      if (walkTime === null || p.totalTime < walkTime) {
        walkTime = p.totalTime;
        walkPathIdx = i;
      }
      continue;
    }

    const firstTransit = p.segments.find(s => s.edgeType === 1);
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
};

function getChartTheme(): ChartTheme {
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? DARK_THEME : LIGHT_THEME;
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
): void {
  const rect = canvas.getBoundingClientRect();
  const size = Math.round(rect.width);
  const height = Math.round(rect.height);
  if (size === 0 || height === 0) return;
  canvas.width = size;
  canvas.height = height;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;

  const { tips, walkTime, walkPathIdx, windowStart, windowEnd, yMax } = info;
  const W = size, H = height;
  const { top: pT, right: pR, bottom: pB, left: pL } = PAD;
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
  if (walkTime === null || walkTime > yMax) {
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
  for (let min = 0; min <= 60; min += 15) {
    const x = xToC(windowStart + min * 60);
    ctx.beginPath(); ctx.moveTo(x, pT); ctx.lineTo(x, pT + plotH); ctx.stroke();
  }
  const step = yTickStep(yMax);
  for (let y = 0; y <= yMax; y += step) {
    const cy = yToC(y);
    ctx.beginPath(); ctx.moveTo(pL, cy); ctx.lineTo(pL + plotW, cy); ctx.stroke();
  }

  // Axes
  ctx.strokeStyle = theme.axis;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(pL, pT);
  ctx.lineTo(pL, pT + plotH);
  ctx.lineTo(pL + plotW, pT + plotH);
  ctx.stroke();

  // X-axis labels (minute offsets from window start)
  ctx.fillStyle = theme.label;
  ctx.font = `${Math.max(9, Math.round(size / 28))}px sans-serif`;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'alphabetic';
  for (let min = 0; min <= 60; min += 15) {
    const x = xToC(windowStart + min * 60);
    ctx.fillText(`+${min}`, x, H - 4);
  }

  // Y-axis labels (minutes)
  ctx.textAlign = 'right';
  ctx.textBaseline = 'middle';
  for (let y = 0; y <= yMax; y += step) {
    const cy = yToC(y);
    ctx.fillText(y === 0 ? '0' : `${Math.round(y / 60)}m`, pL - 3, cy);
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
    ctx.strokeStyle = color;
    ctx.lineWidth = isSelected ? 3.5 : 2;

    // Diagonal from (segStartX, segStartY) down to tip — no horizontal cap
    ctx.beginPath();
    ctx.moveTo(xToC(segStartX), yToC(segStartY));
    ctx.lineTo(xToC(tipX), yToC(tipY));
    ctx.stroke();

    // Dot at tip
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.arc(xToC(tipX), yToC(tipY), isSelected ? 4.5 : 3, 0, Math.PI * 2);
    ctx.fill();
  }

  // Selection highlight ring around the tip dot
  if (selectedIdx !== null) {
    const tip = tips.find(t => t.pathIdx === selectedIdx);
    if (tip && tip.tipY <= yMax) {
      ctx.strokeStyle = theme.selectionRing;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.arc(xToC(tip.tipX), yToC(tip.tipY), 7, 0, Math.PI * 2);
      ctx.stroke();
    }
  }
}

// ─── x-position → path index ─────────────────────────────────────────────────

function pathIdxAtCanvasX(canvasX: number, canvasWidth: number, info: ChartInfo): number | null {
  const { tips, walkPathIdx, windowStart, windowEnd, yMax, walkTime } = info;
  const { left: pL, right: pR } = PAD;
  const plotW = canvasWidth - pL - pR;
  const t = windowStart + ((canvasX - pL) / plotW) * (windowEnd - windowStart);
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
    <div className="absolute top-2.5 right-0 z-[5]">
      <button
        aria-label="How to read this chart"
        aria-expanded={open}
        aria-controls={id}
        onClick={() => setOpen(v => !v)}
        className="flex-shrink-0 w-[18px] h-[18px] text-[11px] leading-[16px] cursor-pointer
          rounded-full p-0
          bg-transparent border border-zinc-600 text-zinc-500
          dark:border-zinc-600 dark:text-zinc-500"
      >?</button>
      {open && (
        <div
          id={id}
          role="tooltip"
          className="absolute top-[22px] right-0 z-10
            bg-zinc-800 dark:bg-zinc-800 border border-zinc-700 dark:border-zinc-700
            rounded-md p-2 w-[220px] text-[11px] leading-relaxed
            text-zinc-300 dark:text-zinc-300
            shadow-[0_2px_8px_rgba(0,0,0,.4)]"
        >
          <strong className="block mb-1">How to read this chart</strong>
          <p className="m-0 mb-1">
            <strong>X-axis:</strong> departure time.{' '}
            <strong>Y-axis:</strong> travel time to this location.
          </p>
          <p className="m-0 mb-1">
            Each <strong>sawtooth curve</strong> is one transit trip — travel time rises as you
            depart later and miss the vehicle, then drops when you catch the next one.
          </p>
          <p className="m-0">
            <strong>Hover</strong> to highlight a departure.{' '}
            <strong>Click</strong> to pin it and see its route on the map.
          </p>
        </div>
      )}
    </div>
  );
}

// ─── component ────────────────────────────────────────────────────────────────

export default function HoverInfo(): React.ReactNode {
  const { state, dispatch } = useAppState();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const chartInfoRef = useRef<ChartInfo | null>(null);
  const [hidden, setHidden] = useState(false);

  const { hoverData, maxTimeMin, departureTime, mode, pinnedNode, selectedSampleIdx, lockedSampleIdx } = state;

  const isSampled = mode === 'sampled' && (hoverData?.allPaths.length ?? 0) > 1;

  // Recompute chart info and redraw whenever relevant state changes
  useEffect(() => {
    if (!canvasRef.current || !hoverData || !isSampled) return;
    const info = computeChartInfo(
      hoverData.allPaths,
      departureTime,
      departureTime + 3600,
      maxTimeMin * 60,
    );
    chartInfoRef.current = info;
    drawChart(canvasRef.current, info, selectedSampleIdx, getChartTheme());
  }, [hoverData, isSampled, maxTimeMin, departureTime, selectedSampleIdx]);

  const handleMouseMove = useCallback((e: React.MouseEvent<HTMLCanvasElement>) => {
    if (lockedSampleIdx !== null || pinnedNode === null || !chartInfoRef.current) return;
    const rect = (e.currentTarget as HTMLCanvasElement).getBoundingClientRect();
    const idx = pathIdxAtCanvasX(e.clientX - rect.left, rect.width, chartInfoRef.current);
    dispatch({ type: 'SELECT_SAMPLE', idx });
  }, [lockedSampleIdx, pinnedNode, dispatch]);

  const handleMouseLeave = useCallback(() => {
    if (lockedSampleIdx !== null || pinnedNode === null) return;
    dispatch({ type: 'SELECT_SAMPLE', idx: null });
  }, [lockedSampleIdx, pinnedNode, dispatch]);

  const handleClick = useCallback((e: React.MouseEvent<HTMLCanvasElement>) => {
    if (pinnedNode === null || !chartInfoRef.current) return;
    const rect = (e.currentTarget as HTMLCanvasElement).getBoundingClientRect();
    const idx = pathIdxAtCanvasX(e.clientX - rect.left, rect.width, chartInfoRef.current);
    dispatch({ type: 'LOCK_SAMPLE', idx: lockedSampleIdx === idx ? null : idx });
  }, [lockedSampleIdx, pinnedNode, dispatch]);

  if (!hoverData) return null;

  if (hidden) {
    return (
      <button
        id="hover-info"
        onClick={() => setHidden(false)}
        className="absolute bottom-5 right-2.5 z-[1000]
          bg-zinc-900 dark:bg-zinc-900
          [@media(prefers-color-scheme:light)]:bg-white
          px-3 py-1.5 rounded-lg shadow-[0_2px_12px_rgba(0,0,0,0.5)]
          text-[12px] text-zinc-400 dark:text-zinc-400
          [@media(prefers-color-scheme:light)]:text-zinc-500
          hover:text-zinc-200 dark:hover:text-zinc-200
          [@media(prefers-color-scheme:light)]:hover:text-zinc-700
          max-sm:bottom-auto max-sm:top-2.5 max-sm:left-auto max-sm:right-2.5
          cursor-pointer"
      >
        Details ▴
      </button>
    );
  }
  const { allPaths, travelTimes } = hoverData;

  // Which path to show details for
  const displayPath = selectedSampleIdx !== null
    ? { ...allPaths[selectedSampleIdx] }
    : getMedianPath(allPaths);

  // Remove initial wait time if it's a selected sample to show the optimal trip time
  if (selectedSampleIdx !== null) {
    const firstTransitIndex = displayPath?.segments.findIndex(s => s.edgeType === 1) ?? -1;
    if (firstTransitIndex !== -1) {
      const firstTransit = displayPath!.segments[firstTransitIndex];
      const waitTime = firstTransit.waitTime;
      displayPath!.segments = displayPath!.segments.map((s, i) => {
        if (i === firstTransitIndex) {
          return { ...s, waitTime: 0 };
        } else {
          return s;
        }
      })
      displayPath!.totalTime! -= waitTime;
      displayPath!.departureTime += waitTime;
    }
  }

  // Title line
  let titleText: string;
  if (selectedSampleIdx !== null) {
    if (displayPath?.totalTime != null) {
      const deptOffMin = Math.round((displayPath.departureTime - departureTime) / 60);
      titleText = `Travel time: ${Math.round(displayPath.totalTime / 60)} min  (+${deptOffMin} min departure)`;
    } else {
      titleText = 'Unreachable';
    }
  } else if (isSampled) {
    const reachable = travelTimes.length;
    const total = allPaths.length;
    const avgMin = reachable > 0
      ? Math.round(travelTimes.reduce((a, b) => a + b, 0) / reachable / 60)
      : 0;
    titleText = `Avg travel time: ${avgMin} min (${reachable}/${total} samples)`;
  } else {
    const p = allPaths[0];
    titleText = p?.totalTime != null
      ? `Travel time: ${Math.round(p.totalTime / 60)} min`
      : 'Unreachable';
  }

  return (
    <div
      id="hover-info"
      className="absolute bottom-5 right-2.5 z-[1000]
        bg-zinc-900 dark:bg-zinc-900
        [@media(prefers-color-scheme:light)]:bg-white
        p-3 rounded-lg shadow-[0_2px_12px_rgba(0,0,0,0.5)]
        min-w-[220px] max-w-[320px]
        flex flex-col
        max-sm:bottom-auto max-sm:top-2.5 max-sm:left-2.5 max-sm:right-2.5
        max-sm:max-w-none max-sm:max-h-[calc(100vh-90px)] max-sm:overflow-y-auto"
    >
      <div id="hover-info-details" className="overflow-y-auto max-h-[30vh]">
        <div className="flex items-start justify-between gap-2 mb-1.5">
          <div className="font-semibold text-[13px] text-zinc-100 dark:text-zinc-100
            [@media(prefers-color-scheme:light)]:text-zinc-900">
            {titleText}
          </div>
          <button
            onClick={() => setHidden(true)}
            className="sm:hidden text-[11px] text-zinc-500 hover:text-zinc-300
              [@media(prefers-color-scheme:light)]:hover:text-zinc-600
              cursor-pointer shrink-0 leading-none mt-0.5"
            title="Hide details"
          >
            ▾ hide
          </button>
        </div>

        {displayPath && displayPath.display && displayPath.display.segmentLines.length > 0 && (
          <div className="border-t border-zinc-800 dark:border-zinc-800
            [@media(prefers-color-scheme:light)]:border-zinc-200
            pt-1.5 mt-0.5">
            {displayPath.display.segmentLines.map((lines, si) => (
              <div key={si}>
                {lines.map((line, li) => (
                  <div
                    key={li}
                    className="text-[12px] py-0.5 text-zinc-100 dark:text-zinc-100
                      [@media(prefers-color-scheme:light)]:text-zinc-900 whitespace-pre"
                  >
                    {line}
                  </div>
                ))}
              </div>
            ))}
          </div>
        )}
      </div>

      {isSampled && (
        <div
          id="hover-info-chart"
          className="flex-shrink-0 relative border-t border-zinc-800 dark:border-zinc-800
            [@media(prefers-color-scheme:light)]:border-zinc-200
            pt-2 mt-1.5
            max-sm:[&_canvas]:[aspect-ratio:5/2]"
        >
          <ChartHintButton />
          <canvas
            ref={canvasRef}
            style={{ width: '100%', display: 'block', cursor: 'crosshair', aspectRatio: '1/1' }}
            onMouseMove={handleMouseMove}
            onMouseLeave={handleMouseLeave}
            onClick={handleClick}
          />
        </div>
      )}
    </div>
  );
}
