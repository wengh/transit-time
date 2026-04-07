export const ROUTE_COLORS = ['#e6194b', '#3cb44b', '#4363d8', '#f58231', '#911eb4', '#42d4f4', '#f032e6', '#bfef45', '#469990', '#e6beff'];

export function hexToRgb(hex: string): [number, number, number] | null {
  const result = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(hex);
  if (!result) return null;
  return [parseInt(result[1], 16), parseInt(result[2], 16), parseInt(result[3], 16)];
}

export function travelTimeColor(seconds: number, maxTimeSec: number): [number, number, number] {
  const t = Math.max(0, Math.min(1, seconds / maxTimeSec));
  let r: number, g: number, b: number;
  if (t < 0.25) {
    const s = t / 0.25;
    r = Math.round(s * 255);
    g = 255;
    b = 0;
  } else if (t < 0.5) {
    const s = (t - 0.25) / 0.25;
    r = 255;
    g = Math.round(255 * (1 - s * 0.47));
    b = 0;
  } else if (t < 0.75) {
    const s = (t - 0.5) / 0.25;
    r = Math.round(255 - s * 119);
    g = Math.round(136 - s * 136);
    b = 0;
  } else {
    const s = (t - 0.75) / 0.25;
    r = Math.round(136 - s * 68);
    g = 0;
    b = 0;
  }
  return [r, g, b];
}

// Cool counterpart of travelTimeColor: cyan→blue→purple→dark purple across travel time.
function coolColor(t: number): [number, number, number] {
  if (t < 0.25) {
    const s = t / 0.25;
    return [Math.round(s * 60), Math.round(200 - s * 80), 255];
  } else if (t < 0.5) {
    const s = (t - 0.25) / 0.25;
    return [Math.round(60 + s * 40), Math.round(120 - s * 80), Math.round(255 - s * 35)];
  } else if (t < 0.75) {
    const s = (t - 0.5) / 0.25;
    return [Math.round(100 - s * 20), Math.round(40 - s * 40), Math.round(220 - s * 60)];
  } else {
    const s = (t - 0.75) / 0.25;
    return [Math.round(80 - s * 40), 0, Math.round(160 - s * 80)];
  }
}

// 2D color: warm (yellow→red) when fraction=1, cool (cyan→purple) when fraction=0,
// with a smooth blend in between.
export function isochroneColor(seconds: number, maxTimeSec: number, fraction: number): [number, number, number] {
  const t = Math.max(0, Math.min(1, seconds / maxTimeSec));
  const f = Math.max(0, Math.min(1, fraction));
  const warm = travelTimeColor(seconds, maxTimeSec);
  const cool = coolColor(t);
  return [
    Math.round(warm[0] * f + cool[0] * (1 - f)),
    Math.round(warm[1] * f + cool[1] * (1 - f)),
    Math.round(warm[2] * f + cool[2] * (1 - f)),
  ];
}

export function legendGradient(maxTimeSec: number): string {
  const stops = [0, 0.25, 0.5, 0.75, 1].map((t) => {
    const [r, g, b] = travelTimeColor(t * maxTimeSec, maxTimeSec);
    return `rgb(${r},${g},${b})`;
  });
  return `linear-gradient(to right, ${stops.join(', ')})`;
}
