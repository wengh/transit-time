export function formatTime(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
}

export function formatSlack(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return `${m}:${String(s).padStart(2, '0')}`;
}

/** Local calendar date as YYYY-MM-DD (what a `<input type="date">` holds). */
export function localISODate(d: Date = new Date()): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

/** Matches the YYYY-MM-DD shape the router expects; anything else is rejected upstream. */
export const ISO_DATE_RE = /^\d{4}-\d{2}-\d{2}$/;

export function dateToYYYYMMDD(dateStr: string): number {
  return parseInt(dateStr.replace(/-/g, ''), 10);
}
