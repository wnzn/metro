import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";
import { decodeEvent } from "../../api/client";
import { attachmentWorld } from "../attachments/test/fixtures";
import { commandSuggestions, parseCommand } from "./registry";

const lobby = "/v1/spaces/acme/rooms/lobby";
const summary = {
  kind: "compaction", actor: "system", text: "**Decision:** ship on Friday.",
  created_at: "2026-11-26T12:00:00Z",
  compaction: { events: 12, trigger: "manual", by: "alice", by_name: "Alice", from_event_id: 1, through_event_id: 20 },
};

async function signIn(network: ReturnType<typeof world>["network"], user = "alice", width = 1200) {
  const app = await open("../../../App.tsx", { modules: launched(), network: network as any, platform: "linux", width, height: 840, clock: {},
    localeSettings: { timeZone: "UTC", hourCycle: "h23" },
  });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:composer");
  return app;
}

async function inAcme(app: App) {
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
}

function fixture() {
  const f = world();
  f.state.events.set("chat:alice", []);
  f.post("chat:alice", { kind: "message", actor: "alice", text: "Unchanged earlier message." });
  const anchor = f.state.events.get("acme/lobby")![0]!;
  const reply = { id: "reply-thread", name: "Replies", parent_room_id: "lobby", anchor_event_id: anchor.id };
  f.addRoom("acme", { id: "agent-thread", name: "Agent thread", parent_room_id: "lobby" });
  const calls: { path: string; body: any }[] = [];
  const posted: number[] = [];
  let refusal: { status: number; body: unknown } | null = null;
  let delay = 0;
  const key = `POST ${BASE}/v1/*`;
  const post = f.network[key]!;
  f.network[key] = req => {
    const path = new URL(req.url).pathname;
    if (path === `${lobby}/events/${anchor.id}/thread`) {
      f.addRoom("acme", reply);
      return { status: 201, body: reply };
    }
    if (!path.endsWith("/compact")) return post(req);
    const body = JSON.parse(req.body ?? "{}");
    calls.push({ path, body });
    if (refusal) return { ...refusal, delay };
    const scope = path === "/v1/chat/compact" ? "chat:alice" : `acme/${path.split("/")[5]}`;
    const event = f.post(scope, { ...summary, compaction: { ...summary.compaction, focus: body.focus } } as any);
    posted.push(event.id);
    return { status: 201, body: event, delay };
  };
  return { f, calls, posted, anchor, fail: (status: number, error: string, code?: string) => { refusal = { status, body: { error, code } }; },
    delay: (ms: number) => { delay = ms; } };
}

test("commands require a leading slash and an exact first word; legacy events decode without metadata", () => {
  expect(parseCommand(" /compact", "chat")).toBe(null);
  expect(parseCommand("//compact", "chat")).toBe(null);
  expect(parseCommand("notes /compact", "chat")).toBe(null);
  expect(parseCommand("/compactish", "room")?.command).toBeUndefined();
  expect(parseCommand("/compact   priorities  ", "thread")?.args).toBe("priorities");
  expect(commandSuggestions("/", "room").map(c => c.name)).toEqual(["compact"]);
  expect(commandSuggestions("/comp", "chat").map(c => c.name)).toEqual(["compact"]);
  expect(commandSuggestions("/compact", "thread")).toHaveLength(0);
  expect(commandSuggestions("/compact focus", "thread")).toHaveLength(0);
  expect(decodeEvent({ id: 1, kind: "message", actor: "Alice", text: "older message" })?.compaction).toBeUndefined();
  expect(decodeEvent({ id: 2, ...summary, compaction: { trigger: "manual" } })?.compaction).toBeUndefined();
});

test("compacting a reply thread uses the thread id and keeps the parent draft and anchor", async () => {
  const { f, anchor, calls, posted } = fixture();
  const app = await signIn(f.network, "alice", 900);
  await inAcme(app);
  await app.type("id:composer", "Keep this parent draft");
  await app.press(`id:reply:${anchor.id}`);
  await app.waitFor(`id:anchor:${anchor.id}`);
  await app.type("id:composer", "/compact next steps");
  await app.submit("id:composer");
  await app.waitUntil(() => posted.length === 1);
  await app.waitFor(`id:thread-transcript >> id:compaction:${posted[0]}:toggle`);
  expect(calls).toEqual([{ path: "/v1/spaces/acme/rooms/reply-thread/compact", body: { focus: "next steps" } }]);
  expect(app.shows(`id:anchor:${anchor.id}`)).toBe(true);
  await app.press("id:close-thread");
  expect(app.value("id:composer")).toBe("Keep this parent draft");
});

test("keyboard selection, focused compaction and expandable copyable marker in narrow Chat", async () => {
  const { f, calls, posted } = fixture();
  const app = await signIn(f.network, "alice", 768);
  await app.type("id:composer", "/");
  await app.waitFor("id:command-suggestions");
  await app.waitFor("label:/compact [focus] — Summarize earlier messages");
  await app.keyboard.press("down");
  await app.keyboard.press("up");
  await app.keyboard.press("tab");
  expect(app.value("id:composer")).toBe("/compact ");
  expect(calls).toHaveLength(0);
  await app.type("id:composer", "decisions");
  await app.submit("id:composer");
  await app.waitFor("text:Conversation summarized");
  expect(calls).toEqual([{ path: "/v1/chat/compact", body: { focus: "decisions" } }]);
  expect(f.posts().filter(p => p.path.endsWith("/messages"))).toHaveLength(0);
  expect(app.value("id:composer")).toBe("");
  const id = `compaction:${posted[0]}`;
  await app.waitFor("label:Earlier messages summarized · 12 messages, expand to read the summary");
  expect(app.shows(`id:${id}:body`)).toBe(false);
  expect(app.shows("text:Unchanged earlier message.")).toBe(true);
  await app.press(`id:${id}:toggle`);
  await app.waitFor(`id:${id}:body`);
  await app.waitFor("text:Summarized for Alice");
  expect(app.shows("text:· focus: decisions")).toBe(true);
  expect(app.shows(`id:${id}:time`)).toBe(true);
  expect(app.texts()).toContain("· 12:00");
  expect(app.shows("label:/12:00:00 \\(UTC\\)$/")).toBe(true);
  expect(app.shows(`id:react:${posted[0]}`)).toBe(false);
  expect(app.shows(`id:reply:${posted[0]}`)).toBe(false);
  expect(app.shows(`id:forward:${posted[0]}`)).toBe(false);
  await app.press(`id:${id}:copy`);
  expect(app.clipboard()).toBe(summary.text);
  await app.press(`id:${id}:toggle`);
  await app.waitFor(`id:${id}:body`, { state: "gone" });
  await app.keyboard.press("shift+tab");
  await app.keyboard.press("enter");
  await app.waitFor(`id:${id}:body`);
});

test("unknown and invalid commands keep the raw draft; // posts just one leading slash", async () => {
  const { f, calls } = fixture();
  const app = await signIn(f.network);
  await app.type("id:composer", "/bogus keep");
  await app.submit("id:composer");
  await app.waitFor("text:Unknown command /bogus. To send text that starts with /, begin with //.");
  expect(app.value("id:composer")).toBe("/bogus keep");
  await app.clear("id:composer");
  await app.type("id:composer", "/compact " + "x".repeat(201));
  await app.press("id:run-command");
  await app.waitFor("text:Use at most 200 printable characters for the focus.");
  expect(calls).toHaveLength(0);
  expect(f.posts().filter(p => p.path.endsWith("/messages"))).toHaveLength(0);
  await app.clear("id:composer");
  await app.type("id:composer", "//compact as text");
  await app.press("id:save-note");
  await app.waitUntil(() => f.posts().some(p => p.body.text === "/compact as text" && p.body.save_note === true));
  await inAcme(app);
  await app.type("id:composer", "//compact here too");
  await app.press("id:send");
  await app.waitUntil(() => f.posts().some(p => p.path === `${lobby}/messages` && p.body.text === "/compact here too"));
  await app.type("id:composer", "//🛰 @agent ");
  await app.press("id:send");
  await app.waitUntil(() => f.posts().some(p => p.path === `${lobby}/messages` && p.body.text === "/🛰 @agent "));
  expect(f.posts().find(p => p.body.text === "/🛰 @agent ")?.body).toMatchObject({
    mentions: [{ kind: "agent" }], spans: [{ start: 6, end: 12, index: 0 }],
  });
  expect(calls).toHaveLength(0);
});

test("compaction works in Space rooms and agent threads, not through their message endpoint", async () => {
  const { f, calls, posted } = fixture();
  const app = await signIn(f.network);
  await inAcme(app);
  await app.type("id:composer", "/comp");
  await app.submit("id:composer");
  expect(app.value("id:composer")).toBe("/compact ");
  await app.submit("id:composer");
  await app.waitUntil(() => posted.length === 1);
  await app.waitFor(`id:compaction:${posted[0]}:toggle`);
  await app.press("id:room:agent-thread");
  await app.type("id:composer", "/compact open questions");
  await app.press("id:run-command");
  await app.waitUntil(() => posted.length === 2);
  await app.waitFor(`id:compaction:${posted[1]}:toggle`);
  expect(calls).toEqual([
    { path: `${lobby}/compact`, body: {} },
    { path: "/v1/spaces/acme/rooms/agent-thread/compact", body: { focus: "open questions" } },
  ]);
  expect(f.posts().filter(p => p.path.endsWith("/messages"))).toHaveLength(0);
});

test("manual refusals are clear, private, and do not discard the command", async () => {
  const { f, calls, fail } = fixture();
  const app = await signIn(f.network);
  await app.type("id:composer", "/compact retry");
  const cases = [
    [409, "reply_running", "Wait for the current reply to finish."],
    [409, "nothing_new", "Nothing new to summarize yet."],
    [409, "compaction_running", "A summary is already being written."],
    [502, "summary_failed", "Couldn't summarize: Your model is offline."],
    [400, "", "Use at most 200 printable characters for the focus."],
    [404, "", "This conversation is no longer available."],
  ] as const;
  for (const [status, code, hint] of cases) {
    fail(status, code === "summary_failed" ? "Your model is offline." : "not displayed", code);
    await app.submit("id:composer");
    await app.waitFor(`text:${hint}`);
    expect(app.value("id:composer")).toBe("/compact retry");
    expect(app.shows(`id:transcript >> text:${hint}`)).toBe(false);
  }
  expect(calls).toHaveLength(cases.length);
  expect(f.posts().filter(p => p.path.endsWith("/messages"))).toHaveLength(0);
  await inAcme(app);
  expect(app.shows("id:command-hint")).toBe(false);
});

test("a delayed private reason does not survive Space access loss", async () => {
  const { f, calls, fail, delay } = fixture();
  const app = await signIn(f.network, "bob");
  await inAcme(app);
  fail(502, "Private model detail", "summary_failed");
  delay(15_000);
  await app.type("id:composer", "/compact");
  await app.submit("id:composer");
  await app.waitUntil(() => calls.length === 1);
  f.state.spaces.get("acme")!.members.delete("bob");
  await app.clock!.runFor(12_000);
  await app.waitFor("id:space:acme", { state: "gone" });
  await app.clock!.runFor(6_000);
  expect(app.shows("text:/Private model detail/")).toBe(false);
  expect(app.shows("id:command-hint")).toBe(false);
});

test("attachments block a command without sending or uploading", async () => {
  const w = attachmentWorld();
  const app = await signIn(w.network);
  app.onPick(() => [{ name: "notes.txt", type: "text/plain", data: new TextEncoder().encode("notes") }]);
  await app.press("id:attach");
  await app.waitFor("id:staged:notes.txt");
  await app.type("id:composer", "/compact decisions");
  await app.submit("id:composer");
  await app.waitFor("text:Commands can't include files.");
  expect(app.value("id:composer")).toBe("/compact decisions");
  expect(app.shows("id:staged:notes.txt")).toBe(true);
  expect(w.f.posts().filter(p => p.path.endsWith("/compact") || p.path.endsWith("/messages"))).toHaveLength(0);
  expect(w.l.log.filter(p => p.method === "POST" && p.path === "/v1/library/assets")).toHaveLength(0);
});

test("auto failure is clear in room activity and only the sender can open its reason", async () => {
  const { f } = fixture();
  const activity = { id: "run-summary", name: "Metro", source: "agent" };
  f.state.runs.push({
    id: activity.id, actor: "alice", status: "completed", space_id: "acme", room_id: "lobby", task: "Continue",
    compaction_failure: { message: "Private auto detail" },
  } as any);
  f.post("acme/lobby", { kind: "activity", actor: "agent", text: "", activity: { ...activity, phase: "started" } } as any);
  f.post("acme/lobby", { kind: "status", actor: "agent", text: "Couldn't summarize earlier messages; the agent used the most recent messages.", activity } as any);
  f.post("acme/lobby", { kind: "agent", actor: "agent", text: "The run still answered.", activity } as any);
  f.post("acme/lobby", { kind: "activity", actor: "agent", text: "", activity: { ...activity, phase: "completed" } } as any);
  const event = f.post("acme/lobby", { ...summary, compaction: { ...summary.compaction, trigger: "auto" } } as any);
  const alice = await signIn(f.network);
  await inAcme(alice);
  await alice.waitFor("text:Couldn't summarize earlier messages");
  await alice.press("id:activity-toggle:agent:run-summary");
  await alice.waitFor("text:Private auto detail");
  await alice.waitFor("text:The run still answered.");
  const bob = await signIn(f.network, "bob");
  await inAcme(bob);
  await bob.waitFor("text:Couldn't summarize earlier messages");
  expect(bob.shows("text:Private auto detail")).toBe(false);
  expect(bob.shows("id:activity-toggle:agent:run-summary")).toBe(false);
  await bob.press(`id:compaction:${event.id}:toggle`);
  await bob.waitFor("text:Automatic");
});

test("the automatic failure reason in personal Chat is legible without affecting the reply", async () => {
  const { f } = fixture();
  const activity = { id: "chat-summary", name: "Metro", source: "agent" };
  f.post("chat:alice", { kind: "status", actor: "agent", text: "Couldn't summarize earlier messages: Private Chat detail. The agent used the most recent messages.", activity } as any);
  f.post("chat:alice", { kind: "agent", actor: "agent", text: "Chat still answered.", activity } as any);
  const app = await signIn(f.network);
  await app.waitFor("text:/Couldn't summarize earlier messages: Private Chat detail/");
  await app.waitFor("text:Chat still answered.");
});
