export interface MapStyle {
  label: string;
  url: string;
  attribution: string;
  subdomains?: string;
}

const CARTO_ATTR = '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> &copy; <a href="https://carto.com/">CARTO</a>';
const OSM_ATTR = '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors';

export const MAP_STYLES: Record<string, MapStyle> = {
  default: {
    label: 'Default (follows system theme)',
    url: '',           // resolved at runtime via resolveMapStyle()
    attribution: '',
  },
  dark: {
    label: 'Dark',
    url: 'https://{s}.basemaps.cartocdn.com/dark_nolabels/{z}/{x}/{y}{r}.png',
    attribution: CARTO_ATTR,
    subdomains: 'abcd',
  },
  'dark-labels': {
    label: 'Dark + Labels',
    url: 'https://{s}.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}{r}.png',
    attribution: CARTO_ATTR,
    subdomains: 'abcd',
  },
  light: {
    label: 'Light',
    url: 'https://{s}.basemaps.cartocdn.com/light_nolabels/{z}/{x}/{y}{r}.png',
    attribution: CARTO_ATTR,
    subdomains: 'abcd',
  },
  'light-labels': {
    label: 'Light + Labels',
    url: 'https://{s}.basemaps.cartocdn.com/light_all/{z}/{x}/{y}{r}.png',
    attribution: CARTO_ATTR,
    subdomains: 'abcd',
  },
  osm: {
    label: 'OSM Standard',
    url: 'https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png',
    attribution: OSM_ATTR,
    subdomains: 'abc',
  },
  'osm-hot': {
    label: 'OSM Humanitarian',
    url: 'https://{s}.tile.openstreetmap.fr/hot/{z}/{x}/{y}.png',
    attribution: OSM_ATTR,
    subdomains: 'abc',
  },
  voyager: {
    label: 'Voyager',
    url: 'https://{s}.basemaps.cartocdn.com/rastertiles/voyager/{z}/{x}/{y}{r}.png',
    attribution: CARTO_ATTR,
    subdomains: 'abcd',
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
