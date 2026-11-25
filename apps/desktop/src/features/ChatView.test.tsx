// The personal Chat's empty state and the runs aside: suggested drafts only
// fill an empty message box, run titles drop the @agent addressing, the task
// form stays folded behind "New task" and keeps its draft and target when
// folded. Real NSH engine, fake server.

import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";

type F = ReturnType<typeof world>;

async function inChat(f: F, width = 1200, user: "alice" | "carol" = "alice") {
  const app = await open("../../App.tsx", { network: f.network as any, platform: "linux", width, height: 800 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
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

for (const width of [780, 860, 960]) {
  test(`at ${width} wide the runs open in a dialog over a full-width Chat; a task sent there leaves the Chat intact`, async () => {
    const f = world();
    f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "Audit the build" });
    const app = await inChat(f, width);
    await app.waitFor("label:Show agent runs, 1 active");
    expect(app.shows("id:runs-toggle-attention")).toBe(true);
    expect(app.shows("id:runs-toggle >> text:/Runs/")).toBe(false); // icon only
    expect(app.shows("id:runs-aside")).toBe(false); // never squeezed beside the Chat
    await app.type("id:composer", "my chat draft");

    await app.press("id:runs-toggle");
    await app.waitFor("id:runs-dialog");
    await app.press("id:run-follow:run1");
    await app.waitFor("text:To: agent in Acme › #lobby");
    await app.type("id:dispatch-text", "also check the tests");
    await app.press("id:dispatch-send");
    await app.waitFor("id:dispatch-notice");
    expect(f.posts().at(-1)).toMatchObject({ path: "/v1/spaces/acme/rooms/lobby/messages", body: { text: "also check the tests", invoke_agent: true } });

    await app.press("id:runs-dialog-close");
    await app.waitFor("id:runs-dialog", { state: "gone" });
    expect(app.shows("id:chat-view")).toBe(true);
    expect(app.value("id:composer")).toBe("my chat draft");

    // Escape closes it too.
    await app.press("id:runs-toggle");
    await app.waitFor("id:runs-dialog");
    await app.back();
    await app.waitFor("id:runs-dialog", { state: "gone" });
    expect(app.value("id:composer")).toBe("my chat draft");
  });
}

test("wide runs collapse and reopen without losing either draft or affecting workspace navigation", async () => {
  const f = world();
  const app = await inChat(f, 1200);
  await app.waitFor("id:runs-aside");
  await app.press("id:dispatch-new");
  await app.type("id:dispatch-text", "unfinished task");
  await app.type("id:composer", "unfinished message");
  await app.press("label:Hide agent runs");
  await app.waitFor("id:runs-aside", { state: "gone" });
  expect(app.shows("id:sidebar")).toBe(true);
  expect(app.shows("label:Show agent runs")).toBe(true);
  expect(app.storage()["metro.layout.runs"]).toBe("collapsed");
  await app.keyboard.press("mod+shift+b");
  await app.waitFor("id:runs-aside");
  expect(app.value("id:dispatch-text")).toBe("unfinished task");
  expect(app.value("id:composer")).toBe("unfinished message");
  expect(f.posts().some(p => p.path.endsWith("/messages"))).toBe(false);
});

test("Open from the dialog closes it and goes to the run's room", async () => {
  const f = world();
  f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "Audit the build" });
  const app = await inChat(f, 860);
  await app.press("id:runs-toggle");
  await app.waitFor("id:run-open:run1");
  await app.press("id:run-open:run1");
  await app.waitFor("text:hello from bob in acme");
  expect(app.shows("id:runs-dialog")).toBe(false);
});

const sentAnything = (f: F) => f.posts().some((p) => p.path.endsWith("/messages"));

test("no runs: a short explanation and task suggestions that only fill an empty task draft", async () => {
  const f = world();
  const app = await inChat(f);
  await app.waitFor("id:runs-empty");
  expect(app.shows("text:No runs yet. Tasks you send to a Space's agent show up here.")).toBe(true);
  expect(app.shows("id:dispatch")).toBe(false);

  // The Chat's own message box has the user's words; a task suggestion never touches it.
  await app.type("id:composer", "personal note");
  await app.press("id:runs-suggestion:0");
  await app.waitFor("id:dispatch");
  expect(app.value("id:dispatch-text")).toBe("Summarize what happened in this room recently.");
  expect(app.value("id:composer")).toBe("personal note");
  // No Space or room is chosen for the user, so nothing can be sent yet.
  expect(app.shows("text:Choose a Space and a room.")).toBe(true);
  expect(app.enabled("id:dispatch-send")).toBe(false);

  // A draft the user wrote is never replaced.
  await app.clear("id:dispatch-text");
  await app.type("id:dispatch-text", "my own task");
  await app.press("id:runs-suggestion:1");
  expect(app.value("id:dispatch-text")).toBe("my own task");
  expect(app.shows("label:List open work: open the task form (your draft is kept)")).toBe(true);

  // Folded and reopened from a suggestion: still the user's draft.
  await app.press("id:dispatch-close");
  await app.waitFor("id:dispatch", { state: "gone" });
  await app.press("id:runs-suggestion:0");
  await app.waitFor("id:dispatch");
  expect(app.value("id:dispatch-text")).toBe("my own task");
  expect(app.value("id:composer")).toBe("personal note");

  await app.wait(300);
  expect(sentAnything(f)).toBe(false);
});

test("no runs and no Space: the suggestions are disabled and say why, and never open or send", async () => {
  const f = world();
  const app = await inChat(f, 1200, "carol");
  await app.waitFor("id:runs-empty");
  expect(app.shows("text:No runs yet. Join or create a Space to send its agent a task.")).toBe(true);
  for (const i of [0, 1]) expect(app.enabled(`id:runs-suggestion:${i}`)).toBe(false);
  expect(app.shows("label:Summarize a room: unavailable, you're not in any Space yet")).toBe(true);
  await app.press("id:runs-suggestion:0").catch(() => {});
  await app.wait(200);
  expect(app.shows("id:dispatch")).toBe(false);
  expect(sentAnything(f)).toBe(false);
});
