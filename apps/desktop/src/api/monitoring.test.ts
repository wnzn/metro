import { expect, test } from "nsh/test";
import { cleanText, MAX_MONITORING_EVENTS, MAX_SUMMARY, monitoringPath, parseMonitoringEvent, parseMonitoringEvents, parseTimestamp } from "./monitoring";

const ev = (i: number, extra: Record<string, unknown> = {}) => ({ timestamp: `2026-11-23T10:${String(i % 60).padStart(2, "0")}:00Z`, component: "runner", code: "claim_failed", summary: `event ${i}`, ...extra });

test("exactly one scope is ever asked for, and never a user id", () => {
  expect(monitoringPath({ kind: "me" })).toBe("/v1/monitoring/events?scope=me");
  expect(monitoringPath({ kind: "space", spaceId: "acme" })).toBe("/v1/monitoring/events?space_id=acme");
  expect(monitoringPath({ kind: "space", spaceId: "a&b=c" })).toBe("/v1/monitoring/events?space_id=a%26b%3Dc");
  expect(monitoringPath({ kind: "instance" })).toBe("/v1/monitoring/events");
});

test("timestamps must be RFC 3339", () => {
  expect(parseTimestamp("2026-11-23T10:15:02Z")).toBe(Date.UTC(2026, 10, 23, 10, 15, 2));
  expect(parseTimestamp("2026-11-23T10:15:02.123+08:00")).toBe(Date.UTC(2026, 10, 23, 2, 15, 2, 123));
  for (const bad of ["2026-11-23", "yesterday", 1732350000, null, "2026-11-23T10:15:02", "2026-13-45T99:99:99Z"]) expect(parseTimestamp(bad)).toBe(null);
});

test("entries are the documented shape, bounded and scrubbed", () => {
  expect(parseMonitoringEvent(ev(1))).toEqual({ timestamp: "2026-11-23T10:01:00Z", at: Date.UTC(2026, 10, 23, 10, 1), component: "runner", code: "claim_failed", summary: "event 1" });
  expect(parseMonitoringEvent({ ...ev(1), component: 3 })).toBe(null);
  expect(parseMonitoringEvent({ ...ev(1), code: "  " })).toBe(null);
  expect(parseMonitoringEvent({ ...ev(1), timestamp: "now" })).toBe(null);
  const long = parseMonitoringEvent({ ...ev(1), summary: "x".repeat(5000) })!;
  expect(long.summary.length).toBe(MAX_SUMMARY);
  expect(long.summary.endsWith("…")).toBe(true);
  expect(parseMonitoringEvent({ ...ev(1), summary: "rejected Bearer abc metro_rt_v2_secret123 sk-live-abcdefgh\nnext" })!.summary).toBe("rejected Bearer [hidden] [hidden] [hidden] next");
  expect(cleanText("a\u0000b\u2028c", 10)).toBe("a b c");
});

test("lists keep the source's order, drop unreadable entries and cap the rest", () => {
  const many = Array.from({ length: MAX_MONITORING_EVENTS + 50 }, (_, i) => ev(i));
  const list = parseMonitoringEvents({ events: [{ nope: true }, ...many, "junk"] })!;
  expect(list.events).toHaveLength(MAX_MONITORING_EVENTS);
  expect(list.events[0]!.summary).toBe("event 0");
  expect(list.truncated).toBe(true);
  expect(list.dropped).toBe(2);
  expect(parseMonitoringEvents({ events: [] })).toEqual({ events: [], dropped: 0, truncated: false });
  expect(parseMonitoringEvents({})).toBe(null);
  expect(parseMonitoringEvents([ev(1)])).toBe(null);
  expect(parseMonitoringEvents(null)).toBe(null);
});
