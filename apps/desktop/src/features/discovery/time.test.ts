// The pure helpers behind the schedule form and the result wording.

import { expect, test } from "nsh/test";
import { discoveryApi } from "../../api/discovery";
import { nextFiringText, rankingText, runStatus, scoreText, taskBadge, taskState } from "./present";
import type { ScheduledTask } from "../../api/discovery";
import { cronOf, cronShapeError, describeStamp, onceInstant, parseDate, pickDeviceZone, recurrenceOf } from "./time";

test("one-time instants carry the zone's offset, and impossible dates are refused", () => {
  expect(onceInstant("2026-11-23", "09:00", "UTC")).toMatchObject({ ok: true, at: "2026-11-23T09:00:00Z" });
  // Offsets are zones this runtime always knows; an IANA zone follows the same path.
  expect(onceInstant("2026-11-23", "09:00", "+08:00")).toMatchObject({ ok: true, at: "2026-11-23T09:00:00+08:00", instantMs: Date.UTC(2026, 10, 23, 1, 0) });
  expect(onceInstant("2026-02-30", "09:00", "UTC").ok).toBe(false);
  expect(onceInstant("2026-11-23", "24:00", "UTC").ok).toBe(false);
  expect(onceInstant("23/11/2026", "09:00", "UTC").ok).toBe(false);
  expect(parseDate("2028-02-29")).toEqual({ year: 2028, month: 2, day: 29 });
  expect(pickDeviceZone("")).toBe("UTC");
  expect(pickDeviceZone("GMT+01:00")).toBe("UTC");
  expect(pickDeviceZone("Etc/UTC")).toBe("UTC");
});

test("common recurrences round-trip through cron; anything else stays custom", () => {
  expect(cronOf({ kind: "daily", hour: 9, minute: 0 })).toBe("0 9 * * *");
  expect(cronOf({ kind: "weekdays", hour: 17, minute: 30 })).toBe("30 17 * * 1-5");
  expect(cronOf({ kind: "weekly", weekday: 1, hour: 8, minute: 5 })).toBe("5 8 * * 1");
  expect(recurrenceOf("0 9 * * *")).toEqual({ kind: "daily", hour: 9, minute: 0 });
  expect(recurrenceOf("30 17 * * mon-fri")).toEqual({ kind: "weekdays", hour: 17, minute: 30 });
  expect(recurrenceOf("0 8 * * 7")).toEqual({ kind: "weekly", weekday: 0, hour: 8, minute: 0 });
  expect(recurrenceOf("*/30 9-17 * * 1-5").kind).toBe("custom");
  expect(cronShapeError("0 9 * *")).toContain("5 fields");
  expect(cronShapeError("@daily")).toBe(null);
  expect(describeStamp("2026-11-24T09:00:00+08:00", "Asia/Manila")).toBe("Tue, Nov 24, 2026 · 09:00 Asia/Manila (UTC+08:00)");
});

test("wording never claims more than the server reported", () => {
  expect(runStatus("dispatched").label).toBe("Dispatched");
  expect(runStatus("dispatched").explain).toContain("doesn't mean it succeeded");
  for (const s of ["dispatched", "error", "unknown", "missed", "cancelled", "revoked", "interrupted", "dispatching"]) {
    expect(runStatus(s).label).not.toMatch(/succe|complete|done/i);
  }
  expect(runStatus("future_status").label).toBe("future_status");
  expect(rankingText("embeddings").label).not.toContain("System One");
  expect(rankingText("classifier").label).toBe("Reranked by System One");

  const task: ScheduledTask = {
    id: "t",
    name: "n",
    prompt: "p",
    target: { kind: "chat" },
    schedule: { kind: "cron", expression: "0 9 * * *", timezone: "Asia/Manila" },
    enabled: true,
    nextRunAt: "2026-11-24T01:00:00Z",
    nextRunLocal: "2026-11-24T09:00:00+08:00",
    lastStatus: null,
    lastRunAt: null,
    lastDetail: null,
  };
  expect(nextFiringText(task, "UTC")).toBe("Next: Tue, Nov 24, 2026 · 09:00 Asia/Manila (UTC+08:00) · Tue, Nov 24, 2026 · 01:00 UTC");
  expect(nextFiringText({ ...task, enabled: false }, "UTC")).toBe("Paused. It won't fire until you resume it.");
  const fired: ScheduledTask = { ...task, schedule: { kind: "once", at: "2026-11-22T06:55:00Z" }, nextRunAt: null, nextRunLocal: null, lastStatus: "dispatched" };
  expect(taskState(fired)).toBe("finished");
  expect(taskBadge(fired).label).toBe("No more firings");
  // Paused before it was due stays paused; a due one-time task stays active.
  expect(taskState({ ...fired, enabled: false })).toBe("paused");
  expect(taskState({ ...fired, nextRunAt: "2026-11-23T00:00:00Z", lastStatus: null })).toBe("active");
  expect(scoreText(0.834, "embeddings")).toBe("similarity 0.83");
  expect(scoreText(0.834, "mixed")).toBe("score 0.83");
});

test("the wrappers send the contract's bodies and reject malformed answers", async () => {
  const calls: { method: string; path: string; body: unknown }[] = [];
  let answer: unknown = { results: [{ space_id: null, room_id: null, event_id: 3, actor: "a", text: "t", score: 1.4 }, { space_id: "s", room_id: null, event_id: 4, actor: "a", text: "bad", score: 1 }], ranking: "mixed", partial: true, notice: "n" };
  const api = discoveryApi(async (method, path, body) => {
    calls.push({ method, path, body });
    return answer;
  });
  const res = await api.search("q", { kind: "space", spaceId: "acme" }, 5);
  expect(calls[0]).toEqual({ method: "POST", path: "/v1/search", body: { query: "q", scope: "space", space_id: "acme", limit: 5 } });
  // A hit with a Space but no room has no place to open: dropped. Scores are clamped.
  expect(res).toEqual({ results: [{ spaceId: null, roomId: null, eventId: 3, actor: "a", text: "t", score: 1 }], ranking: "mixed", partial: true, notice: "n" });

  answer = { results: [], ranking: "reranked", partial: false };
  let error: unknown = null;
  try {
    await api.search("q", { kind: "all" });
  } catch (e) {
    error = e;
  }
  expect((error as { kind?: string })?.kind).toBe("protocol");

  answer = { id: "st1", name: "n", prompt: "p", space_id: null, room_id: null, schedule: { kind: "once", at: "2027-01-01T00:00:00Z" }, enabled: false, next_run_at: null };
  await api.updateSchedule("st1", { enabled: false });
  expect(calls.at(-1)).toEqual({ method: "PATCH", path: "/v1/schedules/st1", body: { enabled: false } });
  await api.updateSchedule("st1", { target: { kind: "chat" } });
  expect(calls.at(-1)?.body).toEqual({ space_id: null, room_id: null });
});
