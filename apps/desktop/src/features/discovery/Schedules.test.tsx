// The scheduled-task cards, their More menu, the time-zone choice and the
// history's wording, against the shared fake server plus this lane's schedule
// routes (test/fixtures.ts). Real NSH engine, no network.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { discoveryWorld } from "./test/fixtures";

type W = ReturnType<typeof discoveryWorld>;

async function signIn(w: W, zone = "UTC"): Promise<App> {
  const app = await open("./test/Harness.tsx", { network: w.network as any, platform: "linux", width: 1280, height: 900, localeSettings: { timeZone: zone } });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:h-search");
  return app;
}

const text = (app: App) => app.texts().join(" | ");
const lastBody = (w: W, path: string) => [...w.d.log].reverse().find((l) => l.path === path && l.method !== "GET")?.body;

const DISPATCHED_BOILERPLATE = "the prompt was posted and agent work was accepted; the agent's result, or its error, is in the conversation";

function seed(w: W) {
  w.d.tasks.push({
    id: "st1",
    owner: "alice",
    name: "Morning digest",
    prompt: "Summarise yesterday's activity for me.\nThen list open questions.",
    space_id: null,
    room_id: null,
    schedule: { kind: "cron", expression: "0 9 * * *", timezone: "Asia/Manila" },
    enabled: true,
    next_run_at: "2099-11-24T01:00:00Z",
    next_run_local: "2099-11-24T09:00:00+08:00",
    last_status: "dispatched",
    last_run_at: "2099-11-23T01:00:00Z",
    last_detail: DISPATCHED_BOILERPLATE,
  });
  w.d.tasks.push({
    id: "st2",
    owner: "alice",
    name: "Release check",
    prompt: "Check the release.",
    space_id: null,
    room_id: null,
    schedule: { kind: "once", at: "2026-11-22T18:00:00Z" },
    enabled: true,
    next_run_at: null,
    next_run_local: null,
    last_status: "error",
    last_run_at: "2026-11-22T18:00:00Z",
    last_detail: "no usable chat connection: the device is offline",
  });
  w.d.runs.set("st1", [
    { id: 3, due_at: "2099-11-23T01:00:00Z", status: "dispatched", detail: DISPATCHED_BOILERPLATE, claimed_at: "2099-11-23T01:00:01Z", finished_at: "2099-11-23T01:00:02Z" },
    { id: 2, due_at: "2099-11-22T01:00:00Z", status: "dispatched", detail: DISPATCHED_BOILERPLATE, claimed_at: "2099-11-22T01:00:01Z", finished_at: "2099-11-22T01:00:02Z" },
    {
      id: 1,
      due_at: "2099-11-21T01:00:00Z",
      status: "missed",
      detail: "the server was not running at the scheduled time; 2 occurrence(s) since 2099-11-21T01:00:00Z were skipped and not run late; next at 2099-11-23T01:00:00Z",
      claimed_at: "2099-11-23T01:00:00Z",
      finished_at: "2099-11-23T01:00:00Z",
    },
  ]);
}

test("a card shows a compact schedule, the next firing relative and in its own zone, the destination and a one-line prompt", async () => {
  const w = discoveryWorld();
  seed(w);
  const app = await signIn(w);
  await app.press("id:h-schedules");
  await app.waitFor("id:schedule:st1");
  expect(text(app)).toContain("Every day at 09:00 · Asia/Manila");
  expect(text(app)).toMatch(/Next in \d+ days · Tue, Nov 24, 2099 · 09:00 Asia\/Manila/);
  // UTC isn't on the card until it's expanded.
  expect(text(app)).not.toContain("Tue, Nov 24, 2099 · 01:00 UTC");
  expect(text(app)).toContain("Your personal Chat");
  expect(text(app)).toContain("Summarise yesterday's activity for me. Then list open questions.");
  expect(text(app)).not.toContain("Prompt: ");
  // The server's dispatched boilerplate is never shown; the status and time are.
  expect(text(app)).not.toContain("the prompt was posted");
  expect(text(app)).toContain("Last firing Mon, Nov 23, 2099 · 01:00 UTC");
  // A failure keeps its own reason; a finished one-time task has no "next" line.
  await app.waitFor("id:schedule-last:st2");
  expect(text(app)).toContain("Not dispatched");
  expect(text(app)).toContain("No usable chat connection: the device is offline.");
  expect(app.shows("id:schedule-next:st2")).toBe(false);
  expect(text(app)).toContain("Once · Sun, Nov 22 · 18:00 UTC");
});

test("Edit stays on the card; History, Pause and Delete… are in the More menu, and Delete still asks", async () => {
  const w = discoveryWorld();
  seed(w);
  const app = await signIn(w);
  await app.press("id:h-schedules");
  await app.waitFor("id:schedule:st1");
  expect(app.shows("id:schedule-edit:st1")).toBe(true);
  expect(app.shows("id:schedule-more:st1")).toBe(true);
  expect(app.shows("id:schedule-pause:st1")).toBe(false);

  // History: the exact time in the task's zone and UTC, each status explained once, no duplicates.
  await app.press("id:schedule-more:st1");
  await app.press("id:schedule-history:st1");
  await app.waitFor("id:schedule-run:st1:3");
  await app.waitFor("id:schedule-exact:st1");
  expect(text(app)).toContain("Tue, Nov 24, 2099 · 09:00 Asia/Manila (UTC+08:00)");
  expect(text(app)).toContain("Tue, Nov 24, 2099 · 01:00 UTC");
  const all = text(app);
  expect(all.split("This doesn't mean it succeeded").length - 1).toBe(1);
  expect(all).not.toContain("the prompt was posted");
  // The card's own last-firing row steps aside while the history lists it.
  expect(app.shows("id:schedule-last:st1")).toBe(false);
  expect(all).toContain("2 firings skipped.");
  expect(all).not.toContain("2099-11-21T01:00:00Z");

  await app.press("id:schedule-more:st1");
  await app.press("id:schedule-pause:st1");
  await app.waitFor("text:Won't fire while paused.");
  expect(lastBody(w, "/v1/schedules/st1")).toEqual({ enabled: false });
  await app.press("id:schedule-more:st1");
  await app.press("id:schedule-resume:st1");
  await app.waitFor("id:schedule-next:st1");
  expect(lastBody(w, "/v1/schedules/st1")).toEqual({ enabled: true });

  await app.press("id:schedule-more:st1");
  await app.press("id:schedule-delete:st1");
  await app.waitFor("id:schedule-confirm:st1");
  expect(w.d.tasks.some((t) => t.id === "st1")).toBe(true);
  await app.press("id:schedule-confirm-delete:st1");
  await app.waitFor("text:Deleted “Morning digest”. It won't fire again.");
  expect(w.d.tasks.some((t) => t.id === "st1")).toBe(false);
});

test("the time zone is one choice: this device, UTC or another IANA name, shown selected", async () => {
  const w = discoveryWorld();
  const app = await signIn(w, "UTC");
  await app.press("id:h-schedules");
  await app.press("id:schedule-new");
  await app.waitFor("id:schedule-zone:device");
  expect(app.value("id:schedule-timezone")).toBe("UTC");
  await app.press("id:schedule-zone:other");
  expect(app.value("id:schedule-timezone")).toBe("");
  await app.type("id:schedule-timezone", "Asia/Manila");
  await app.waitFor("text:/Sends cron “0 9 \\* \\* \\*” in Asia\\/Manila/");
  await app.press("id:schedule-zone:utc");
  expect(app.value("id:schedule-timezone")).toBe("UTC");
  await app.clear("id:schedule-timezone");
  await app.type("id:schedule-timezone", "Europe/Berlin");
  await app.type("id:schedule-name", "Digest");
  await app.type("id:schedule-prompt", "Summarise.");
  await app.press("id:schedule-save");
  await app.waitFor("text:Scheduled “Digest”.");
  expect((lastBody(w, "/v1/schedules") as { schedule: unknown }).schedule).toEqual({ kind: "cron", expression: "0 9 * * *", timezone: "Europe/Berlin" });

  // A one-time task on a UTC device: both choices are live, the chosen one filled.
  await app.press("id:schedule-new");
  await app.press("id:schedule-when:once");
  await app.waitFor("id:schedule-once-zone:device");
  expect(text(app)).toContain("This device (UTC)");
  await app.press("id:schedule-once-zone:utc");
  await app.press("id:schedule-once-zone:device");
});
