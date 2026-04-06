import JSONC from 'jsonc-simple-parser';

export interface City {
  id: string;
  name: string;
  file: string;
  bbox: [number, number, number, number];
  center: [number, number];
  zoom: number;
  detail: string;
}

const cityModules = import.meta.glob<string>('../../cities/*.jsonc', {
  eager: true,
  query: '?raw',
  import: 'default',
});

export const CITIES: City[] = Object.values(cityModules).map((content) => JSONC.parse(content) as City);

export function getCityFromUrl(): City | null {
  const base = import.meta.env.BASE_URL.replace(/\/$/, '');
  const path = window.location.pathname.replace(new RegExp(`^${base}/`), '').replace(/\/$/, '');
  return CITIES.find((c) => c.id === path) || null;
}
