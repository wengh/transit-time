const cityModules = import.meta.glob('../../cities/*.json', { eager: true });
export const CITIES = Object.values(cityModules).map(m => m.default || m);
