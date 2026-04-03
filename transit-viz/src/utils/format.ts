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

function getNextMonday(): string {
  const now = new Date();
  const day = now.getDay();
  const daysUntilMonday = day === 0 ? 1 : 8 - day; // If today is Sunday (0), next Monday is tomorrow
  const nextMonday = new Date(now);
  nextMonday.setDate(nextMonday.getDate() + daysUntilMonday);
  return nextMonday.toISOString().slice(0, 10);
}

export function dateToYYYYMMDD(dateStr?: string): number {
  if (!dateStr) {
    dateStr = getNextMonday();
  }
  return parseInt(dateStr.replace(/-/g, ''), 10);
}
