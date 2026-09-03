import type { StyleSpecification } from 'maplibre-gl';

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
