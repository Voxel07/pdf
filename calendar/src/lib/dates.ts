import type { CalendarEvent } from './events';

export function dateKey(date: Date) {
  const year = String(date.getFullYear()).padStart(4, '0');
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${year}-${month}-${day}`;
}

export function localDate(key: string) {
  const date = new Date(key + 'T12:00:00');
  date.setHours(0, 0, 0, 0);
  return date;
}

export function moveDay(key: string, days: number) {
  // Stored all-day dates are calendar dates, independent of the browser time zone.
  const date = new Date(key + 'T00:00:00Z');
  date.setUTCDate(date.getUTCDate() + days);
  return date.toISOString().slice(0, 10);
}

export function localInput(date: Date) {
  const hours = String(date.getHours()).padStart(2, '0');
  const minutes = String(date.getMinutes()).padStart(2, '0');
  return `${dateKey(date)}T${hours}:${minutes}`;
}

export function isoFromInput(value: string, original?: string) {
  if (original && value === localInput(new Date(original))) return original;
  const date = new Date(value);
  if (isNaN(+date) || localInput(date) !== value) {
    throw new Error('Choose a valid time in your local time zone.');
  }
  return date.toISOString();
}

export function monthDays(month: Date) {
  const first = dateKey(month);
  const mondayOffset = (month.getDay() + 6) % 7;
  const daysInMonth = new Date(month.getFullYear(), month.getMonth() + 1, 0).getDate();
  const cellCount = Math.ceil((daysInMonth + mondayOffset) / 7) * 7;
  return Array.from({ length: cellCount }, (_, index) => moveDay(first, index - mondayOffset));
}

export function eventsOnDay(events: CalendarEvent[], day: string) {
  const next = moveDay(day, 1);
  const start = localDate(day).toISOString();
  const end = localDate(next).toISOString();
  return events.filter((event) => {
    return event.all_day ? event.starts_at < next && event.ends_at > day
      : event.starts_at < end && event.ends_at > start;
  }).sort((a, b) => Number(b.all_day) - Number(a.all_day) || a.starts_at.localeCompare(b.starts_at));
}
