export interface City {
  id: string;
  name: string;
  file: string;
  bbox: [number, number, number, number];
  center: [number, number];
  zoom: number;
  detail: string;
}

const cityModules = import.meta.glob<City>('../../cities/*.json', { eager: true });

export const CITIES: City[] = Object.values(cityModules).map((m: any) => m.default || m);

export function getCityFromUrl(): City | null {
  const path = window.location.pathname.replace(/^\//, '').replace(/\/$/, '');
  return CITIES.find((c) => c.id === path) || null;
}
