// When transcript entries were written, in the viewer's own time zone. Pure
// functions, no UI.
//
// - Times come only from the server (`created_at`). An entry without one
//   (older history) shows no time; nothing is guessed from its neighbours,
//   the clock or the order.
// - The zone is named explicitly: the device's zone when this runtime can
//   format it, else UTC, and then "UTC" is written next to every time so it
//   never reads as local.
// - Day separators show the absolute date (never "Today"), so they never go
//   stale while the app stays open.

import { pickDeviceZone, wallIn } from "./discovery/time";

export type HourCycle = "h11" | "h12" | "h23" | "h24";

export type Clock = {
  /** The zone times are shown in: an IANA name this runtime formats, or "UTC". */
  zone: string;
  /** The device's zone fell back to UTC: say so next to each time. */
  utcFallback: boolean;
  hourCycle: HourCycle;
  /** BCP 47 locale for names of days and months; undefined: the runtime's. */
  locale?: string;
};

/** The clock for the user's locale settings (`localeSettings()`). */
export function clockFor(settings: { timeZone?: string | null; hourCycle?: string | null; languages?: readonly string[] }): Clock {
  const zone = pickDeviceZone(settings.timeZone);
  const systemIsUtc = /^(Etc\/)?(UTC|UCT|GMT|Zulu|Universal|Greenwich)([+-]?0)?$/i.test((settings.timeZone ?? "").trim());
  const hc = settings.hourCycle;
  return {
    zone,
    utcFallback: zone === "UTC" && !systemIsUtc,
    hourCycle: hc === "h11" || hc === "h12" || hc === "h23" || hc === "h24" ? hc : "h23",
    locale: settings.languages?.[0] || undefined,
  };
}

/** Milliseconds since the epoch for a normalized `createdAt`, or null. */
export function instantOf(createdAt: string | undefined | null): number | null {
  if (!createdAt) return null;
  const ms = Date.parse(createdAt);
  return Number.isFinite(ms) && ms > 0 ? ms : null;
}

const pad = (n: number) => String(n).padStart(2, "0");

/** "2026-11-22": the calendar day of `ms` in the clock's zone. */
export function dayKey(ms: number, clock: Clock): string {
  const w = wallIn(ms, clock.zone);
  return `${w.year}-${pad(w.month)}-${pad(w.day)}`;
}

function format(ms: number, clock: Clock, options: Intl.DateTimeFormatOptions): string {
  try {
    return new Intl.DateTimeFormat(clock.locale, { ...options, timeZone: clock.zone }).format(new Date(ms));
  } catch {
    // An unknown locale tag: the runtime's own.
    return new Intl.DateTimeFormat(undefined, { ...options, timeZone: clock.zone }).format(new Date(ms));
  }
}

/** "3:04 PM" or "15:04", as the user reads times; " UTC" after it when the device's zone couldn't be used. */
export function shortTime(ms: number, clock: Clock): string {
  const text = format(ms, clock, { hour: "numeric", minute: "2-digit", hourCycle: clock.hourCycle });
  return clock.utcFallback ? `${text} UTC` : text;
}

/** "Sunday, November 22, 2026": a day separator's label. */
export function dayLabel(ms: number, clock: Clock): string {
  return format(ms, clock, { weekday: "long", year: "numeric", month: "long", day: "numeric" });
}

/** "Sunday, November 22, 2026, 3:04 PM (Asia/Manila)": for screen readers and the hover hint. */
export function fullStamp(ms: number, clock: Clock): string {
  const time = format(ms, clock, { hour: "numeric", minute: "2-digit", second: "2-digit", hourCycle: clock.hourCycle });
  return `${dayLabel(ms, clock)}, ${time} (${clock.zone})`;
}

/** The time shown for an entry, or null when the server didn't record one. */
export type Stamp = { ms: number; day: string; short: string; full: string };

export function stampOf(createdAt: string | undefined | null, clock: Clock): Stamp | null {
  const ms = instantOf(createdAt);
  if (ms === null) return null;
  return { ms, day: dayKey(ms, clock), short: shortTime(ms, clock), full: fullStamp(ms, clock) };
}
