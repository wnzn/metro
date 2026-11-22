// The personal Chat's empty state and the runs aside: suggested drafts only
// fill an empty message box, run titles drop the @agent addressing, the task
// form stays folded behind "New task" and keeps its draft and target when
// folded. Real NSH engine, fake server.

import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";

type F = ReturnType<typeof world>;

async function inChat(f: F, width = 1200) {
  const app = await open("../../App.tsx", { network: f.network as any, platform: "linux", width, height: 800 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:chat-view");
  return app;
}

const chatPosts = (f: F) => f.posts().filter((p) => p.path.startsWith("/v1/chat/"));

test("an empty Chat suggests drafts that only fill an empty message box and never send", async () => {
  const f = world();
  const app = await inChat(f);
  await app.waitFor("id:chat-empty");
  expect(app.shows("text:Start a conversation")).toBe(true);

  await app.press("id:chat-suggestion:0");
  await app.waitUntil(() => app.value("id:composer") !== "");
  const filled = app.value("id:composer");
  expect(filled).toContain("plan my day");
  await app.wait(300);
  expect(chatPosts(f)).toHaveLength(0); // nothing was sent

  // What the user wrote is never replaced.
  await app.clear("id:composer");
  await app.type("id:composer", "my own words");
  expect(app.enabled("id:chat-suggestion:1")).toBe(false);
  await app.press("id:chat-suggestion:1").catch(() => {});
  expect(app.value("id:composer")).toBe("my own words");
  expect(chatPosts(f)).toHaveLength(0);

  // Sending is still the user's choice; then the transcript replaces the empty state.
  await app.press("id:send");
  await app.waitFor("text:my own words");
  expect(app.shows("id:chat-empty")).toBe(false);
});

test("run cards: titles without @agent, compact Open and Follow up, and Stop only while active", async () => {
  const f = world();
  f.state.runs.push(
    { id: "r-done", space_id: "acme", room_id: "lobby", status: "completed", actor: "alice", task: "@agent what is in the workspace?" },
    { id: "r-live", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "List the workspace and report the files." },
  );
  const app = await inChat(f);
  await app.waitFor("id:run:r-done");
  expect(app.shows("id:run:r-done >> text:What is in the workspace?")).toBe(true);
  expect(app.shows("id:run-title:r-done")).toBe(true);
  expect(app.shows("id:runs-aside >> text:@agent what is in the workspace?")).toBe(false);
  expect(app.shows("id:run-open:r-done")).toBe(true);
  expect(app.shows("id:run-follow:r-done")).toBe(true);
  expect(app.shows("id:run-stop:r-done")).toBe(false);
  expect(app.shows("id:run-stop:r-live")).toBe(true);
  expect(app.shows("label:Stop this run")).toBe(true);
});

test("the task form is folded behind New task, never covers the runs, and folding keeps the draft and target", async () => {
  const f = world();
  f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "Audit the build" });
  const app = await inChat(f);
  await app.waitFor("id:run:run1");
  expect(app.shows("id:dispatch")).toBe(false);
  expect(app.shows("id:dispatch-new")).toBe(true);

  await app.press("id:dispatch-new");
  await app.waitFor("id:dispatch");
  expect(app.shows("id:dispatch-new")).toBe(false);
  expect(app.shows("id:run:run1")).toBe(true); // the list stays visible below the form
  await app.press("id:dispatch-space:acme");
  await app.waitFor("text:To: agent in Acme › #lobby");
  await app.type("id:dispatch-text", "half-written task");

  await app.press("id:dispatch-close");
  await app.waitFor("id:dispatch", { state: "gone" });
  await app.press("id:dispatch-new");
  await app.waitFor("id:dispatch");
  expect(app.value("id:dispatch-text")).toBe("half-written task");
  expect(app.shows("text:To: agent in Acme › #lobby")).toBe(true);
  expect(f.posts().some((p) => p.path.endsWith("/messages"))).toBe(false);
});

test("Follow up opens the folded form, aimed at the run's room", async () => {
  const f = world();
  f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "completed", actor: "alice", task: "Audit the build" });
  const app = await inChat(f);
  await app.waitFor("id:run:run1");
  await app.press("id:run-follow:run1");
  await app.waitFor("id:dispatch");
  await app.waitFor("text:To: agent in Acme › #lobby");
});

test("in a narrow window the runs are shown on request", async () => {
  const f = world();
  f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "Audit the build" });
  const app = await inChat(f, 820);
  await app.waitFor("id:runs-toggle");
  expect(app.shows("id:runs-aside")).toBe(false);
  await app.waitFor("id:runs-toggle >> text:Runs · 1 active");
  await app.press("id:runs-toggle");
  await app.waitFor("id:run:run1");
  await app.press("id:runs-toggle");
  await app.waitFor("id:runs-aside", { state: "gone" });
});
