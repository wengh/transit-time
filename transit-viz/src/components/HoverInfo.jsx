import { useEffect, useRef } from 'react';
import { useAppState } from '../state/AppContext.jsx';

export default function HoverInfo() {
  const { state } = useAppState();
  const canvasRef = useRef(null);
  const { hoverData } = state;

  useEffect(() => {
    if (!canvasRef.current || !hoverData) return;
    const { travelTimes } = hoverData;
    if (!travelTimes || travelTimes.length < 2) return;
    drawDistribution(canvasRef.current, travelTimes);
  }, [hoverData]);

  if (!hoverData) return null;

  const { allPaths, travelTimes } = hoverData;
  if (!travelTimes || travelTimes.length === 0) return null;

  const isSampled = allPaths.length > 1;
  const avg = Math.round(travelTimes.reduce((a, b) => a + b, 0) / travelTimes.length / 60);

  // Find median path for itinerary display
  const reachable = allPaths.filter(p => p.totalTime !== null && isFinite(p.totalTime));
  const midPath = reachable.length > 0 ? reachable[Math.floor(reachable.length / 2)] : null;

  return (
    <div id="hover-info" style={{ display: 'block' }}>
      <div style={{ fontWeight: 600, marginBottom: 6 }}>
        {isSampled
          ? `Avg travel time: ${avg} min (${travelTimes.length}/${allPaths.length} reachable, showing median route)`
          : `Travel time: ${Math.round(travelTimes[0] / 60)} min`}
      </div>

      {midPath && (
        <div style={{ borderTop: '1px solid #ddd', paddingTop: 6, marginTop: 2 }}>
          {midPath.segments.map((seg, si) => (
            <div key={si}>
              {seg.edgeType === 0 ? (
                <div style={{ fontSize: 12, color: '#666', padding: '2px 0' }}>
                  Walk {Math.round(seg.duration / 60)} min
                </div>
              ) : (
                <>
                  {seg.waitTime > 0 && (
                    <div style={{ fontSize: 11, color: '#999', padding: '1px 0', fontStyle: 'italic' }}>
                      {si <= 1 ? 'Initial wait' : 'Transfer wait'}: {(seg.waitTime / 60).toFixed(1)} min
                    </div>
                  )}
                  <div style={{ fontSize: 12, padding: '2px 0' }}>
                    <b>{seg.routeName || 'Transit'}</b>
                    {seg.startStopName && seg.endStopName
                      ? ` \u00b7 ${seg.startStopName} \u2192 ${seg.endStopName}` : ''}
                    {'  '}{Math.round(seg.duration / 60)} min
                  </div>
                </>
              )}
            </div>
          ))}
        </div>
      )}

      {isSampled && travelTimes.length >= 2 && (
        <div style={{ borderTop: '1px solid #ddd', paddingTop: 6, marginTop: 6 }}>
          <canvas ref={canvasRef} height="32" style={{ width: '100%', height: 32, display: 'block' }} />
          <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 10, color: '#888', marginTop: 2 }}>
            <span>min {Math.round(travelTimes[0] / 60)}</span>
            <span>avg {avg}</span>
            <span>max {Math.round(travelTimes[travelTimes.length - 1] / 60)}</span>
          </div>
        </div>
      )}
    </div>
  );
}

function drawDistribution(canvas, travelTimes) {
  const rect = canvas.getBoundingClientRect();
  canvas.width = Math.round(rect.width);
  canvas.height = Math.round(rect.height);
  const ctx = canvas.getContext('2d');
  const w = canvas.width, h = canvas.height;
  ctx.clearRect(0, 0, w, h);

  const minT = travelTimes[0];
  const maxT = travelTimes[travelTimes.length - 1];
  const avgT = travelTimes.reduce((a, b) => a + b, 0) / travelTimes.length;
  const range = maxT - minT;
  const y = h / 2;
  const pad = 8;
  const plotW = w - 2 * pad;

  ctx.strokeStyle = '#ccc';
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(pad, y);
  ctx.lineTo(w - pad, y);
  ctx.stroke();

  ctx.strokeStyle = '#aaa';
  ctx.beginPath();
  ctx.moveTo(pad, y - 6); ctx.lineTo(pad, y + 6);
  ctx.moveTo(w - pad, y - 6); ctx.lineTo(w - pad, y + 6);
  ctx.stroke();

  ctx.fillStyle = '#4a90d9';
  for (let si = 0; si < travelTimes.length; si++) {
    const t = travelTimes[si];
    const x = range > 0 ? pad + ((t - minT) / range) * plotW : w / 2;
    const jitter = ((si * 7 + 3) % 11 - 5) * 1.2;
    ctx.beginPath();
    ctx.arc(x, y + jitter, 3, 0, Math.PI * 2);
    ctx.fill();
  }

  const avgX = range > 0 ? pad + ((avgT - minT) / range) * plotW : w / 2;
  ctx.fillStyle = '#333';
  ctx.beginPath();
  ctx.moveTo(avgX, y - 8);
  ctx.lineTo(avgX - 4, y - 14);
  ctx.lineTo(avgX + 4, y - 14);
  ctx.closePath();
  ctx.fill();
}
