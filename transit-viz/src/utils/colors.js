export const ROUTE_COLORS = ['#e6194b','#3cb44b','#4363d8','#f58231','#911eb4','#42d4f4','#f032e6','#bfef45','#469990','#e6beff'];

export function travelTimeColor(seconds, maxTimeSec) {
  const t = Math.max(0, Math.min(1, seconds / maxTimeSec));
  let r, g, b;
  if (t < 0.25) {
    const s = t / 0.25;
    r = Math.round(s * 255); g = 255; b = 0;
  } else if (t < 0.5) {
    const s = (t - 0.25) / 0.25;
    r = 255; g = Math.round(255 * (1 - s * 0.47)); b = 0;
  } else if (t < 0.75) {
    const s = (t - 0.5) / 0.25;
    r = Math.round(255 - s * 119); g = Math.round(136 - s * 136); b = 0;
  } else {
    const s = (t - 0.75) / 0.25;
    r = Math.round(136 - s * 68); g = 0; b = 0;
  }
  return [r, g, b];
}

export function legendGradient(maxTimeSec) {
  const stops = [0, 0.25, 0.5, 0.75, 1].map(t => {
    const [r, g, b] = travelTimeColor(t * maxTimeSec, maxTimeSec);
    return `rgb(${r},${g},${b})`;
  });
  return `linear-gradient(to right, ${stops.join(', ')})`;
}
