import type { LayerSpecification, StyleSpecification } from 'maplibre-gl';

export interface MapStyle {
  label: string;
  /**
   * MapLibre style: a style.json URL for CARTO's vector basemaps, or an inline
   * spec wrapping a raster tile source. Vector styles carry their labels as
   * separate `symbol` layers, which lets `MapOverlays` slot the isochrone and
   * route lines *beneath* place and street names.
   */
  style: string | StyleSpecification;
}

export const REPO_ATTR =
  '<a href="https://github.com/wengh/transit-time" target="_blank" rel="noopener">wengh/transit-time</a>';

// CARTO's free vector basemaps (Dark Matter / Positron / Voyager). The tile
// source metadata carries the CARTO + OpenStreetMap attribution, which the
// map's attribution control picks up automatically.
const carto = (name: string): string =>
  `https://basemaps.cartocdn.com/gl/${name}-gl-style/style.json`;

const OSM_ATTR = '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>';

function rasterStyle(tiles: string[], attribution: string, maxzoom: number): StyleSpecification {
  return {
    version: 8,
    sources: { raster: { type: 'raster', tiles, tileSize: 256, attribution, maxzoom } },
    layers: [{ id: 'raster', type: 'raster', source: 'raster' }],
  };
}

export const MAP_STYLES: Record<string, MapStyle> = {
  default: {
    label: 'Default (follows system theme)',
    style: '', // resolved at runtime via resolveMapStyle()
  },
  dark: {
    label: 'Dark',
    style: carto('dark-matter-nolabels'),
  },
  'dark-labels': {
    label: 'Dark + Labels',
    style: carto('dark-matter'),
  },
  light: {
    label: 'Light',
    style: carto('positron-nolabels'),
  },
  'light-labels': {
    label: 'Light + Labels',
    style: carto('positron'),
  },
  osm: {
    label: 'OSM Standard',
    style: rasterStyle(['https://tile.openstreetmap.org/{z}/{x}/{y}.png'], OSM_ATTR, 19),
  },
  'osm-hot': {
    label: 'OSM Humanitarian',
    style: rasterStyle(
      ['a', 'b', 'c'].map((s) => `https://${s}.tile.openstreetmap.fr/hot/{z}/{x}/{y}.png`),
      OSM_ATTR +
        ', tiles style by <a href="https://www.hotosm.org/">Humanitarian OpenStreetMap Team</a>',
      19
    ),
  },
  voyager: {
    label: 'Voyager',
    style: carto('voyager'),
  },
};

export const DEFAULT_MAP_STYLE = 'default';

export function resolveMapStyle(id: string): MapStyle {
  if (id === 'default') {
    const isDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    return MAP_STYLES[isDark ? 'dark-labels' : 'light-labels'];
  }
  return MAP_STYLES[id] ?? MAP_STYLES['light-labels'];
}

// ── Zoom-out detail ────────────────────────────────────────────────────────
//
// CARTO's vector styles hide road classes long after their data is in the
// tiles: minor-road fill only from z15, secondary fill from z13, service roads
// from z15, while the `carto.streets` tiles carry secondary/tertiary from
// z10–11, minor from z12 and service from z13. Their raster tiles draw the
// full street grid at those zooms, so the vector map looked comparatively
// empty when zoomed out. This rewrites the affected layers' `minzoom` and
// low-zoom `line-width` stops to start as hairlines as soon as the data
// exists. Applied through `setStyle`'s `transformStyle`, so it works for any
// of the CARTO styles (they share layer ids) and is a no-op for raster ones.

type Stops = [number, number][];

/** Replace the stops below the last new zoom, keep the style's own above it. */
function withLowZoomStops(width: unknown, low: Stops): unknown {
  if (!width || typeof width !== 'object' || !Array.isArray((width as { stops?: Stops }).stops)) {
    return width;
  }
  const w = width as { base?: number; stops: Stops };
  const cutoff = low[low.length - 1][0];
  return { ...w, stops: [...low, ...w.stops.filter(([z]) => z > cutoff)] };
}

const ROAD_TWEAKS: { match: RegExp; minzoom: number; stops: Stops }[] = [
  // Residential streets: data from z12.
  {
    match: /_minor_case$/,
    minzoom: 12,
    stops: [
      [12, 0.5],
      [13, 1],
    ],
  },
  {
    match: /_minor_fill$/,
    minzoom: 12,
    stops: [
      [12, 0.8],
      [13, 1.2],
      [14, 2],
    ],
  },
  // Secondary/tertiary: case already shows from z11; fill waited until z13.
  {
    match: /_sec_fill/,
    minzoom: 11,
    stops: [
      [11, 1],
      [12, 1.5],
      [13, 2],
    ],
  },
  // Service roads and alleys: data from z13.
  {
    match: /_service_case$/,
    minzoom: 13,
    stops: [
      [13, 0.5],
      [14, 0.8],
      [15, 1],
    ],
  },
  {
    match: /_service_fill$/,
    minzoom: 13,
    stops: [
      [13, 0.5],
      [14, 1],
      [15, 2],
    ],
  },
];

export function tuneStyleForZoomOut(style: StyleSpecification): StyleSpecification {
  const layers = style.layers.map((layer): LayerSpecification => {
    if (layer.type !== 'line' || layer['source-layer'] !== 'transportation') return layer;
    const tweak = ROAD_TWEAKS.find((t) => t.match.test(layer.id));
    if (!tweak) return layer;
    const paint = layer.paint ?? {};
    return {
      ...layer,
      minzoom: Math.min(layer.minzoom ?? tweak.minzoom, tweak.minzoom),
      paint: { ...paint, 'line-width': withLowZoomStops(paint['line-width'], tweak.stops) },
    } as LayerSpecification;
  });
  return { ...style, layers };
}
