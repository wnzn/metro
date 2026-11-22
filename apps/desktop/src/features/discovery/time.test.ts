// The pure helpers behind the schedule form and the result wording.

import { expect, test } from "nsh/test";
import { discoveryApi } from "../../api/discovery";
import { destinationText, nextFiring, rankingText, runDetail, runStatus, scheduleText, scoreText, taskBadge, taskState } from "./present";
import type { ScheduledTask } from "../../api/discovery";
import { cronOf, cronShapeError, describeStamp, onceInstant, parseDate, pickDeviceZone, recurrenceOf, relativeTime } from "./time";

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
  // Relative, then the server's own wall time in the task's zone; the exact
  // moment (with offset, and in UTC) is kept for the expanded card.
  const now = Date.UTC(2026, 10, 23, 22, 0);
  expect(nextFiring(task, "UTC", now)).toEqual({
    text: "Next in 3 h · Tue, Nov 24 · 09:00 Asia/Manila",
    exact: ["Tue, Nov 24, 2026 · 09:00 Asia/Manila (UTC+08:00)", "Tue, Nov 24, 2026 · 01:00 UTC"],
  });
  expect(nextFiring({ ...task, enabled: false }, "UTC", now)).toEqual({ text: "Won't fire while paused.", exact: [] });
  const utcTask: ScheduledTask = { ...task, schedule: { kind: "cron", expression: "0 9 * * *", timezone: "UTC" }, nextRunAt: "2026-11-24T09:00:00Z", nextRunLocal: "2026-11-24T09:00:00Z" };
  expect(nextFiring(utcTask, "UTC", now)).toEqual({ text: "Next in 11 h · Tue, Nov 24 · 09:00 UTC", exact: ["Tue, Nov 24, 2026 · 09:00 UTC"] });
  // Another year shows its year.
  expect(nextFiring({ ...utcTask, nextRunAt: "2027-01-04T09:00:00Z", nextRunLocal: "2027-01-04T09:00:00Z" }, "UTC", now)?.text).toBe("Next in 41 days · Mon, Jan 4, 2027 · 09:00 UTC");
  expect(scheduleText(task)).toBe("Every day at 09:00 · Asia/Manila");
  expect(scheduleText({ ...task, schedule: { kind: "cron", expression: "30 8 * * 1-5", timezone: "UTC" } })).toBe("Weekdays at 08:30 · UTC");
  expect(scheduleText({ ...task, schedule: { kind: "cron", expression: "0 17 * * 5", timezone: "UTC" } })).toBe("Fridays at 17:00 · UTC");
  expect(scheduleText({ ...task, schedule: { kind: "cron", expression: "30 7 1 * *", timezone: "UTC" } })).toBe("Cron 30 7 1 * * · UTC");
  expect(scheduleText({ ...task, schedule: { kind: "once", at: "2026-11-22T18:00:00Z" } }, "UTC", now)).toBe("Once · Sun, Nov 22 · 18:00 UTC");
  expect(destinationText({ kind: "room", spaceId: "acme", roomId: "lobby" }, { spaceName: () => "Acme", roomName: () => "lobby" })).toEqual({ text: "Acme · #lobby", lost: false });
  const fired: ScheduledTask = { ...task, schedule: { kind: "once", at: "2026-11-22T06:55:00Z" }, nextRunAt: null, nextRunLocal: null, lastStatus: "dispatched" };
  expect(taskState(fired)).toBe("finished");
  expect(taskBadge(fired).label).toBe("No more firings");
  // A finished one-time task has no next line; its last firing says what happened.
  expect(nextFiring(fired, "UTC", now)).toBe(null);
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

test("relative times are short and never round up", () => {
  const now = Date.UTC(2026, 10, 23, 12, 0);
  expect(relativeTime(now + 30_000, now)).toBe("in under a minute");
  expect(relativeTime(now + 59 * 60_000 + 59_000, now)).toBe("in 59 min");
  expect(relativeTime(now + 3 * 3_600_000 + 5 * 60_000, now)).toBe("in 3 h 5 min");
  expect(relativeTime(now + 24 * 3_600_000, now)).toBe("in 1 day");
  expect(relativeTime(now - 2 * 3_600_000, now)).toBe("2 h ago");
  expect(relativeTime(now - 10_000, now)).toBe("just now");
});

test("an occurrence's detail is shown only when it adds to its status", () => {
  // The server's fixed wording for these statuses repeats the status's own explanation.
  expect(runDetail("dispatched", "the prompt was posted and agent work was accepted; the agent's result, or its error, is in the conversation")).toBe(null);
  expect(runDetail("unknown", "the submission had not returned after 60s; it was left running and is not retried, and this entry is updated when it returns")).toBe(null);
  expect(runDetail("interrupted", "the server stopped while submitting this occurrence; the trigger may or may not have been posted, and it is not retried")).toBe(null);
  expect(runDetail("dispatched", null)).toBe(null);
  // A reason of its own is kept, as a sentence.
  expect(runDetail("error", "no usable chat connection: the device is offline")).toBe("No usable chat connection: the device is offline.");
  expect(runDetail("revoked", "the owner is no longer a member of the task's Space; the task was paused")).toBe("The owner is no longer a member of the task's Space; the task was paused.");
  expect(runDetail("cancelled", "the task was paused before dispatch")).toBe("The task was paused before dispatch.");
  // A missed run says how many firings it stands for, without raw timestamps.
  expect(runDetail("missed", "the server was not running at the scheduled time; 3 occurrence(s) since 2026-11-22T09:00:00Z were skipped and not run late; next at 2026-11-25T09:00:00Z")).toBe("3 firings skipped.");
  expect(runDetail("missed", "the server was not running at the scheduled time; 1 occurrence(s) since 2026-11-22T09:00:00Z were skipped and not run late; no further occurrence")).toBe("1 firing skipped.");
  expect(runDetail("missed", "1 occurrence skipped")).toBe("1 occurrence skipped.");
});
