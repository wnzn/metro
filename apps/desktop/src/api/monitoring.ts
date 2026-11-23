// Monitoring events: short operational notes (a component, a code, a one-line
// summary, a time) from exactly one source at a time.
//
// - A Metro server answers `GET /v1/monitoring/events?space_id=SID` for one
//   Space its owner monitors, `?scope=me` for the signed-in user's own
//   personal Chat (resolved from the session alone: no user id is ever sent,
//   so nobody can read someone else's), and `GET /v1/monitoring/events` (no
//   query) for the instance, to org admins only. There is no all-Spaces
//   aggregate, and this client never asks for one.
// - The desktop bridge answers the same path for this computer's own helper
//   (src/device/bridge.ts).
//
// Both answer `{ events: [{ timestamp, component, code, summary }] }`, newest
// first. This module parses that shape defensively: entries it can't read
// are dropped and counted, text is bounded and scrubbed of token shapes, and
// at most MAX_MONITORING_EVENTS are kept.

import { scrub } from "../device/bridge";
import { ApiError } from "./client";
import type { RequestFn } from "./client";

export type MonitoringEvent = {
  /** RFC 3339, as the source wrote it. */
  timestamp: string;
  /** Milliseconds since the epoch, from `timestamp`. */
  at: number;
  component: string;
  code: string;
  summary: string;
};

export type MonitoringList = {
  /** Newest first, as the source ordered them; at most MAX_MONITORING_EVENTS. */
  events: MonitoringEvent[];
  /** Entries that weren't the documented shape and aren't shown. */
  dropped: number;
  /** The source sent more than MAX_MONITORING_EVENTS readable entries. */
  truncated: boolean;
};

/** A server scope: one Space its owner monitors, the caller's own personal Chat, or the instance (org admins). */
export type ServerMonitoringScope = { kind: "space"; spaceId: string } | { kind: "me" } | { kind: "instance" };

export const MAX_MONITORING_EVENTS = 200;
export const MAX_COMPONENT = 64;
export const MAX_CODE = 64;
export const MAX_SUMMARY = 500;

const RFC3339 = /^\d{4}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}(\.\d{1,9})?([Zz]|[+-]\d{2}:\d{2})$/;

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** One line of bounded, printable text with no token shapes; "" when nothing is left. */
export function cleanText(v: string, max: number): string {
  let printable = "";
  for (const ch of v) {
    const c = ch.codePointAt(0)!;
    printable += c < 0x20 || (c >= 0x7f && c <= 0x9f) || c === 0x2028 || c === 0x2029 ? " " : ch;
  }
  const flat = scrub(printable.replace(/\s+/g, " ").trim());
  return flat.length > max ? `${flat.slice(0, max - 1).trimEnd()}…` : flat;
}

/** The time of an RFC 3339 timestamp, or null when it isn't one. */
export function parseTimestamp(v: unknown): number | null {
  if (typeof v !== "string" || v.length > 40 || !RFC3339.test(v)) return null;
  const ms = Date.parse(v.replace(" ", "T"));
  return Number.isFinite(ms) ? ms : null;
}

/** One entry, or null when it isn't the documented shape. */
export function parseMonitoringEvent(v: unknown): MonitoringEvent | null {
  if (!isObject(v)) return null;
  const at = parseTimestamp(v.timestamp);
  if (at === null || typeof v.component !== "string" || typeof v.code !== "string" || typeof v.summary !== "string") return null;
  const component = cleanText(v.component, MAX_COMPONENT);
  const code = cleanText(v.code, MAX_CODE);
  const summary = cleanText(v.summary, MAX_SUMMARY);
  if (!component || !code) return null;
  return { timestamp: v.timestamp as string, at, component, code, summary };
}

/** The list in a response, or null when the response isn't `{ events: [...] }`. */
export function parseMonitoringEvents(v: unknown): MonitoringList | null {
  if (!isObject(v) || !Array.isArray(v.events)) return null;
  const events: MonitoringEvent[] = [];
  let dropped = 0;
  let truncated = false;
  for (const raw of v.events) {
    const event = parseMonitoringEvent(raw);
    if (!event) {
      dropped++;
      continue;
    }
    if (events.length >= MAX_MONITORING_EVENTS) {
      truncated = true;
      continue;
    }
    events.push(event);
  }
  return { events, dropped, truncated };
}

/** The request path for a server scope: exactly one scope, and never a user id. */
export function monitoringPath(scope: ServerMonitoringScope): string {
  if (scope.kind === "space") return `/v1/monitoring/events?space_id=${encodeURIComponent(scope.spaceId)}`;
  if (scope.kind === "me") return "/v1/monitoring/events?scope=me";
  return "/v1/monitoring/events";
}

export function monitoringApi(request: RequestFn) {
  return {
    /** One server scope's recent events, newest first. */
    async monitoringEvents(scope: ServerMonitoringScope): Promise<MonitoringList> {
      const data = await request("GET", monitoringPath(scope));
      const list = parseMonitoringEvents(data);
      if (!list) throw new ApiError("protocol", "The server's monitoring events aren't in a shape this app understands.");
      return list;
    },
  };
}
