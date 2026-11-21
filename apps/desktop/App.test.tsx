// Headless tests: the real NSH engine against an in-memory fake server
// (test/fakeServer.ts) through nsh test's network stubs. No real network.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, fakeServer, header, TOKENS, world } from "./test/fakeServer";

async function openApp(f: ReturnType<typeof fakeServer>, storage: Record<string, string> = {}) {
  return open("./App.tsx", { network: f.network as any, platform: "linux", width: 1200, height: 800, storage });
}

async function signIn(app: App, user: string, url = BASE) {
  await app.clear("id:server-url");
  await app.type("id:server-url", url);
  await app.type("id:access-token", TOKENS[user] ?? user);
  await app.press("id:connect");
}

async function openAcme(app: App) {
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
}

const text = (app: App) => app.texts().join(" | ");

test("refuses to send a token over plain HTTP to a remote host", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice", "http://192.168.1.20:8787");
  await app.waitFor("id:connect-error");
  expect(text(app)).toContain("Refusing to send your access token over plain HTTP");
  expect(app.requests()).toHaveLength(0);
});

test("an account without Spaces lands in its Chat with instructions, no implicit lobby", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "carol");
  await app.waitFor("id:onboarding");
  expect(app.shows("text:You're not in any Space yet")).toBe(true);
  expect(app.shows("id:onboarding-id")).toBe(true);
  expect(text(app)).toContain("carol");
  expect(f.state.log.some((l) => l.path.includes("/rooms"))).toBe(false);
  const first = app.requests()[0];
  expect(header(first as any, "authorization")).toBe(`Bearer ${TOKENS.carol}`);
  // Only the server URL is remembered; the token never reaches storage.
  await app.waitUntil(() => app.storage()["metro.serverUrl"] === BASE);
  expect(JSON.stringify(app.storage())).not.toContain(TOKENS.carol);
});

test("Spaces keep their own lobby, cursor and cache", async () => {
  const f = world();
  f.state.ignoreCursor = true; // the server repeats old events on every poll
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  expect(app.shows("text:hello from bob in acme")).toBe(false);

  f.post("acme/lobby", { kind: "message", actor: "bob", text: "while you were away" });
  await app.press("id:space:acme");
  await app.waitFor("text:while you were away");
  await app.wait(1800);
  expect(app.count("text:hello from bob in acme")).toBe(1);
  expect(f.state.afters.some((a) => a.startsWith("acme/lobby:1"))).toBe(true); // resumed from its cursor
});

test("threads are nested under their parent room and show where they came from", async () => {
  const f = world();
  f.addRoom("acme", { id: "t1", name: "fix-build", parent_room_id: "lobby" });
  f.post("acme/t1", { kind: "agent", actor: "agent", text: "child agent working on the build" });
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.press("id:room:t1");
  await app.waitFor("text:child agent working on the build");
  expect(app.shows("id:open-parent")).toBe(true);
  await app.press("id:open-parent");
  await app.waitFor("text:hello from bob in acme");
});

test("@mentions: pick a member, send, prune and clear on room change", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);

  await app.type("id:composer", "can you look @");
  await app.waitFor("id:mention-picker");
  await app.press("id:pick:bob");
  await app.waitFor("id:mention-chip:bob");
  expect(app.value("id:composer")).toBe("can you look @bob ");
  await app.press("id:send");
  await app.waitFor("text:can you look @bob");
  const sent = f.posts().find((p) => p.path.endsWith("/messages"))!;
  expect(sent.body).toEqual({ text: "can you look @bob", invoke_agent: false, mentions: [{ kind: "user", id: "bob" }] });
  expect(app.shows("text:to people")).toBe(true);
  expect(f.state.events.get("acme/lobby")!.some((e) => e.actor === "agent")).toBe(false); // a person only

  // A chip disappears when its text is deleted, and on room change.
  await app.press("id:mention");
  await app.press("id:pick:agent");
  await app.waitFor("id:mention-chip:agent");
  await app.clear("id:composer");
  await app.waitFor("id:mention-chip:agent", { state: "gone" });
  await app.press("id:mention");
  await app.press("id:pick:bob");
  await app.waitFor("id:mention-chip:bob");
  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  expect(app.shows("id:mention-chips")).toBe(false);
});

test("a message that mentions me is highlighted as @you", async () => {
  const f = world();
  const ev = f.post("acme/lobby", { kind: "message", actor: "bob", text: "@alice please review", mentions: [{ kind: "user", id: "alice" }, { kind: "agent" }], audience: "mixed" });
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.waitFor(`id:recipient:${ev.id}:alice`);
  expect(app.shows("text:@you")).toBe(true);
  expect(app.shows("text:to the agent and people")).toBe(true);
});

test("reactions: add and remove mine, fold the agent's acknowledgement, no extra rows", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  const target = f.state.events.get("acme/lobby")![0].id;

  await app.press(`id:react:${target}`);
  await app.press(`id:pick-reaction:${target}:👍`);
  await app.waitFor(`id:reaction:${target}:👍`);
  expect(f.posts().at(-1)).toMatchObject({ method: "PUT", path: `/v1/spaces/acme/rooms/lobby/events/${target}/reactions`, body: { emoji: "👍" } });

  f.react("acme/lobby", target, "✅", true, "agent", "agent");
  await app.waitFor(`id:reaction:${target}:✅`);
  expect(app.shows(`id:reaction:${target}:✅ >> text:agent`)).toBe(true);

  await app.press(`id:reaction:${target}:👍`);
  await app.waitFor(`id:reaction:${target}:👍`, { state: "gone" });
  expect(f.posts().at(-1)).toMatchObject({ method: "DELETE", body: { emoji: "👍" } });
  expect(text(app)).not.toContain("target_event_id");
});

test("Demo needs this Space's paired runner", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  expect(app.shows("text:Runner paired")).toBe(true);
  await app.press("id:run-demo");
  await app.waitFor("text:/README\\.md/");
  expect(f.posts().at(-1)).toMatchObject({ path: "/v1/spaces/acme/rooms/lobby/demo", body: {} });

  await app.press("id:space:side");
  await app.waitFor("text:No runner paired");
  expect(app.enabled("id:run-demo")).toBe(false);
});

test("owners manage members and see a runner token once; members can't manage", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.press("id:manage-space");
  await app.waitFor("id:member:bob");
  await app.type("id:add-member-id", "carol");
  await app.press("id:add-member");
  await app.waitFor("id:member:carol");
  await app.press("id:remove-member:carol");
  await app.waitFor("id:member:carol", { state: "gone" });

  await app.press("id:issue-runner-token");
  await app.waitFor("id:runner-token-value");
  expect(text(app)).toContain("secret-runner-token-");
  await app.press("id:close-manage"); // leaving the view forgets it
  await app.press("id:manage-space");
  await app.waitFor("id:runner-card");
  expect(text(app)).not.toContain("secret-runner-token-");

  await app.press("id:space:side"); // Bob owns Side; Alice is a member
  await app.waitFor("text:side lobby only");
  expect(app.shows("id:manage-space")).toBe(false);
});

test("losing membership purges the Space and returns to the Chat", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "bob");
  await openAcme(app);
  f.state.spaces.get("acme")!.members.delete("bob");
  await app.waitFor("text:You no longer have access to Acme.", { timeout: 15_000 });
  expect(app.shows("id:space:acme")).toBe(false);
  expect(app.shows("text:hello from bob in acme")).toBe(false);
  expect(app.shows("id:chat-view")).toBe(true);
});

test("Chat: personal messages, runs, follow-up to an explicit Space and room, stop", async () => {
  const f = world();
  f.state.runs.push({ id: "run1", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "Audit the build" });
  const app = await openApp(f);
  await signIn(app, "alice");
  await app.waitFor("id:chat-view");
  await app.type("id:composer", "what's running?");
  await app.press("id:ask-agent");
  await app.waitFor("text:(agent) re: what's running?");

  await app.waitFor("id:run:run1");
  expect(app.shows("text:Acme › #lobby")).toBe(true);
  await app.press("id:run-follow:run1");
  await app.waitFor("text:To: agent in Acme › #lobby");
  await app.type("id:dispatch-text", "also check the tests");
  await app.press("id:dispatch-send");
  await app.waitFor("id:dispatch-notice");
  expect(f.posts().at(-1)).toMatchObject({ path: "/v1/spaces/acme/rooms/lobby/messages", body: { text: "also check the tests", invoke_agent: true } });

  await app.press("id:run-stop:run1");
  await app.waitFor("text:stopped");
  expect(f.posts().at(-1)).toMatchObject({ path: "/v1/runs/run1/stop", body: {} });

  await app.press("id:run-open:run1");
  await app.waitFor("text:hello from bob in acme");
});

test("a rejected token returns to the connect screen with the reason", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "nobody");
  await app.waitFor("id:connect-error");
  expect(text(app)).toContain("rejected the access token (HTTP 401)");
  expect(app.value("id:access-token")).toBe("");
});

test("unreachable server: clear error, then retry banner and recovery mid-session", async () => {
  const f = world();
  f.state.down = true;
  const app = await openApp(f);
  await signIn(app, "alice");
  await app.waitFor("id:connect-error");
  expect(text(app)).toContain(`Cannot reach ${BASE}`);

  f.state.down = false;
  await signIn(app, "alice");
  await openAcme(app);
  f.state.down = true;
  await app.waitFor("id:connection-banner", { timeout: 10_000 });
  expect(app.shows("text:hello from bob in acme")).toBe(true);
  f.state.down = false;
  f.post("acme/lobby", { kind: "message", actor: "bob", text: "back online" });
  await app.press("id:retry-now");
  await app.waitFor("text:back online", { timeout: 10_000 });
  await app.waitFor("id:connection-banner", { state: "gone" });
});

test("long tool output starts folded and expands on demand", async () => {
  const f = world();
  const long = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join("\n");
  f.post("acme/lobby", { kind: "tool_result", actor: "runner", text: long });
  const app = await openApp(f);
  await signIn(app, "alice");
  await openAcme(app);
  await app.waitFor("text:Show all 30 lines");
  await app.press("text:Show all 30 lines");
  await app.waitFor("text:Show less");
});
