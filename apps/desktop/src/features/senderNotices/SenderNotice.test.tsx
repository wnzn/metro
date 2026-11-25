import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, spaceMessageText, TOKENS } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";
import { spaceWorld } from "../space/test/fixtures";

const pickup = { kind: "classification_failed", reason: "timeout" };
const generic = {
  kind: "generic_agent_unavailable",
  reason: "timeout",
  message: "No agent ran for @agent. This affects only @agent; the agents you mentioned by handle were asked on their own.",
};
const routing = {
  kind: "routing_failed",
  reason: "no_classifier",
  message: "Your model routing could not choose a model: check your System One connection.",
};
const unavailable = {
  kind: "model_unavailable",
  source: "space_default",
  reason: "connection_changed",
  message: "Your Space default model can't be used here. No other model was used.",
};
const time = "2026-11-25T00:00:00.000Z";
const post = `POST ${BASE}/v1/*`;
const get = `GET ${BASE}/v1/*`;
const note = (id: number) => `id:addressing-note:${id}`;

function fixture() {
  const w = spaceWorld();
  const respond = w.network[post]!;
  type Event = NonNullable<ReturnType<typeof w.f.state.events.get>>[number];
  type Reply = { status: number; delay?: number; body: Record<string, unknown> };
  const sent = new Map<string, Event>();
  const replies = new Map<string, (event: Event, key: string) => Reply>();
  const denials = new Map<string, Reply>();
  const denied = new Set<string>();
  const uploads: string[] = [];
  const reply = (text: string, fields: Record<string, unknown>, delay = 0) =>
    replies.set(text, (ev) => ({ status: 202, body: { ...ev, ...fields }, delay }));
  const deny = (text: string, status: 403 | 404, delay: number) =>
    denials.set(text, { status, delay, body: { error: "held send denial" } });
  w.network[post] = req => {
    const url = new URL(req.url);
    const path = url.pathname;
    if (path === "/v1/library/assets") {
      const name = url.searchParams.get("name") ?? "";
      uploads.push(name);
      return { status: 201, body: { id: `asset-${name}`, name, kind: "text", size_bytes: 1, space_id: "side" } };
    }
    const body = req.body && JSON.parse(req.body);
    if (path.endsWith("/messages") && denials.has(body?.text)) {
      denied.add(body.text);
      return denials.get(body.text);
    }
    const answer = respond(req) as Reply;
    if (!path.endsWith("/messages") || answer.status !== 202) return answer;
    const call = w.f.posts().at(-1)!;
    const chat = path.startsWith("/v1/chat/");
    const key = chat ? `chat:${call.user}` :
      path.replace("/v1/spaces/", "").replace("/rooms/", "/").replace("/messages", "");
    // U11's fake applies the server's Space trim and span rebasing before
    // inserting the event. Only the request log and fixture keys use raw text.
    const stored = chat ? body.text : spaceMessageText(body).text;
    const event = w.f.state.events.get(key)!.findLast(e => e.kind === "message" && e.text === stored)!;
    const text = event.text.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");
    const mentions = [...(body.mentions ?? [])];
    if (body.invoke_agent && !mentions.some(m => m.kind === "agent" && m.agent_id === body.agent_id)) {
      mentions.push(body.agent_id ? { kind: "agent", agent_id: body.agent_id } : { kind: "agent" });
    }
    Object.assign(event, { actor: "Same display name", actor_id: call.user, text, mentions, created_at: time });
    sent.set(body.text, event);
    return replies.get(body.text)?.(event, key) ?? answer;
  };
  return { ...w, reply, replies, deny, denied, uploads, sent, messages: () => w.f.posts().filter(p => p.path.endsWith("/messages")) };
}

async function signIn(app: App, account = "alice") {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[account]!);
  await app.press("id:connect");
  await app.waitFor("id:account-menu");
}

async function launch(w: ReturnType<typeof fixture>, opts: { storage?: Record<string, string> } = {}) {
  const app = await open("../../../App.tsx", {
    modules: launched(), network: w.network as any, platform: "linux", width: 1260, height: 900, clock: {}, ...opts,
  });
  if (app.shows("id:show-server-form")) await app.press("id:show-server-form");
  await signIn(app);
  return app;
}

async function space(app: App, id = "acme") {
  await app.press(`id:space:${id}`);
  await app.waitFor(`text:${id === "acme" ? "hello from bob in acme" : "side lobby only"}`);
}

async function send(app: App, text: string, button = "send") {
  await app.type("id:composer", text);
  await app.press(`id:${button}`);
}

async function dispatch(app: App, text: string, sid = "side") {
  if (!app.shows("id:dispatch-text")) await app.press("id:dispatch-new");
  await app.press(`id:dispatch-space:${sid}`);
  await app.clear("id:dispatch-text");
  await app.type("id:dispatch-text", text);
  await app.waitUntil(() => app.enabled("id:dispatch-send"));
  await app.press("id:dispatch-send");
}

test("classification note is under its message; Settings opens Space preferences and Ask agent only prepares or focuses", async () => {
  const w = fixture();
  w.reply("plan together", { addressing_note: pickup });
  const app = await launch(w);
  await space(app);
  await send(app, "plan together");
  await app.waitUntil(() => w.sent.has("plan together"));
  const id = w.sent.get("plan together")!.id;
  await app.waitFor(`id:event:${id} >> ${note(id)}`);
  expect(app.shows("text:Only you see this")).toBe(true);
  expect(app.shows("text:/it took too long.*Nothing was started/")).toBe(true);
  await app.press(`id:addressing-note-settings:${id}`);
  await app.waitFor("id:space-preferences");
  expect(app.shows("id:space-settings-dialog")).toBe(true);
  expect(app.shows("id:settings-dialog")).toBe(false);
  await app.press("id:space-settings-close");

  await app.type("id:composer", "unsent draft");
  await app.press(`id:addressing-note-ask:${id}`);
  await app.keyboard.press("x");
  expect(app.value("id:composer")).toBe("unsent draftx");
  await app.clear("id:composer");
  await app.press(`id:addressing-note-ask:${id}`);
  expect(app.value("id:composer")).toBe("@agent ");
  expect(app.shows("id:mention-chip:agent")).toBe(true);
  expect(w.messages()).toHaveLength(1);

  await app.press(`id:addressing-note-dismiss:${id}`);
  await app.waitFor(note(id), { state: "gone" });
  w.f.state.ignoreCursor = true;
  await app.clock!.runFor(3_000);
  expect(app.shows(note(id))).toBe(false);
  expect(app.shows("text:plan together")).toBe(true);
});

test("both notices can coexist; generic failure does not claim named agents did not run or remove public errors", async () => {
  const w = fixture();
  const coder = w.agent("acme", { handle: "coder", name: "Coder" });
  const text = "  🌿 @agent @coder please review  ";
  w.replies.set(text, (ev, key) => {
    w.f.post(key, { kind: "error", actor: "system", text: "The agent could not start." });
    return { status: 202, body: { ...ev, addressing_note: generic, routing_note: unavailable } };
  });
  const app = await launch(w);
  await space(app);
  await app.press("id:mention");
  await app.waitFor("id:pick:agent");
  await app.keyboard.press("escape");
  await send(app, text);
  await app.waitUntil(() => w.sent.has(text));
  const id = w.sent.get(text)!.id;
  await app.waitFor(`id:event:${id} >> ${note(id)}`);
  expect(w.messages()[0]!.body).toEqual({
    text, invoke_agent: false, infer_text_mentions: false,
    mentions: [{ kind: "agent" }, { kind: "agent", agent_id: coder.id }],
    spans: [{ start: 7, end: 13, index: 0 }, { start: 14, end: 20, index: 1 }],
  });
  expect(app.shows("text:/No agent ran for @agent.*asked on their own/")).toBe(true);
  expect(app.texts().join(" ")).not.toContain("Nothing was started");
  expect(app.shows(`id:routing-note:${id} >> text:One agent turn couldn’t start`)).toBe(true);
  expect(app.shows("text:/No other model was used for this turn\\./")).toBe(true);
  expect(app.shows(`id:addressing-note-ask:${id}`)).toBe(false);
  const error = w.f.state.events.get("acme/lobby")!.at(-1)!;
  await app.waitFor(`id:failure:${error.id}`);
  await app.press(`id:routing-note-settings:${id}`);
  await app.waitFor("id:space-preferences");
});

test("direct Chat accepts a routing note only; Save note, Pages and restart do not replay it", async () => {
  const w = fixture();
  w.reply("route this", { addressing_note: pickup, routing_note: routing });
  w.reply("keep private", { addressing_note: pickup, routing_note: routing });
  const app = await launch(w);
  await send(app, "route this");
  await app.waitUntil(() => w.sent.has("route this"));
  const id = w.sent.get("route this")!.id;
  await app.waitFor(`id:routing-note:${id}`);
  expect(app.shows(`id:addressing-note-ask:${id}`)).toBe(false);
  expect(app.shows(`id:addressing-note-settings:${id}`)).toBe(false);
  expect(w.messages()[0]!.body).toEqual({ text: "route this", invoke_agent: true });
  await app.press(`id:routing-note-settings:${id}`);
  await app.waitFor("id:routing-settings"); // same destination via the Settings alias
  await app.press("id:settings-close");

  await send(app, "keep private", "save-note");
  await app.waitFor("text:keep private");
  expect(w.messages()[1]!.body).toEqual({ text: "keep private", save_note: true });
  expect(app.shows(note(w.sent.get("keep private")!.id))).toBe(false);
  await app.press("id:nav-pages");
  expect(app.shows(note(id))).toBe(false);
  await app.press("id:nav-chat");
  await app.waitFor(note(id));
  expect(JSON.stringify(app.storage())).not.toContain(routing.message);

  const reopened = await launch(w, { storage: app.storage() });
  await reopened.waitFor("text:route this");
  expect(reopened.shows(note(id))).toBe(false);
});

test("replayed response ids, foreign authors, wrong variants and injected transcript fields never create notices", async () => {
  const w = fixture();
  const old = w.f.post("acme/lobby", { kind: "message", actor: "Same display name", text: "same old text" });
  Object.assign(old, { actor_id: "alice", created_at: time, addressing_note: pickup, routing_note: routing });
  w.replies.set(old.text, () => ({ status: 202, body: { ...old } }));
  w.reply("foreign author", { actor_id: "bob", routing_note: routing });
  w.reply("wrong place", { text: "different message", routing_note: routing });
  w.reply("invalid shapes", { addressing_note: routing, routing_note: { ...unavailable, message: { text: "not plain text" } } });
  w.reply("conflicting codes", { addressing_note: pickup, routing_note: routing });
  w.reply("overlong or unknown", {
    addressing_note: { ...generic, message: "!".repeat(1_201) }, routing_note: { ...unavailable, source: "unknown" },
  });
  w.replies.set("denied", (ev) => ({ status: 409, body: { ...ev, addressing_note: pickup, routing_note: routing } }));
  const app = await launch(w);
  await space(app);
  await app.waitFor(`id:event:${old.id}`);
  expect(app.shows(note(old.id))).toBe(false);
  for (const text of [old.text, "foreign author", "wrong place", "invalid shapes", "conflicting codes", "overlong or unknown", "denied"]) {
    await send(app, text);
    await app.waitUntil(() => w.sent.has(text));
    await app.waitFor(`id:event:${w.sent.get(text)!.id}`);
    expect(app.shows(note(w.sent.get(text)!.id))).toBe(false);
    expect(app.shows(note(old.id))).toBe(false);
  }
});

test("receipt waits for the poll, rejects a text mismatch, and honors Rust's exact trim without changing U11's body", async () => {
  const w = fixture();
  const app = await launch(w);
  await space(app);
  w.f.answerLate("/v1/spaces/acme/rooms/lobby/events", TOKENS.alice!, 2_000);
  const exact = "\u0085 🌿 canonical, including BOM\uFEFF\u0085";
  w.reply(exact, { addressing_note: pickup });
  w.replies.set("sent body", (ev) => {
    const response = { ...ev, addressing_note: pickup };
    ev.text = "different polled body";
    return { status: 202, body: response };
  });
  await send(app, "sent body");
  await app.waitUntil(() => w.sent.has("sent body") && app.value("id:composer") === "");
  const wrong = w.sent.get("sent body")!.id;
  expect(app.shows(note(wrong))).toBe(false);
  await send(app, exact);
  await app.waitUntil(() => w.sent.has(exact));
  const good = w.sent.get(exact)!.id;
  expect(w.messages().at(-1)!.body.text).toBe(exact);
  expect(app.shows(note(good))).toBe(false);
  await app.waitFor(`id:event:${wrong} >> text:different polled body`);
  await app.waitFor(note(good));
  expect(w.sent.get(exact)!.text).toBe("🌿 canonical, including BOM\uFEFF");
  expect(app.shows(note(wrong))).toBe(false);
});

test("missing, noncanonical or different timestamps never corroborate even when the polled row came first", async () => {
  const w = fixture();
  const app = await launch(w);
  await space(app);
  const bad = "2026-02-30T00:00:00.000Z";
  const inputs = [
    { text: "missing both", post: undefined, row: undefined },
    { text: "invalid both", post: bad, row: bad },
    { text: "invalid poll only", post: "2026-03-02T00:00:00.000Z", row: bad },
    { text: "not UTC canonical", post: time, row: "2026-11-25T08:00:00.000+08:00" },
    { text: "different instants", post: "2026-11-25T00:00:01.000Z", row: time },
  ];
  for (const { text, post, row } of inputs) {
    w.replies.set(text, (ev) => {
      Object.assign(ev, { created_at: row });
      return { status: 202, delay: 3_000, body: { ...ev, created_at: post, addressing_note: pickup } };
    });
    await send(app, text);
    await app.waitUntil(() => w.sent.has(text));
    const id = w.sent.get(text)!.id;
    await app.waitFor(`id:event:${id}`);
    expect(app.shows(note(id))).toBe(false);
    await app.clock!.runFor(3_000);
    expect(app.shows(note(id))).toBe(false);
  }
});

test("the server's typed detail is plain text and cannot set the Settings destination", async () => {
  const w = fixture();
  const message = 'Model <a href="https://invalid.example/">needs attention</a>.';
  w.reply("inspect connection", { routing_note: { ...routing, message: message + "\u202e", href: "https://invalid.example/" } });
  const app = await launch(w);
  await send(app, "inspect connection");
  await app.waitUntil(() => w.sent.has("inspect connection"));
  const id = w.sent.get("inspect connection")!.id;
  await app.waitFor(`id:routing-note:${id}`);
  expect(app.shows(`text:${message}`)).toBe(true);
  await app.press(`id:routing-note-settings:${id}`);
  await app.waitFor("id:routing-settings");
  expect(app.requests().some(req => req.url.startsWith("https://invalid.example/"))).toBe(false);
});

test("a delayed POST stays tied to its Space and room, not a matching event id or draft elsewhere", async () => {
  const w = fixture();
  w.reply("original", { addressing_note: pickup }, 4_000);
  const app = await launch(w);
  await space(app);
  await send(app, "original");
  await app.waitUntil(() => w.sent.has("original"));
  const original = w.sent.get("original")!;
  w.f.state.events.get("side/lobby")!.push({ ...original, text: "same id in another Space" });
  await space(app, "side");
  await app.waitFor(`id:event:${original.id}`);
  await app.type("id:composer", "work in Side");
  await app.clock!.runFor(5_000);
  expect(app.shows(note(original.id))).toBe(false);
  expect(app.value("id:composer")).toBe("work in Side");
  await space(app);
  await app.waitFor(note(original.id));
  expect(app.value("id:composer")).toBe("");
});

test("detected revocation scrubs existing and delayed notes even after the same Space is visible again", async () => {
  const w = fixture();
  w.reply("already visible", { addressing_note: pickup });
  w.reply("arrives after access returns", { routing_note: routing }, 8_000);
  const app = await launch(w);
  await space(app, "side");
  await send(app, "already visible");
  await app.waitUntil(() => w.sent.has("already visible"));
  const existing = w.sent.get("already visible")!.id;
  await app.waitFor(note(existing));
  await send(app, "arrives after access returns", "ask-agent");
  await app.waitUntil(() => w.sent.has("arrives after access returns"));
  const delayed = w.sent.get("arrives after access returns")!.id;
  w.f.state.spaces.get("side")!.members.delete("alice");
  await app.waitFor("id:space:side", { state: "gone", timeout: 3_000 });
  expect(app.shows(note(existing))).toBe(false);
  w.f.state.spaces.get("side")!.members.add("alice");
  // The next timed list refresh discovers the regrant. The delayed POST
  // cannot leak even if the old transcript is later loaded again.
  await app.clock!.runFor(12_000);
  await space(app, "side");
  await app.waitFor(`id:event:${delayed}`);
  expect(app.shows(note(existing))).toBe(false);
  expect(app.shows(note(delayed))).toBe(false);
});

test("losing a thread scrubs its notice without touching the Space lobby's", async () => {
  const w = fixture();
  const thread = { id: "t1", name: "Follow-up", parent_room_id: "lobby" };
  w.f.addRoom("acme", thread);
  w.reply("lobby note", { addressing_note: pickup });
  w.reply("thread note", { routing_note: unavailable });
  const app = await launch(w);
  await space(app);
  await send(app, "lobby note");
  await app.waitUntil(() => w.sent.has("lobby note"));
  const lobbyId = w.sent.get("lobby note")!.id;
  await app.waitFor(note(lobbyId));
  await app.press("id:room:t1");
  await app.waitFor("id:open-parent");
  await send(app, "thread note", "ask-agent");
  await app.waitUntil(() => w.sent.has("thread note"));
  const threadId = w.sent.get("thread note")!.id;
  await app.waitFor(note(threadId));

  const history = w.f.state.events.get("acme/t1")!;
  w.f.state.events.delete("acme/t1");
  w.f.state.rooms.set("acme", w.f.state.rooms.get("acme")!.filter(r => r.id !== "t1"));
  await app.waitFor("id:room:t1", { state: "gone", timeout: 3_000 });
  await app.waitFor(note(lobbyId));
  w.f.addRoom("acme", thread);
  w.f.state.events.set("acme/t1", history);
  await app.clock!.runFor(12_000);
  await app.press("id:room:t1");
  await app.waitFor(`id:event:${threadId}`);
  expect(app.shows(note(threadId))).toBe(false);
});

test("late response and existing sender-only text do not survive sign-out or switch to another member", async () => {
  const w = fixture();
  w.reply("already read", { routing_note: routing });
  w.reply("pending response", { addressing_note: pickup }, 8_000);
  const app = await launch(w);
  await space(app);
  await send(app, "already read", "ask-agent");
  await app.waitUntil(() => w.sent.has("already read"));
  const existing = w.sent.get("already read")!.id;
  await app.waitFor(note(existing));
  await send(app, "pending response");
  await app.waitUntil(() => w.sent.has("pending response"));
  const late = w.sent.get("pending response")!.id;
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  await signIn(app, "bob");
  await space(app);
  await app.waitFor(`id:event:${late}`);
  await app.clock!.runFor(9_000);
  expect(app.shows(note(existing))).toBe(false);
  expect(app.shows(note(late))).toBe(false);
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  await signIn(app);
  await space(app);
  await app.waitFor(`id:event:${existing}`);
  expect(app.shows(note(existing))).toBe(false);
  expect(app.shows(note(late))).toBe(false);
});

test("a successful workspace switch scrubs the old session before a late POST resolves", async () => {
  const w = fixture();
  w.reply("old session", { addressing_note: pickup }, 8_000);
  const app = await launch(w);
  await space(app);
  await send(app, "old session");
  await app.waitUntil(() => w.sent.has("old session"));
  const id = w.sent.get("old session")!.id;
  await app.press("id:account-menu");
  await app.press("id:add-server");
  await app.type("id:add-server-url", BASE);
  await app.type("id:add-server-name", "Bob's workspace");
  await app.type("id:add-server-token", TOKENS.bob!);
  await app.press("id:add-server-submit");
  await app.press("id:workspace-discard-switch");
  await app.waitFor("id:workspace-dialog", { state: "gone" });
  await space(app);
  await app.waitFor(`id:event:${id}`);
  await app.clock!.runFor(9_000);
  expect(app.shows(note(id))).toBe(false);
});

test("a held dispatch denial rechecks only its source after navigation and invalidates a concurrent receipt", async () => {
  const w = fixture();
  w.reply("old Side note", { addressing_note: pickup });
  w.reply("unrelated Acme note", { addressing_note: pickup });
  w.reply("concurrent Side send", { addressing_note: pickup }, 6_000);
  w.deny("held Side task", 403, 3_000);
  const app = await launch(w);
  await space(app);
  await send(app, "unrelated Acme note");
  await app.waitUntil(() => w.sent.has("unrelated Acme note"));
  const acme = w.sent.get("unrelated Acme note")!.id;
  await app.waitFor(note(acme));
  await space(app, "side");
  await send(app, "old Side note");
  await app.waitUntil(() => w.sent.has("old Side note"));
  const old = w.sent.get("old Side note")!.id;
  await app.waitFor(note(old));
  await app.press("id:nav-chat");
  await dispatch(app, "held Side task");
  await app.waitUntil(() => w.denied.has("held Side task"));
  // No newer successful read of this room before the original denial.
  w.f.answerLate("/v1/spaces/side/rooms/lobby/events", TOKENS.alice!, 4_500);
  await space(app, "side");
  await send(app, "concurrent Side send");
  await app.waitUntil(() => w.sent.has("concurrent Side send"));
  const concurrent = w.sent.get("concurrent Side send")!.id;
  await space(app);
  await app.type("id:composer", "current Acme draft");
  w.f.state.spaces.get("side")!.members.delete("alice");
  await app.waitFor("id:space:side", { state: "gone", timeout: 5_000 });
  expect(app.shows(note(acme))).toBe(true);
  expect(app.value("id:composer")).toBe("current Acme draft");
  expect(app.shows("id:action-error")).toBe(false);
  expect(app.texts().join(" ")).not.toContain("held send denial");

  w.f.state.spaces.get("side")!.members.add("alice");
  await app.clock!.runFor(12_000);
  await space(app, "side");
  await app.waitFor(`id:event:${concurrent}`);
  expect(app.shows(note(old))).toBe(false);
  expect(app.shows(note(concurrent))).toBe(false);
  await app.press("id:nav-chat");
  expect(app.shows("id:dispatch-error")).toBe(false);
});

test("a held composer 404 is applied to its thread even when another room of the same Space is open", async () => {
  const w = fixture();
  w.f.addRoom("acme", { id: "t1", name: "Follow-up", parent_room_id: "lobby" });
  w.reply("lobby notice", { addressing_note: pickup });
  w.reply("thread notice", { addressing_note: pickup });
  w.deny("held thread send", 404, 3_000);
  const app = await launch(w);
  await space(app);
  await send(app, "lobby notice");
  await app.waitUntil(() => w.sent.has("lobby notice"));
  const lobby = w.sent.get("lobby notice")!.id;
  await app.waitFor(note(lobby));
  await app.press("id:room:t1");
  await send(app, "thread notice");
  await app.waitUntil(() => w.sent.has("thread notice"));
  const thread = w.sent.get("thread notice")!.id;
  await app.waitFor(note(thread));
  w.f.answerLate("/v1/spaces/acme/rooms/t1/events", TOKENS.alice!, 4_500);
  await send(app, "held thread send");
  await app.waitUntil(() => w.denied.has("held thread send"));
  await app.press("id:room:lobby");
  await app.type("id:composer", "a new lobby draft");
  await app.clock!.runFor(4_000);
  expect(app.shows(note(lobby))).toBe(true);
  expect(app.value("id:composer")).toBe("a new lobby draft");
  expect(app.shows("id:action-error")).toBe(false);
  await app.press("id:room:t1");
  await app.waitFor(`id:event:${thread}`);
  expect(app.shows(note(thread))).toBe(false);
  expect(app.value("id:composer")).toBe("held thread send"); // a denied POST keeps its draft
});

test("a delayed room send denial scrubs only its room; an older denial cannot erase a newer receipt", async () => {
  const w = fixture();
  w.reply("original Side note", { addressing_note: pickup });
  w.reply("replacement Side note", { routing_note: routing });
  w.deny("stale room denial", 404, 8_000);
  w.deny("first resolved denial", 403, 1_000);
  const app = await launch(w);
  await space(app, "side");
  await send(app, "original Side note");
  await app.waitUntil(() => w.sent.has("original Side note"));
  const old = w.sent.get("original Side note")!.id;
  await app.waitFor(note(old));
  await send(app, "stale room denial");
  await app.waitUntil(() => w.denied.has("stale room denial"));
  await app.press("id:nav-chat");
  await dispatch(app, "first resolved denial");
  await app.waitFor("id:dispatch-error");
  // The target is still a member's visible room. A POST denial could have
  // been for a model or file, so recheck and keep its public transcript.
  expect(w.f.state.log.some(req => req.path === "/v1/spaces/side/rooms")).toBe(true);
  await dispatch(app, "replacement Side note");
  await app.waitUntil(() => w.sent.has("replacement Side note"));
  const fresh = w.sent.get("replacement Side note")!.id;
  await space(app, "side");
  await app.waitFor(note(fresh));
  expect(app.shows(`id:event:${old}`)).toBe(true);
  expect(app.shows(note(old))).toBe(false);
  await app.clock!.runFor(8_000);
  expect(app.shows(note(fresh))).toBe(true);
  expect(app.shows("id:action-error")).toBe(false);
});

test("a delayed composer denial for a former account cannot clear the replacement account's receipt", async () => {
  const w = fixture();
  w.deny("Alice's held denial", 404, 8_000);
  w.reply("Bob's note", { addressing_note: pickup });
  const app = await launch(w);
  await space(app, "side");
  await send(app, "Alice's held denial");
  await app.waitUntil(() => w.denied.has("Alice's held denial"));
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  await signIn(app, "bob");
  await space(app, "side");
  await send(app, "Bob's note");
  await app.waitUntil(() => w.sent.has("Bob's note"));
  const id = w.sent.get("Bob's note")!.id;
  await app.waitFor(note(id));
  await app.clock!.runFor(8_000);
  expect(app.shows(note(id))).toBe(true);
  expect(app.shows("id:action-error")).toBe(false);
});

test("a delayed original POST denial cannot invalidate a newer same-room receipt", async () => {
  const w = fixture();
  w.deny("old dispatch denial", 404, 5_000);
  w.reply("new same-room receipt", { addressing_note: pickup });
  const app = await launch(w);
  await dispatch(app, "old dispatch denial");
  await app.waitUntil(() => w.denied.has("old dispatch denial"));
  await space(app, "side");
  await send(app, "new same-room receipt");
  await app.waitUntil(() => w.sent.has("new same-room receipt"));
  const id = w.sent.get("new same-room receipt")!.id;
  await app.waitFor(note(id));
  const probes = () => w.f.state.log.filter(r => r.method === "GET" && r.path === "/v1/spaces/side/rooms").length;
  const before = probes();
  await app.clock!.runFor(5_200);
  expect(app.shows(note(id))).toBe(true);
  expect(app.shows("id:action-error")).toBe(false);
  expect(probes()).toBe(before);
});

test("a real room loss between recheck legs outranks access verified before the room read", async () => {
  const w = fixture();
  w.f.addRoom("side", { id: "backup", name: "backup" });
  w.reply("visible during Space probe", { addressing_note: pickup });
  w.deny("start a recheck", 403, 300);
  const base = w.network[get]!;
  let firstLeg = false;
  let roomLeg = false;
  let revoked = false;
  w.network[get] = req => {
    const path = new URL(req.url).pathname;
    if (!firstLeg && w.denied.has("start a recheck") && path === "/v1/spaces") {
      firstLeg = true;
      return { ...(base(req) as object), delay: 5_000 };
    }
    if (firstLeg && revoked && path === "/v1/spaces/side/rooms") roomLeg = true;
    return base(req);
  };
  const app = await launch(w);
  await dispatch(app, "start a recheck");
  await app.waitUntil(() => firstLeg);
  await space(app, "side");
  await send(app, "visible during Space probe");
  await app.waitUntil(() => w.sent.has("visible during Space probe"));
  const id = w.sent.get("visible during Space probe")!.id;
  await app.waitFor(note(id));
  // Stop polling this room before removing it. The follow-up listRooms,
  // not a subsequent events denial, has to apply the loss.
  await app.press("id:nav-chat");
  revoked = true;
  w.f.state.rooms.set("side", w.f.state.rooms.get("side")!.filter(room => room.id !== "lobby"));
  w.f.state.events.delete("side/lobby");
  await app.waitUntil(() => roomLeg, { timeout: 6_000 });
  await app.clock!.runFor(100);
  await app.press("id:space:side");
  await app.waitFor("id:room:backup");
  expect(app.shows("id:room:lobby")).toBe(false);
  expect(app.shows(note(id))).toBe(false);
});

for (const probe of ["post-room", "post-space", "poll-space"] as const) {
  test(`${probe}: a held negative recheck cannot purge newer verified access and its corroborated notice`, async () => {
    const w = fixture();
    const oldText = `old ${probe}`;
    const newText = `new ${probe}`;
    w.reply(oldText, { addressing_note: pickup });
    w.reply(newText, { addressing_note: pickup });
    const base = w.network[get]!;
    let armed = false;
    let rechecking = false;
    let deniedPoll = false;
    const spaces = w.f.state.spaces.get("side")!;
    w.network[get] = req => {
      if (!armed) return base(req);
      const path = new URL(req.url).pathname;
      if (probe === "poll-space" && !deniedPoll && path === "/v1/spaces/side/rooms/lobby/events") {
        deniedPoll = true;
        spaces.members.delete("alice");
        return { status: 404, body: { error: "room not visible" } };
      }
      const spaceCheck = path === "/v1/spaces" && probe !== "post-room" && (deniedPoll || w.denied.has("held task"));
      const roomCheck = path === "/v1/spaces/side/rooms" && probe === "post-room" && w.denied.has("held task");
      if (!rechecking && (spaceCheck || roomCheck)) {
        rechecking = true;
        if (spaceCheck) {
          spaces.members.delete("alice");
          return { ...(base(req) as object), delay: 6_000 };
        }
        return { status: 200, body: { rooms: [] }, delay: 6_000 };
      }
      return base(req);
    };
    const app = await launch(w);
    await space(app, "side");
    await send(app, oldText);
    await app.waitUntil(() => w.sent.has(oldText));
    const old = w.sent.get(oldText)!.id;
    await app.waitFor(note(old));
    armed = true;
    if (probe !== "poll-space") {
      w.deny("held task", 403, 300);
      await app.press("id:nav-chat");
      await dispatch(app, "held task");
    }
    await app.waitUntil(() => rechecking);
    spaces.members.add("alice");
    if (probe !== "poll-space") await space(app, "side");
    expect(app.shows(note(old))).toBe(false);
    await send(app, newText);
    await app.waitUntil(() => w.sent.has(newText));
    const id = w.sent.get(newText)!.id;
    await app.waitFor(note(id));
    await app.clock!.runFor(6_200);
    expect(app.shows("id:space:side")).toBe(true);
    expect(app.shows(note(id))).toBe(true);
    expect(app.shows(note(old))).toBe(false);
    expect(app.shows("id:action-error")).toBe(false);
  });
}

for (const edit of [false, true]) {
  test(`accepted composer send retires only its draft and files after a concurrent denial (edited: ${edit})`, async () => {
    const w = fixture();
    w.deny("dispatch denial", 403, 2_000);
    w.reply("accepted with a file", { addressing_note: pickup }, 4_500);
    const app = await launch(w);
    await space(app, "side");
    await app.press("id:nav-chat");
    await dispatch(app, "dispatch denial");
    await app.waitUntil(() => w.denied.has("dispatch denial"));
    w.f.answerLate("/v1/spaces/side/rooms/lobby/events", TOKENS.alice!, 3_000);
    await space(app, "side");
    app.onPick(() => [{ name: "posted.txt", type: "text/plain", data: new Uint8Array([65]) }]);
    await app.press("id:attach");
    await app.waitFor("id:staged:posted.txt");
    await send(app, "accepted with a file");
    await app.waitUntil(() => w.sent.has("accepted with a file"));
    const id = w.sent.get("accepted with a file")!.id;
    expect(app.shows("id:staged:posted.txt")).toBe(true);
    if (edit) {
      await app.clear("id:composer");
      await app.type("id:composer", "a later edit");
      app.onPick(() => [{ name: "later.txt", type: "text/plain", data: new Uint8Array([66]) }]);
      await app.press("id:attach");
      await app.waitFor("id:staged:later.txt");
    }
    await app.clock!.runFor(5_000);
    await app.waitFor(`id:event:${id}`);
    expect(app.shows(note(id))).toBe(false);
    expect(app.value("id:composer")).toBe(edit ? "a later edit" : "");
    expect(app.shows("id:staged:posted.txt")).toBe(false);
    expect(app.shows("id:staged:later.txt")).toBe(edit);
    expect(w.uploads).toEqual(["posted.txt"]);
    expect(w.messages().map(p => p.body.asset_ids)).toEqual([["asset-posted.txt"]]);
  });
}

test("accepted files retire by key after a Space is revoked and rejoined, without touching newer staging or drafts", async () => {
  const w = fixture();
  const text = "accepted before losing access";
  w.reply(text, { addressing_note: pickup }, 9_000);
  const app = await launch(w);
  const pick = async (name: string) => {
    app.onPick(() => [{ name, type: "text/plain", data: new Uint8Array([65]) }]);
    await app.press("id:attach");
    await app.waitFor(`id:staged:${name}`);
  };
  await space(app);
  await app.type("id:composer", "unrelated Acme draft");
  await pick("unrelated.txt");
  await space(app, "side");
  await pick("accepted.txt");
  // Let the next ordinary /spaces refresh happen after revocation,
  // but before the accepted POST reply's ten-second timeout.
  await app.clock!.runFor(5_000);
  await send(app, text);
  await app.waitUntil(() => w.sent.has(text));
  const id = w.sent.get(text)!.id;
  const members = w.f.state.spaces.get("side")!.members;
  members.delete("alice");
  await app.waitFor("id:space:side", { state: "gone", timeout: 3_000 });
  members.add("alice");
  await app.waitFor("id:space:side", { timeout: 6_000 });
  await space(app, "side");
  await app.waitFor(`id:event:${id}`);
  await app.waitFor("id:staged:accepted.txt");
  await pick("later.txt");
  await app.type("id:composer", "a newer Side draft");
  await app.waitFor("id:staged:accepted.txt", { state: "gone", timeout: 6_000 });
  expect(app.shows("id:staged:later.txt")).toBe(true);
  expect(app.value("id:composer")).toBe("a newer Side draft");
  expect(app.shows(note(id))).toBe(false);
  expect(w.uploads).toEqual(["accepted.txt"]);
  expect(w.messages().map(p => p.body.asset_ids)).toEqual([["asset-accepted.txt"]]);
  await space(app);
  expect(app.shows("id:staged:unrelated.txt")).toBe(true);
  expect(app.value("id:composer")).toBe("unrelated Acme draft");
});

test("a late accepted Space POST never retires a replacement account's draft or staging", async () => {
  const w = fixture();
  w.reply("Alice's accepted send", { addressing_note: pickup }, 8_000);
  const app = await launch(w);
  await space(app, "side");
  app.onPick(() => [{ name: "same-name.txt", type: "text/plain", data: new Uint8Array([65]) }]);
  await app.press("id:attach");
  await app.waitFor("id:staged:same-name.txt");
  await send(app, "Alice's accepted send");
  await app.waitUntil(() => w.sent.has("Alice's accepted send"));
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  await signIn(app, "bob");
  await space(app, "side");
  await app.type("id:composer", "Bob's draft");
  app.onPick(() => [{ name: "same-name.txt", type: "text/plain", data: new Uint8Array([66]) }]);
  await app.press("id:attach");
  await app.waitFor("id:staged:same-name.txt");
  await app.clock!.runFor(8_200);
  expect(app.value("id:composer")).toBe("Bob's draft");
  expect(app.shows("id:staged:same-name.txt")).toBe(true);
  expect(w.messages()).toHaveLength(1);
  expect(w.uploads).toEqual(["same-name.txt"]);
});

test("a held positive Space list cannot resurrect a Space after newer denial, but a fresh list can", async () => {
  const w = fixture();
  w.deny("request denied after snapshot", 404, 300);
  const base = w.network[get]!;
  let armed = false;
  let held = false;
  w.network[get] = req => {
    if (armed && !held && new URL(req.url).pathname === "/v1/spaces") {
      held = true;
      // Capture the actual membership before changing it, then hold the reply.
      return { ...(base(req) as object), delay: 6_000 };
    }
    return base(req);
  };
  const app = await launch(w);
  await space(app);
  await space(app, "side"); // dispatch can target the already-cached room
  await app.press("id:nav-chat");
  armed = true;
  await app.waitUntil(() => held, { timeout: 12_000 });
  await dispatch(app, "request denied after snapshot");
  await app.waitUntil(() => w.denied.has("request denied after snapshot"));
  const members = w.f.state.spaces.get("side")!.members;
  members.delete("alice");
  await app.waitFor("id:space:side", { state: "gone", timeout: 2_000 });
  await space(app);
  await app.type("id:composer", "newer Acme draft");
  await app.clock!.runFor(6_200);
  expect(app.shows("id:space:side")).toBe(false);
  expect(app.value("id:composer")).toBe("newer Acme draft");
  expect(app.shows("text:hello from bob in acme")).toBe(true);
  members.add("alice");
  await app.clock!.runFor(12_000);
  await app.waitFor("id:space:side");
  expect(app.value("id:composer")).toBe("newer Acme draft");
  await space(app, "side");
});

for (const save of [false, true]) {
  test(`an accepted Chat ${save ? "Save note" : "Send"} cleans up without overriding a newer poll denial`, async () => {
    const w = fixture();
    const text = save ? "private note with a file" : "direct send with a file";
    w.reply(text, { routing_note: routing }, 4_000);
    const base = w.network[get]!;
    let inaccessible = false;
    let denies = 0;
    w.network[get] = req => {
      if (inaccessible && new URL(req.url).pathname === "/v1/chat/events") {
        denies++;
        return { status: 403, body: { error: "temporarily unavailable" } };
      }
      return base(req);
    };
    const app = await launch(w);
    await app.waitFor("id:chat-empty");
    app.onPick(() => [{ name: "chat.txt", type: "text/plain", data: new Uint8Array([65]) }]);
    await app.press("id:attach");
    await app.waitFor("id:staged:chat.txt");
    await send(app, text, save ? "save-note" : "send");
    await app.waitUntil(() => w.sent.has(text));
    const id = w.sent.get(text)!.id;
    if (save) {
      // A whitespace-only later edit is a different raw draft.
      await app.clear("id:composer");
      await app.type("id:composer", `${text} `);
    }
    inaccessible = true;
    await app.waitFor("text:/Your Chat isn't available/");
    expect(denies).toBe(1);
    await app.clock!.runFor(4_200);
    expect(app.shows("text:/Your Chat isn't available/")).toBe(true);
    expect(app.value("id:composer")).toBe(save ? `${text} ` : "");
    expect(app.shows("id:staged:chat.txt")).toBe(false);
    expect(w.uploads).toEqual(["chat.txt"]);
    expect(w.messages()).toHaveLength(1);
    inaccessible = false;
    await app.clock!.runFor(12_000);
    await app.waitFor(`id:event:${id}`);
    expect(app.shows(note(id))).toBe(false);
    expect(app.shows("text:/Your Chat isn't available/")).toBe(false);
    expect(app.value("id:composer")).toBe(save ? `${text} ` : "");
    expect(w.messages()).toHaveLength(1);
  });
}

test("an accepted Dispatch does not clear a whitespace-only later edit", async () => {
  const w = fixture();
  w.reply("dispatch draft", {}, 3_000);
  const app = await launch(w);
  await dispatch(app, "dispatch draft");
  await app.waitUntil(() => w.sent.has("dispatch draft"));
  await app.clear("id:dispatch-text");
  await app.type("id:dispatch-text", "dispatch draft ");
  await app.waitFor("id:dispatch-notice");
  expect(app.value("id:dispatch-text")).toBe("dispatch draft ");
  expect(w.messages()).toHaveLength(1);
});

test("a classified routing receipt needs matching canonical recipients in the echo and the rendered row", async () => {
  const w = fixture();
  const coder = w.agent("acme", { handle: "coder", name: "Coder" });
  const pick = { kind: "agent", agent_id: coder.id, addressed_by: "classifier" };
  const cases = [
    { text: "valid classifier pick", echo: [pick], row: [pick], show: true },
    { text: "different recipient", echo: [pick], row: [{ ...pick, agent_id: "ag-other" }] },
    { text: "lost classifier", echo: [pick], row: [{ kind: "agent", agent_id: coder.id }] },
    { text: "unknown attribution", echo: [pick], row: [{ ...pick, addressed_by: "other" }] },
    { text: "noncanonical recipient", echo: [{ ...pick, agent_id: ` ${coder.id}` }], row: [{ ...pick, agent_id: ` ${coder.id}` }] },
    { text: "unaddressed row", echo: [pick], row: [], audience: "room" },
    { text: "contradictory audience", echo: [pick], row: [pick], audience: "room" },
    { text: "duplicate recipient", echo: [pick], row: [pick, pick] },
  ];
  const app = await launch(w);
  await space(app);
  for (const input of cases) {
    w.replies.set(input.text, ev => {
      Object.assign(ev, { mentions: input.row, audience: input.audience ?? "agent" });
      return { status: 202, body: { ...ev, mentions: input.echo, audience: "agent", routing_note: routing } };
    });
    await send(app, input.text);
    await app.waitUntil(() => w.sent.has(input.text) && app.value("id:composer") === "");
    const id = w.sent.get(input.text)!.id;
    await app.waitFor(`id:event:${id}`);
    expect(app.shows(note(id))).toBe(input.show ?? false);
  }
  // Even a matching echo and poll cannot override the POST's explicit
  // generic recipient with a different agent.
  w.replies.set("explicit generic ask", ev => {
    Object.assign(ev, { mentions: [{ kind: "agent", agent_id: coder.id }] });
    return { status: 202, body: { ...ev, routing_note: routing } };
  });
  await send(app, "explicit generic ask", "ask-agent");
  await app.waitUntil(() => w.sent.has("explicit generic ask") && app.value("id:composer") === "");
  const id = w.sent.get("explicit generic ask")!.id;
  await app.waitFor(`id:event:${id}`);
  expect(app.shows(note(id))).toBe(false);
});

for (const account of ["123", 123] as const) {
  test(`legacy numeric display IDs still render "you" without granting private identity (${typeof account})`, async () => {
    const w = fixture();
    const base = w.network[get]!;
    w.network[get] = req => new URL(req.url).pathname === "/v1/me"
      ? { status: 200, body: { id: account } } : base(req);
    const app = await launch(w);
    await space(app);
    for (const rawActor of ["123", 123] as const) {
      const text = `raw actor ${typeof rawActor}`;
      w.replies.set(text, ev => {
        Object.assign(ev, { actor_id: rawActor, created_at: typeof rawActor === "number" ? "2026-11-26T00:00:00.000Z" : time });
        return { status: 202, body: { ...ev, actor_id: "123", addressing_note: pickup } };
      });
      await send(app, text);
      await app.waitUntil(() => w.sent.has(text) && app.value("id:composer") === "");
      const id = w.sent.get(text)!.id;
      await app.waitFor(`id:you:${id}`);
      expect(app.shows(note(id))).toBe(typeof account === "string" && typeof rawActor === "string");
    }
  });
}

test("oldest uncorroborated receipts are evicted across rooms without starving a new receipt", async () => {
  const w = fixture();
  const base = w.network[get]!;
  const release = new Set<number>();
  w.network[get] = req => {
    const url = new URL(req.url);
    if (url.pathname !== "/v1/spaces/acme/rooms/lobby/events") return base(req);
    const after = Number(url.searchParams.get("after"));
    const events = w.f.state.events.get("acme/lobby")!;
    return { status: 200, body: { events: events.filter((e, i) => e.id > after && (i === 0 || release.has(e.id))) } };
  };
  const app = await launch(w);
  await space(app);
  for (let i = 0; i < 32; i++) {
    const text = `idle receipt ${i}`;
    w.reply(text, { addressing_note: pickup });
    await send(app, text);
    await app.waitUntil(() => w.sent.has(text) && app.value("id:composer") === "");
  }
  const oldest = w.sent.get("idle receipt 0")!.id;
  const retained = w.sent.get("idle receipt 31")!.id;
  expect(app.shows(note(oldest))).toBe(false);
  await space(app, "side");
  w.reply("new room receipt", { addressing_note: pickup });
  await send(app, "new room receipt");
  await app.waitUntil(() => w.sent.has("new room receipt"));
  await app.waitFor(note(w.sent.get("new room receipt")!.id));
  release.add(oldest);
  release.add(retained);
  await space(app);
  await app.waitFor(`id:event:${oldest}`);
  await app.waitFor(note(retained));
  expect(app.shows(note(oldest))).toBe(false);
});
