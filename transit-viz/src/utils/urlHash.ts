export interface HashParams {
  src?: [number, number];
  dst?: [number, number];
  trip?: number;
  style?: string;
  date?: string;
  time?: number;
  dur?: number;
  maxtime?: number;
  slack?: number;
  zoom?: number;
  center?: [number, number];
}

export function getHashParams(): HashParams {
  const hash = window.location.hash.slice(1);
  if (!hash) return {};
  const p = new URLSearchParams(hash);
  const result: HashParams = {};

  function parseLatLng(s: string | null): [number, number] | undefined {
    if (!s) return undefined;
    const [a, b] = s.split(',').map(Number);
    return isFinite(a) && isFinite(b) ? [a, b] : undefined;
  }

  function parseInt2(s: string | null): number | undefined {
    if (!s) return undefined;
    const v = parseInt(s);
    return isFinite(v) ? v : undefined;
  }

  result.src = parseLatLng(p.get('src'));
  result.dst = parseLatLng(p.get('dst'));
  result.trip = parseInt2(p.get('trip'));
  result.center = parseLatLng(p.get('center'));
  const style = p.get('style');
  if (style) result.style = style;
  const date = p.get('date');
  if (date) result.date = date;
  result.time = parseInt2(p.get('time'));
  result.dur = parseInt2(p.get('dur'));
  result.maxtime = parseInt2(p.get('maxtime'));
  result.slack = parseInt2(p.get('slack'));
  result.zoom = parseInt2(p.get('zoom'));

  // Remove undefined keys
  return Object.fromEntries(Object.entries(result).filter(([, v]) => v !== undefined)) as HashParams;
}

export function setHashParams(params: HashParams): void {
  const p = new URLSearchParams();
  if (params.src) p.set('src', `${params.src[0].toFixed(5)},${params.src[1].toFixed(5)}`);
  if (params.dst) p.set('dst', `${params.dst[0].toFixed(5)},${params.dst[1].toFixed(5)}`);
  if (params.trip !== undefined) p.set('trip', String(params.trip));
  if (params.style) p.set('style', params.style);
  if (params.date) p.set('date', params.date);
  if (params.time !== undefined) p.set('time', String(params.time));
  if (params.dur !== undefined) p.set('dur', String(params.dur));
  if (params.maxtime !== undefined) p.set('maxtime', String(params.maxtime));
  if (params.slack !== undefined) p.set('slack', String(params.slack));
  if (params.zoom !== undefined) p.set('zoom', String(params.zoom));
  if (params.center) p.set('center', `${params.center[0].toFixed(5)},${params.center[1].toFixed(5)}`);
  history.replaceState(null, '', window.location.pathname + window.location.search + '#' + p.toString());
}
