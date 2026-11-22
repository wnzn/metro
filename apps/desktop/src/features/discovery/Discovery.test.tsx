// Search and Scheduled tasks against the shared fake server plus this lane's
// search and schedule routes (test/fixtures.ts). Real NSH engine, no network.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
import { discoveryWorld } from "./test/fixtures";

type W = ReturnType<typeof discoveryWorld>;

async function signIn(w: W, user: "alice" | "bob" | "carol" = "alice", zone = "UTC"): Promise<App> {
  const app = await open("./test/Harness.tsx", { network: w.network as any, platform: "linux", width: 1280, height: 900, localeSettings: { timeZone: zone } });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:h-search");
  return app;
}

const text = (app: App) => app.texts().join(" | ");
const lastBody = (w: W, path: string) => [...w.d.log].reverse().find((l) => l.path === path && l.method !== "GET")?.body;

function seedCorpus(w: W) {
  w.d.corpus.push(
    { owner: "alice", space_id: null, room_id: null, event_id: 7, actor: "alice", text: "note to self: deploy checklist lives in the wiki", score: 0.91 },
    { owner: "bob", space_id: null, room_id: null, event_id: 8, actor: "bob", text: "bob's private deploy notes", score: 0.99 },
    { space_id: "acme", room_id: "lobby", event_id: 101, actor: "bob", text: "deploy checklist: tag, build, smoke test", score: 0.83 },
    { space_id: "secret", room_id: "lobby", event_id: 900, actor: "carol", text: "carol's secret deploy plan", score: 0.97 },
  );
  w.f.addSpace("secret", "Secret", "carol");
}

/** Types in the search box at the top and presses Return there: the one query field. */
async function search(app: App, query: string) {
  await app.clear("id:top-search");
  await app.type("id:top-search", query);
  await app.submit("id:top-search");
  await app.waitFor("id:search-results");
}

/** Return again in the toolbar box: the only way to (re)run a search; there is no Search button. */
async function searchAgain(app: App) {
  await app.submit("id:top-search");
}

/** A task's History, Pause or Resume and Delete… are items of its "More" menu. */
async function taskMenu(app: App, id: string, item: "history" | "pause" | "resume" | "delete") {
  await app.press(`id:schedule-more:${id}`);
  await app.press(`id:schedule-${item}:${id}`);
}

/** The highlight of a scope chip (its background); exactly the chosen one differs. */
const chip = (app: App, value: string) => app.viewStyle(`id:search-scope:${value}`)?.backgroundColor ?? null;

function expectChosen(app: App, chosen: string, others: string[]) {
  for (const o of others) expect(chip(app, o)).not.toBe(chip(app, chosen));
  for (const o of others.slice(1)) expect(chip(app, o)).toBe(chip(app, others[0]!));
}

const searches = (w: W) => w.d.log.filter((l) => l.path === "/v1/search" && l.method !== "GET").length;

test("search covers my Chat and my Spaces only, says it was reranked by System One, and opens the conversation", async () => {
  const w = discoveryWorld();
  seedCorpus(w);
  const app = await signIn(w);
  await app.press("id:h-search");
  await app.waitFor("id:search-view");
  // One query field, in the toolbar: no second box or Search button on the page.
  expect(app.count("id:top-search")).toBe(1);
  expect(app.shows("id:search-query")).toBe(false);
  expect(app.shows("id:search-submit")).toBe(false);
  expectChosen(app, "all", ["chat", "space:acme"]);
  // Typing changes only the query: nothing is sent (so nothing reaches an embedding
  // connection), the scope stays, and no results appear. Return searches, once.
  await app.type("id:top-search", "deploy checklist");
  await app.wait(300);
  expect(searches(w)).toBe(0);
  expect(app.shows("id:search-results")).toBe(false);
  expectChosen(app, "all", ["chat", "space:acme"]);
  await app.submit("id:top-search");
  await app.waitFor("id:search-results");
  expect(searches(w)).toBe(1);
  expect(lastBody(w, "/v1/search")).toEqual({ query: "deploy checklist", scope: "all", limit: 20 });
  await app.waitFor("id:search-result:chat:chat:7");
  expect(app.shows("id:search-result:acme:lobby:101")).toBe(true);
  // Nobody else's Chat, no Space I'm not in.
  expect(text(app)).not.toContain("bob's private deploy notes");
  expect(text(app)).not.toContain("carol's secret deploy plan");
  expect(text(app)).toContain("2 results for “deploy checklist” · Everything you can read");
  // Ranked by a real System One call, and it says so; no other rerank claim.
  expect(app.shows("id:search-ranking")).toBe(true);
  expect(text(app)).toContain("Reranked by System One");
  expect(text(app)).toContain("Every result was scored by your System One connection.");
  expect(text(app)).not.toContain("Partly reranked");
  expect(text(app)).toContain("Acme › #lobby");
  expect(text(app)).toContain("Personal Chat");
  expect(app.shows("id:search-partial")).toBe(false);
  // Each result opens its conversation, and says whose and where to a screen reader.
  expect(app.shows("label:Open conversation: bob in Acme › #lobby")).toBe(true);
  expect(app.shows("label:Open conversation: alice in Personal Chat")).toBe(true);

  await app.press("id:search-open:acme:lobby:101");
  await app.waitFor("id:space-view");
  await app.waitFor("text:hello from bob in acme");

  await app.press("id:h-search");
  await app.waitFor("id:search-result:chat:chat:7");
  await app.press("id:search-open:chat:chat:7");
  await app.waitFor("id:chat-view");
});

test("the scope lists only joined Spaces; a Space I lost access to answers 404 and resets the scope", async () => {
  const w = discoveryWorld();
  seedCorpus(w);
  const app = await signIn(w);
  await app.press("id:h-search");
  await app.waitFor("id:search-scope:space:side");
  // One chip per Space I'm in; none for a Space I'm not in.
  expect(app.shows("id:search-scope:space:acme")).toBe(true);
  expect(app.shows("id:search-scope:space:secret")).toBe(false);
  expect(app.shows("text:Secret")).toBe(false);
  expect(app.shows("label:Search in Acme")).toBe(true);
  // Choosing a scope sends nothing; only Return does.
  await app.press("id:search-scope:space:acme");
  expectChosen(app, "space:acme", ["all", "chat", "space:side"]);
  expect(searches(w)).toBe(0);
  await search(app, "checklist");
  expect(searches(w)).toBe(1);
  expect(lastBody(w, "/v1/search")).toEqual({ query: "checklist", scope: "space", space_id: "acme", limit: 20 });
  expect(app.shows("id:search-result:chat:chat:7")).toBe(false);
  expect(text(app)).toContain("1 result for “checklist” · Acme");

  // Alice is removed from Acme while the page is open.
  w.f.state.spaces.get("acme")!.members.delete("alice");
  await searchAgain(app);
  await app.waitFor("id:search-error");
  expect(text(app)).toContain("You're not a member of that Space (any more), so it can't be searched. The scope is back to everything you can read.");
  expect(app.shows("id:search-results")).toBe(false);
  expect(text(app)).not.toContain("deploy checklist: tag, build, smoke test");
  expectChosen(app, "all", ["chat", "space:side"]);
});

test("similarity-only and incomplete answers say so; Index more advances indexing", async () => {
  const w = discoveryWorld();
  seedCorpus(w);
  w.d.ranking = "embeddings";
  w.d.partial = true;
  w.d.notice = "120 messages aren't indexed yet. Search again or index more.";
  w.d.remaining = 120;
  const app = await signIn(w);
  await app.press("id:h-search");
  await search(app, "deploy");
  await app.waitFor("id:search-partial");
  // Similarity only: the matches are listed, and nothing claims a rerank.
  expect(app.shows("id:search-result:chat:chat:7")).toBe(true);
  expect(text(app)).toContain("Similarity order");
  expect(text(app)).toContain("Ordered by embedding similarity only. System One didn't rerank these.");
  expect(text(app)).not.toContain("Partly reranked");
  expect(text(app)).not.toContain("Reranked by System One");
  expect(text(app)).toContain("Incomplete: not everything was searched.");
  expect(text(app)).toContain("120 messages aren't indexed yet. Search again or index more.");

  await app.press("id:search-index-more");
  await app.waitFor("id:search-index-progress");
  expect(lastBody(w, "/v1/search/index")).toEqual({ scope: "all" });
  expect(text(app)).toContain("Indexed 64 more; 56 still waiting.");
  await app.press("id:search-index-more");
  await app.waitFor("text:/Indexing is complete/");
  expect(app.enabled("id:search-index-more")).toBe(false);

  w.d.ranking = "mixed";
  w.d.partial = false;
  w.d.notice = null;
  await search(app, "deploy");
  await app.waitFor("text:Partly reranked");
  expect(text(app)).toContain("System One scored the top results; the rest follow in similarity order.");
  expect(text(app)).not.toContain("Reranked by System One");
  expect(app.shows("id:search-partial")).toBe(false);
});

test("search without an embedding model points to Model providers; nothing is cached after sign-out", async () => {
  const w = discoveryWorld();
  seedCorpus(w);
  const app = await signIn(w);
  await app.press("id:h-search");
  await search(app, "deploy checklist");
  expect(text(app)).toContain("note to self: deploy checklist lives in the wiki");

  w.d.fail = { status: 409, error: "choose an embedding model in Settings to use search" };
  await searchAgain(app);
  await app.waitFor("id:search-error");
  expect(text(app)).toContain("Search needs an embedding model: choose an embedding model in Settings to use search.");
  expect(app.shows("id:search-open-providers")).toBe(true);

  // Bob signs in on the same app: none of Alice's query or results remain.
  w.d.fail = null;
  await app.press("id:h-chat");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  await app.clear("id:access-token");
  await app.type("id:access-token", TOKENS.bob!);
  await app.press("id:connect");
  await app.waitFor("id:h-search");
  await app.press("id:h-search");
  await app.waitFor("id:search-view");
  expect(app.value("id:top-search")).toBe("");
  expect(app.shows("id:search-query")).toBe(false); // no second query field on the results page
  expect(app.shows("id:search-results")).toBe(false);
  expect(text(app)).not.toContain("note to self");
  expect(JSON.stringify(app.storage())).not.toContain(TOKENS.alice!);
});

test("a daily 09:00 task in Asia/Manila goes to a chosen room, shows its next firing and destination", async () => {
  const w = discoveryWorld();
  const app = await signIn(w, "alice", "UTC");
  await app.press("id:h-schedules");
  await app.waitFor("text:No scheduled tasks yet");
  await app.press("id:schedule-new");
  await app.waitFor("id:schedule-form");
  await app.type("id:schedule-name", "Morning summary");
  await app.type("id:schedule-prompt", "Summarise yesterday in the lobby.");
  await app.press("id:schedule-destination");
  await app.press("text:Acme");
  await app.waitFor("text:To: the agent in Acme · #lobby");
  // Every day at 09:00 is the default shape; the zone defaults to the device's and is visible.
  expect(app.value("id:schedule-timezone")).toBe("UTC");
  await app.press("id:schedule-zone:other");
  await app.clear("id:schedule-timezone");
  await app.type("id:schedule-timezone", "Asia/Manila");
  await app.waitFor("text:/Sends cron “0 9 \\* \\* \\*” in Asia\\/Manila/");
  await app.press("id:schedule-save");
  await app.waitFor("text:Scheduled “Morning summary”.");
  const body = lastBody(w, "/v1/schedules");
  expect(body).toEqual({
    name: "Morning summary",
    prompt: "Summarise yesterday in the lobby.",
    space_id: "acme",
    room_id: "lobby",
    schedule: { kind: "cron", expression: "0 9 * * *", timezone: "Asia/Manila" },
    enabled: true,
  });
  const id = w.d.tasks[0]!.id;
  await app.waitFor(`id:schedule:${id}`);
  expect(text(app)).toContain("Every day at 09:00 · Asia/Manila");
  // The card: how far off, then the server's wall time in the task's own zone.
  expect(text(app)).toMatch(/Next (in|ago) [^|]* · Tue, Nov 24(, 2026)? · 09:00 Asia\/Manila/);
  expect(app.shows(`id:schedule:${id} >> text:Acme · #lobby`)).toBe(true);
  // The exact instant, with its offset and in UTC, is behind History.
  expect(text(app)).not.toContain("01:00 UTC");
  await app.press(`id:schedule-more:${id}`);
  await app.press(`id:schedule-history:${id}`);
  await app.waitFor(`id:schedule-exact:${id}`);
  expect(text(app)).toContain("Tue, Nov 24, 2026 · 09:00 Asia/Manila (UTC+08:00)");
  expect(text(app)).toContain("Tue, Nov 24, 2026 · 01:00 UTC");

  // A zone the server doesn't know: its own reason, and nothing saved.
  await app.press(`id:schedule-edit:${id}`);
  await app.waitFor("id:schedule-form");
  await app.press("id:schedule-repeat:custom");
  await app.clear("id:schedule-cron");
  await app.type("id:schedule-cron", "30 7 1 * *");
  await app.clear("id:schedule-timezone");
  await app.type("id:schedule-timezone", "Mars/Base");
  await app.press("id:schedule-save");
  await app.waitFor("id:schedule-form-error");
  expect(text(app)).toContain("unknown time zone `Mars/Base`; use an IANA zone such as Asia/Manila");
  expect(w.d.tasks[0]!.schedule).toEqual({ kind: "cron", expression: "0 9 * * *", timezone: "Asia/Manila" }); // nothing saved
  await app.press("id:schedule-zone:device");
  expect(app.value("id:schedule-timezone")).toBe("UTC");
  await app.press("id:schedule-save");
  await app.waitFor("text:Saved “Morning summary”.");
  expect(lastBody(w, `/v1/schedules/${id}`)?.schedule).toEqual({ kind: "cron", expression: "30 7 1 * *", timezone: "UTC" });
  expect(text(app)).toContain("Cron 30 7 1 * * · UTC");
  expect(w.d.tasks).toHaveLength(1); // edited in place, not a second task
});

test("a one-time task in UTC or the device's zone is sent with its offset; past times are refused here", async () => {
  const w = discoveryWorld();
  const app = await signIn(w, "alice", "UTC");
  await app.press("id:h-schedules");
  await app.press("id:schedule-new");
  await app.type("id:schedule-name", "Reminder");
  await app.type("id:schedule-prompt", "Remind me about the release.");
  await app.press("id:schedule-when:once");
  await app.waitFor("id:schedule-once-date");
  // This device is UTC: the device choice is UTC itself.
  expect(text(app)).toContain("This device (UTC)");
  await app.press("id:schedule-once-zone:utc");
  await app.clear("id:schedule-once-date");
  await app.type("id:schedule-once-date", "2020-01-01");
  await app.press("id:schedule-save");
  await app.waitFor("text:That time has already passed. Pick a time in the future.");
  await app.clear("id:schedule-once-date");
  await app.type("id:schedule-once-date", "2099-02-30");
  await app.waitFor("text:Enter the date as YYYY-MM-DD, for example 2026-11-23.");

  const year = new Date().getUTCFullYear() + 1;
  await app.clear("id:schedule-once-date");
  await app.type("id:schedule-once-date", `${year}-03-02`);
  await app.clear("id:schedule-once-time");
  await app.type("id:schedule-once-time", "17:30");
  await app.waitFor(`text:/Fires .*Mar 2, ${year} · 17:30 UTC\\./`);
  await app.press("id:schedule-save");
  await app.waitFor("text:Scheduled “Reminder”.");
  expect(lastBody(w, "/v1/schedules")).toMatchObject({ space_id: null, room_id: null, schedule: { kind: "once", at: `${year}-03-02T17:30:00Z` } });
  const id = w.d.tasks[0]!.id;
  expect(app.shows(`id:schedule:${id} >> text:Your personal Chat`)).toBe(true);
  expect(app.shows(`id:schedule:${id} >> text:/^Next in /`)).toBe(true);
  expect(text(app)).toContain(`Mar 2, ${year} · 17:30 UTC`);
  expect(app.shows(`id:schedule:${id}`)).toBe(true);
});

test("pause, resume, history (dispatched is not completed) and delete with confirmation", async () => {
  const w = discoveryWorld();
  w.d.tasks.push({
    id: "st9",
    owner: "alice",
    name: "Standup nudge",
    prompt: "Ask for standup notes.",
    space_id: null,
    room_id: null,
    schedule: { kind: "cron", expression: "0 9 * * *", timezone: "UTC" },
    enabled: true,
    next_run_at: "2026-11-24T09:00:00Z",
    next_run_local: "2026-11-24T09:00:00Z",
    last_status: "dispatched",
    last_run_at: "2026-11-23T09:00:00Z",
    last_detail: null,
  });
  // Already fired, enabled, nothing left: not "Active", nothing to pause.
  w.d.tasks.push({ id: "st11", owner: "alice", name: "Release check", prompt: "Check the release.", space_id: null, room_id: null, schedule: { kind: "once", at: "2026-11-22T06:55:00Z" }, enabled: true, next_run_at: null, next_run_local: null, last_status: "dispatched", last_run_at: "2026-11-22T06:55:00Z", last_detail: null });
  w.d.tasks.push({ id: "st10", owner: "bob", name: "Bob's own task", prompt: "x", space_id: null, room_id: null, schedule: { kind: "once", at: "2027-01-01T00:00:00Z" }, enabled: true, next_run_at: "2027-01-01T00:00:00Z", next_run_local: null });
  w.d.runs.set("st9", [
    { id: 2, due_at: "2026-11-23T09:00:00Z", status: "dispatched", detail: null, claimed_at: "2026-11-23T09:00:01Z", finished_at: "2026-11-23T09:00:02Z" },
    { id: 1, due_at: "2026-11-22T09:00:00Z", status: "missed", detail: "1 occurrence skipped", claimed_at: "2026-11-22T09:20:00Z", finished_at: null },
  ]);
  const app = await signIn(w);
  await app.press("id:h-schedules");
  await app.waitFor("id:schedule:st9");
  expect(text(app)).not.toContain("Bob's own task");
  expect(text(app)).toContain("Every day at 09:00 · UTC");
  expect(text(app)).toMatch(/Next (in|ago) [^|]* · Tue, Nov 24(, 2026)? · 09:00 UTC/);

  await app.waitFor("id:schedule:st11");
  // Already fired, nothing left: the badge says so and there's no "next" line.
  expect(app.shows("id:schedule:st11 >> text:No more firings")).toBe(true);
  expect(app.shows("id:schedule-next:st11")).toBe(false);
  expect(app.shows("id:schedule-edit:st11")).toBe(true);
  // Its menu holds History and Delete…, but nothing to pause or resume.
  await app.press("id:schedule-more:st11");
  await app.waitFor("id:schedule-history:st11");
  expect(app.shows("id:schedule-delete:st11")).toBe(true);
  expect(app.shows("id:schedule-pause:st11")).toBe(false);
  expect(app.shows("id:schedule-resume:st11")).toBe(false);
  await app.back(); // Escape closes the menu, changing nothing
  await app.waitFor("id:schedule-history:st11", { state: "gone" });

  await taskMenu(app, "st9", "history");
  await app.waitFor("id:schedule-run:st9:2");
  expect(text(app)).toContain("Dispatched");
  expect(text(app)).toContain("This doesn't mean it succeeded");
  expect(text(app)).not.toMatch(/Completed|Succeeded|Fired successfully/);
  expect(text(app)).toContain("Missed");
  expect(text(app)).toContain("1 occurrence skipped");

  await taskMenu(app, "st9", "pause");
  await app.waitFor("id:schedule:st9 >> text:Won't fire while paused.");
  expect(app.shows("id:schedule:st9 >> text:Paused")).toBe(true);
  expect(lastBody(w, "/v1/schedules/st9")).toEqual({ enabled: false });
  await app.press("id:schedule-more:st9");
  await app.waitFor("id:schedule-resume:st9");
  expect(app.shows("id:schedule-pause:st9")).toBe(false);
  await app.press("id:schedule-resume:st9");
  await app.waitUntil(() => w.d.tasks.find((t) => t.id === "st9")?.enabled === true);
  expect(lastBody(w, "/v1/schedules/st9")).toEqual({ enabled: true });

  // Delete… only asks; Cancel keeps the task, and only the confirmation deletes it.
  await taskMenu(app, "st9", "delete");
  await app.waitFor("id:schedule-confirm:st9");
  expect(w.d.tasks.some((t) => t.id === "st9")).toBe(true);
  await app.press("id:schedule-cancel-delete");
  await app.waitFor("id:schedule-confirm:st9", { state: "gone" });
  expect(w.d.tasks.some((t) => t.id === "st9")).toBe(true);
  await taskMenu(app, "st9", "delete");
  await app.press("id:schedule-confirm-delete:st9");
  await app.waitFor("text:Deleted “Standup nudge”. It won't fire again.");
  expect(w.d.tasks.some((t) => t.id === "st9")).toBe(false);
  expect(app.shows("id:schedule:st9")).toBe(false);
});
