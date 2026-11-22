// Dates, time zones and the common cron shapes for scheduled prompts. Pure
// functions, no UI.
//
// The server evaluates cron in any IANA zone and stores one-time instants in
// UTC. This app's runtime can format only the device's own zone, UTC and
// fixed offsets, so:
// - a one-time task's date and time are read in the device's zone or in UTC
//   (shown by name), and sent as RFC 3339 with that offset;
// - a recurring task may name any IANA zone. The server checks it, and its
//   next firing comes from the server (`next_run_local`), never from a guess
//   made here.

export type Wall = { year: number; month: number; day: number; hour: number; minute: number };

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] as const;
const WEEKDAY_NAMES = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"] as const;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"] as const;

const pad = (n: number) => String(n).padStart(2, "0");

/** Whether this runtime can format times in `zone`. */
export function zoneFormattable(zone: string): boolean {
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: zone });
    return true;
  } catch {
    return false;
  }
}

/** The device's zone when it is an IANA name this runtime can use, else "UTC". */
export function pickDeviceZone(systemZone: string | undefined | null): string {
  const zone = (systemZone ?? "").trim();
  if (/^(Etc\/)?(UTC|UCT|GMT|Zulu|Universal|Greenwich)([+-]?0)?$/i.test(zone)) return "UTC";
  if (!zone || !/^[A-Za-z_]+(\/[A-Za-z0-9_+\-]+)*$/.test(zone)) return "UTC";
  return zoneFormattable(zone) ? zone : "UTC";
}

/** A plausible IANA zone name (the server is the judge). */
export function looksLikeZone(zone: string): boolean {
  const z = zone.trim();
  return z === "UTC" || /^[A-Za-z_]+(\/[A-Za-z0-9_+\-]+)+$/.test(z) || /^Etc\/[A-Za-z0-9+\-]+$/.test(z);
}

/** The wall-clock time of `instantMs` in `zone`. */
export function wallIn(instantMs: number, zone: string): Wall {
  if (zone === "UTC") {
    const d = new Date(instantMs);
    return { year: d.getUTCFullYear(), month: d.getUTCMonth() + 1, day: d.getUTCDate(), hour: d.getUTCHours(), minute: d.getUTCMinutes() };
  }
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone: zone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).formatToParts(new Date(instantMs));
  const get = (type: string) => Number(parts.find((p) => p.type === type)?.value ?? NaN);
  return { year: get("year"), month: get("month"), day: get("day"), hour: get("hour") % 24, minute: get("minute") };
}

const wallMs = (w: Wall) => Date.UTC(w.year, w.month - 1, w.day, w.hour, w.minute);

/** Minutes `zone` is ahead of UTC at `instantMs`. */
export function offsetMinutes(instantMs: number, zone: string): number {
  return Math.round((wallMs(wallIn(instantMs, zone)) - Math.floor(instantMs / 60_000) * 60_000) / 60_000);
}

export function formatOffset(minutes: number): string {
  if (minutes === 0) return "+00:00";
  const sign = minutes > 0 ? "+" : "-";
  const abs = Math.abs(minutes);
  return `${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`;
}

/** "UTC+08:00", "UTC", "UTC−05:00". */
export function offsetLabel(minutes: number): string {
  return minutes === 0 ? "UTC" : `UTC${formatOffset(minutes)}`;
}

/** Parses "YYYY-MM-DD". */
export function parseDate(input: string): { year: number; month: number; day: number } | null {
  const m = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(input.trim());
  if (!m) return null;
  const [year, month, day] = [Number(m[1]), Number(m[2]), Number(m[3])];
  if (month < 1 || month > 12 || day < 1) return null;
  const last = new Date(Date.UTC(year, month, 0)).getUTCDate();
  return day <= last ? { year, month, day } : null;
}

/** Parses "HH:MM" (24-hour). */
export function parseTime(input: string): { hour: number; minute: number } | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(input.trim());
  if (!m) return null;
  const [hour, minute] = [Number(m[1]), Number(m[2])];
  return hour <= 23 && minute <= 59 ? { hour, minute } : null;
}

export type OnceResult = { ok: true; at: string; instantMs: number } | { ok: false; error: string };

/**
 * The RFC 3339 instant for a date and time read in `zone` ("UTC" or a zone
 * this runtime formats), with that zone's offset at that moment.
 */
export function onceInstant(dateInput: string, timeInput: string, zone: string): OnceResult {
  const date = parseDate(dateInput);
  if (!date) return { ok: false, error: "Enter the date as YYYY-MM-DD, for example 2026-11-23." };
  const time = parseTime(timeInput);
  if (!time) return { ok: false, error: "Enter the time as HH:MM on a 24-hour clock, for example 09:00 or 17:30." };
  const wall: Wall = { ...date, ...time };
  const stamp = `${date.year}-${pad(date.month)}-${pad(date.day)}T${pad(time.hour)}:${pad(time.minute)}:00`;
  if (zone === "UTC") return { ok: true, at: `${stamp}Z`, instantMs: wallMs(wall) };
  if (!zoneFormattable(zone)) return { ok: false, error: `This app can't read times in ${zone}. Use your device's zone or UTC.` };
  // Two passes settle the offset (it may differ on either side of a clock change).
  let guess = wallMs(wall) - offsetMinutes(wallMs(wall), zone) * 60_000;
  guess = wallMs(wall) - offsetMinutes(guess, zone) * 60_000;
  const back = wallIn(guess, zone);
  if (wallMs(back) !== wallMs(wall)) {
    return { ok: false, error: `${stamp.replace("T", " ").slice(0, 16)} doesn't exist in ${zone} (the clocks change then). Pick another time.` };
  }
  return { ok: true, at: `${stamp}${formatOffset(offsetMinutes(guess, zone))}`, instantMs: guess };
}

/** "Mon, Nov 23, 2026 · 09:00". */
export function formatWall(w: Wall): string {
  const weekday = WEEKDAYS[new Date(wallMs(w)).getUTCDay()];
  return `${weekday}, ${MONTHS[w.month - 1]} ${w.day}, ${w.year} · ${pad(w.hour)}:${pad(w.minute)}`;
}

/** Splits an RFC 3339 timestamp into its own wall-clock time and offset (no zone database needed). */
export function parseStamp(stamp: string): { wall: Wall; offset: number; instantMs: number } | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::\d{2}(?:\.\d+)?)?(Z|[+-]\d{2}:\d{2})$/i.exec(stamp.trim());
  if (!m) return null;
  const wall: Wall = { year: Number(m[1]), month: Number(m[2]), day: Number(m[3]), hour: Number(m[4]), minute: Number(m[5]) };
  let offset = 0;
  if (m[6].toUpperCase() !== "Z") {
    const sign = m[6][0] === "-" ? -1 : 1;
    offset = sign * (Number(m[6].slice(1, 3)) * 60 + Number(m[6].slice(4, 6)));
  }
  return { wall, offset, instantMs: wallMs(wall) - offset * 60_000 };
}

/** An instant in `zone`: "Mon, Nov 23, 2026 · 09:00 Asia/Manila (UTC+08:00)". */
export function describeInstant(instantMs: number, zone: string): string {
  const z = zone === "UTC" || zoneFormattable(zone) ? zone : "UTC";
  const offset = z === "UTC" ? 0 : offsetMinutes(instantMs, z);
  const name = z === "UTC" ? "UTC" : `${z} (${offsetLabel(offset)})`;
  return `${formatWall(wallIn(instantMs, z))} ${name}`;
}

/** A stamp read as it was written: "Mon, Nov 23, 2026 · 09:00 Asia/Manila (UTC+08:00)". */
export function describeStamp(stamp: string, zoneName?: string): string | null {
  const parsed = parseStamp(stamp);
  if (!parsed) return null;
  const label = offsetLabel(parsed.offset);
  return `${formatWall(parsed.wall)} ${zoneName && zoneName !== "UTC" ? `${zoneName} (${label})` : label}`;
}

/** "Mon, Nov 23 · 09:00": the year only when it isn't `nowYear`. */
export function formatWallShort(w: Wall, nowYear: number): string {
  const weekday = WEEKDAYS[new Date(wallMs(w)).getUTCDay()];
  const year = w.year === nowYear ? "" : `, ${w.year}`;
  return `${weekday}, ${MONTHS[w.month - 1]} ${w.day}${year} · ${pad(w.hour)}:${pad(w.minute)}`;
}

/**
 * How far `instantMs` is from `nowMs`, in words: "in 45 min", "in 3 h 5 min",
 * "in 2 days", "1 h ago". Rounded down to whole minutes; under a minute is
 * "in under a minute" (or "just now" when past).
 */
export function relativeTime(instantMs: number, nowMs: number): string {
  const diff = instantMs - nowMs;
  const minutes = Math.floor(Math.abs(diff) / 60_000);
  let span: string;
  if (minutes < 1) return diff >= 0 ? "in under a minute" : "just now";
  if (minutes < 60) span = `${minutes} min`;
  else if (minutes < 24 * 60) {
    const h = Math.floor(minutes / 60);
    const m = minutes % 60;
    span = m ? `${h} h ${m} min` : `${h} h`;
  } else {
    const days = Math.floor(minutes / (24 * 60));
    span = days === 1 ? "1 day" : `${days} days`;
  }
  return diff >= 0 ? `in ${span}` : `${span} ago`;
}

/** Today's date in `zone`, as YYYY-MM-DD. */
export function todayIn(nowMs: number, zone: string): string {
  const w = wallIn(nowMs, zone === "UTC" || zoneFormattable(zone) ? zone : "UTC");
  return `${w.year}-${pad(w.month)}-${pad(w.day)}`;
}

// ------------------------------------------------------------------ recurrences

/** The recurrences the form offers. `custom` is any 5-field cron expression. */
export type Recurrence =
  | { kind: "daily"; hour: number; minute: number }
  | { kind: "weekdays"; hour: number; minute: number }
  | { kind: "weekly"; weekday: number; hour: number; minute: number }
  | { kind: "custom"; expression: string };

export function cronOf(r: Recurrence): string {
  switch (r.kind) {
    case "daily":
      return `${r.minute} ${r.hour} * * *`;
    case "weekdays":
      return `${r.minute} ${r.hour} * * 1-5`;
    case "weekly":
      return `${r.minute} ${r.hour} * * ${r.weekday}`;
    case "custom":
      return r.expression.trim().split(/\s+/).join(" ");
  }
}

const num = (s: string, max: number) => (/^\d{1,2}$/.test(s) && Number(s) <= max ? Number(s) : null);
const DAY_NAMES = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/** Reads an expression back into one of the simple shapes, else `custom`. */
export function recurrenceOf(expression: string): Recurrence {
  const fields = expression.trim().toLowerCase().split(/\s+/);
  if (fields.length === 5 && fields[2] === "*" && fields[3] === "*") {
    const minute = num(fields[0], 59);
    const hour = num(fields[1], 23);
    if (minute !== null && hour !== null) {
      const dow = fields[4];
      if (dow === "*") return { kind: "daily", hour, minute };
      if (dow === "1-5" || dow === "mon-fri") return { kind: "weekdays", hour, minute };
      const d = /^\d$/.test(dow) ? Number(dow) % 7 : DAY_NAMES.indexOf(dow);
      if (d >= 0 && d <= 6) return { kind: "weekly", weekday: d, hour, minute };
    }
  }
  return { kind: "custom", expression: expression.trim() };
}

/** A light check before the server's full one: five fields, or an @shorthand. */
export function cronShapeError(expression: string): string | null {
  const e = expression.trim();
  if (!e) return "Enter a cron expression, for example 0 9 * * 1-5.";
  if (/^@(hourly|daily|midnight|weekly|monthly|yearly|annually)$/i.test(e)) return null;
  const fields = e.split(/\s+/);
  if (fields.length !== 5) return `A cron expression has 5 fields (minute hour day-of-month month day-of-week); this one has ${fields.length}.`;
  if (fields.some((f) => !/^[\d*,\-\/a-z]+$/i.test(f))) return "Cron fields use digits, *, commas, ranges (1-5), steps (*/15) and names (mon, jan).";
  return null;
}

export { WEEKDAY_NAMES };
