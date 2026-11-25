import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS } from "../../../test/fakeServer";
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
  const reply = (text: string, fields: Record<string, unknown>, delay = 0) =>
    replies.set(text, (ev) => ({ status: 202, body: { ...ev, ...fields }, delay }));
  const deny = (text: string, status: 403 | 404, delay: number) =>
    denials.set(text, { status, delay, body: { error: "held send denial" } });
  w.network[post] = req => {
    const path = new URL(req.url).pathname;
    const body = req.body && JSON.parse(req.body);
    if (path.endsWith("/messages") && denials.has(body?.text)) {
      denied.add(body.text);
      return denials.get(body.text);
    }
    const answer = respond(req) as Reply;
    if (!path.endsWith("/messages") || answer.status !== 202) return answer;
    const call = w.f.posts().at(-1)!;
    const key = path.startsWith("/v1/chat/") ? `chat:${call.user}` :
      path.replace("/v1/spaces/", "").replace("/rooms/", "/").replace("/messages", "");
    const event = w.f.state.events.get(key)!.findLast(e => e.kind === "message" && e.text === body.text)!;
    const text = event.text.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");
    Object.assign(event, { actor: "Same display name", actor_id: call.user, text, created_at: time });
    sent.set(body.text, event);
    return replies.get(body.text)?.(event, key) ?? answer;
  };
  return { ...w, reply, replies, deny, denied, sent, messages: () => w.f.posts().filter(p => p.path.endsWith("/messages")) };
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
