import React, { useEffect, useRef, useState, useCallback } from 'react';
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
  const tipsByKey = new Map<number, ChartTip & { depTime: number }>();

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

    // Deduplicate by arrival time: same arrival = same destination reached at the
    // same moment, so keep only the latest departure (least wasted waiting time).
    const key = Math.round(p.departureTime + p.totalTime!);
    const existing = tipsByKey.get(key);
    if (!existing || p.departureTime > existing.depTime) {
      tipsByKey.set(key, { tipX, tipY, pathIdx: i, color: p.routeColor, depTime: p.departureTime });
    }
  }

  const tips: ChartTip[] = [...tipsByKey.values()]
    .sort((a, b) => a.tipX - b.tipX)
    .map(({ tipX, tipY, pathIdx, color }) => ({ tipX, tipY, pathIdx, color }));

  const yMax = maxTimeSec;
  return { tips, walkTime, walkPathIdx, windowStart, windowEnd, yMax };
}

// ─── chart drawing ────────────────────────────────────────────────────────────

const PAD = { top: 8, right: 8, bottom: 22, left: 34 };

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
): void {
  const rect = canvas.getBoundingClientRect();
  const size = Math.round(rect.width);
  if (size === 0) return;
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;

  const { tips, walkTime, walkPathIdx, windowStart, windowEnd, yMax } = info;
  const W = size, H = size;
  const { top: pT, right: pR, bottom: pB, left: pL } = PAD;
  const plotW = W - pL - pR;
  const plotH = H - pT - pB;

  const xToC = (t: number) => pL + ((t - windowStart) / (windowEnd - windowStart)) * plotW;
  const yToC = (y: number) => pT + plotH - (y / yMax) * plotH;

  // Background
  ctx.fillStyle = '#fff';
  ctx.fillRect(0, 0, W, H);

  // Grid
  ctx.strokeStyle = '#f0f0f0';
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
  ctx.strokeStyle = '#ccc';
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(pL, pT);
  ctx.lineTo(pL, pT + plotH);
  ctx.lineTo(pL + plotW, pT + plotH);
  ctx.stroke();

  // X-axis labels (minute offsets from window start)
  ctx.fillStyle = '#999';
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
    ctx.strokeStyle = isSelected ? '#555' : '#bbb';
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
    // Clip transit trips that are slower than walking even at the tip
    const clipY = walkTime !== null ? Math.min(walkTime, yMax) : yMax;
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
      ctx.strokeStyle = '#333';
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.arc(xToC(tip.tipX), yToC(tip.tipY), 7, 0, Math.PI * 2);
      ctx.stroke();
    }
  }
}

// ─── x-position → path index ─────────────────────────────────────────────────

function pathIdxAtCanvasX(canvasX: number, canvasWidth: number, info: ChartInfo): number | null {
  const { tips, walkPathIdx, windowStart, windowEnd } = info;
  const { left: pL, right: pR } = PAD;
  const plotW = canvasWidth - pL - pR;
  const t = windowStart + ((canvasX - pL) / plotW) * (windowEnd - windowStart);

  for (let i = 0; i < tips.length; i++) {
    const leftBound = i === 0 ? windowStart : tips[i - 1].tipX;
    if (t >= leftBound && t <= tips[i].tipX) {
      return tips[i].pathIdx;
    }
  }
  return walkPathIdx;
}

// ─── component ────────────────────────────────────────────────────────────────

export default function HoverInfo(): React.ReactNode {
  const { state, dispatch } = useAppState();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const chartInfoRef = useRef<ChartInfo | null>(null);
  const [lockedIdx, setLockedIdx] = useState<number | null>(null);

  const { hoverData, maxTimeMin, departureTime, mode, pinnedNode, selectedSampleIdx } = state;

  // Reset lock when destination is cleared
  useEffect(() => {
    if (!hoverData) setLockedIdx(null);
  }, [hoverData]);

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
    drawChart(canvasRef.current, info, selectedSampleIdx);
  }, [hoverData, isSampled, maxTimeMin, departureTime, selectedSampleIdx]);

  const handleMouseMove = useCallback((e: React.MouseEvent<HTMLCanvasElement>) => {
    if (lockedIdx !== null || pinnedNode === null || !chartInfoRef.current) return;
    const rect = (e.currentTarget as HTMLCanvasElement).getBoundingClientRect();
    const idx = pathIdxAtCanvasX(e.clientX - rect.left, rect.width, chartInfoRef.current);
    dispatch({ type: 'SELECT_SAMPLE', idx });
  }, [lockedIdx, pinnedNode, dispatch]);

  const handleMouseLeave = useCallback(() => {
    if (lockedIdx !== null || pinnedNode === null) return;
    dispatch({ type: 'SELECT_SAMPLE', idx: null });
  }, [lockedIdx, pinnedNode, dispatch]);

  const handleClick = useCallback((e: React.MouseEvent<HTMLCanvasElement>) => {
    if (pinnedNode === null || !chartInfoRef.current) return;
    const rect = (e.currentTarget as HTMLCanvasElement).getBoundingClientRect();
    const idx = pathIdxAtCanvasX(e.clientX - rect.left, rect.width, chartInfoRef.current);
    if (lockedIdx === idx) {
      setLockedIdx(null);
      dispatch({ type: 'SELECT_SAMPLE', idx: null });
    } else {
      setLockedIdx(idx);
      dispatch({ type: 'SELECT_SAMPLE', idx });
    }
  }, [lockedIdx, pinnedNode, dispatch]);

  if (!hoverData) return null;
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
          return { ...s, waitTime: 0, duration: s.duration - waitTime };
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
    const p = allPaths[selectedSampleIdx];
    if (p?.totalTime != null) {
      const deptOffMin = Math.round((p.departureTime - departureTime) / 60);
      titleText = `Travel time: ${Math.round(p.totalTime / 60)} min  (+${deptOffMin} min departure)`;
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
    <div id="hover-info">
      <div id="hover-info-details">
        <div style={{ fontWeight: 600, marginBottom: 6, fontSize: 13 }}>
          {titleText}
        </div>

        {displayPath && displayPath.segments.length > 0 && (() => {
          return (
            <div style={{ borderTop: '1px solid #ddd', paddingTop: 6, marginTop: 2 }}>
              {displayPath.segments.map((seg, si) => (
                <div key={si}>
                  {seg.edgeType === 0 ? (
                    <div style={{ fontSize: 12, color: '#666', padding: '2px 0' }}>
                      Walk {(seg.duration / 60).toFixed(1)} min
                    </div>
                  ) : (
                    <>
                      {seg.waitTime > 0 && (
                        <div style={{ fontSize: 11, color: '#999', padding: '1px 0', fontStyle: 'italic' }}>
                          Wait {(seg.waitTime / 60).toFixed(1)} min
                        </div>
                      )}
                      <div style={{ fontSize: 12, padding: '2px 0' }}>
                        <b>{seg.routeName || 'Transit'}</b>
                        {seg.startStopName && seg.endStopName
                          ? ` · ${seg.startStopName} → ${seg.endStopName}`
                          : ''}
                        {' '}{(seg.duration / 60).toFixed(1)} min
                      </div>
                    </>
                  )}
                </div>
              ))}
            </div>
          );
        })()}
      </div>

      {isSampled && (
        <div id="hover-info-chart" style={{ borderTop: '1px solid #ddd', paddingTop: 8, marginTop: 6 }}>
          <canvas
            ref={canvasRef}
            style={{ width: '100%', aspectRatio: '1 / 1', display: 'block', cursor: 'crosshair' }}
            onMouseMove={handleMouseMove}
            onMouseLeave={handleMouseLeave}
            onClick={handleClick}
          />
        </div>
      )}
    </div>
  );
}
