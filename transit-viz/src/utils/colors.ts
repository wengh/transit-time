export const ROUTE_COLORS = [
  '#e6194b',
  '#3cb44b',
  '#4363d8',
  '#f58231',
  '#911eb4',
  '#42d4f4',
  '#f032e6',
  '#bfef45',
  '#469990',
  '#e6beff',
];

/** Sentinel the worker uses for a segment with no GTFS route (walk legs). */
const NO_ROUTE = 0xffffffff;

/**
 * Colour for a transit route, used by both the map polylines and the
 * itinerary dots so they always agree. Prefers the GTFS colour from
 * `routeColors[routeIdx]` (already luminance-adjusted on the Rust side;
 * empty string = none) and otherwise falls back to the palette, indexed by
 * the route index so the same route gets the same colour on every path and
 * in every panel rather than depending on encounter order.
 */
export function routeColorFor(routeIdx: number, routeColors: string[]): string {
  const idx = routeIdx === NO_ROUTE ? 0 : routeIdx;
  return routeColors[idx] || ROUTE_COLORS[idx % ROUTE_COLORS.length];
}

export function hexToRgb(hex: string): [number, number, number] | null {
  const result = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(hex);
  if (!result) return null;
  return [parseInt(result[1], 16), parseInt(result[2], 16), parseInt(result[3], 16)];
}
