// Threads of the personal Chat: created from the navigation or as a reply
// thread under a Chat message, each its own conversation; renamed and
// deleted (after a confirmation) from their menu. Real NSH engine, fake server.

import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";

type F = ReturnType<typeof world>;

async function inChat(f: F) {
  const app = await open("../../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux", width: 1200, height: 800 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:chat-view");
  return app;
}

const threadsOf = (f: F) => f.state.chatThreads.get("alice") ?? [];

test("a new Chat thread is its own conversation, listed under Chat", async () => {
  const f = world();
  const app = await inChat(f);
  await app.type("id:composer", "main chat words");
  await app.press("id:send");
  await app.waitFor("text:main chat words");

  await app.waitFor("id:new-chat-thread");
  await app.press("id:new-chat-thread");
  await app.type("id:new-chat-thread-title", "Trip plans");
  await app.press("id:create-chat-thread");
  const [thread] = threadsOf(f);
  expect(thread?.title).toBe("Trip plans");
  await app.waitFor(`id:chat-thread:${thread!.id}`);
  await app.waitFor("id:chat-view >> text:Trip plans");
  expect(app.shows("text:main chat words")).toBe(false);

  await app.type("id:composer", "thread only words");
  await app.press("id:send");
  await app.waitFor("text:thread only words");
  expect(f.posts().some(p => p.path === `/v1/chat/messages?thread=${thread!.id}` && p.body.text === "thread only words")).toBe(true);
  expect(f.state.events.get(`chat:alice:${thread!.id}`)!.some(e => e.text === "thread only words")).toBe(true);
  expect(f.state.events.get("chat:alice")!.some(e => e.text === "thread only words")).toBe(false);

  // Back in the main Chat, the thread's words aren't there.
  await app.press("id:nav-chat");
  await app.waitFor("text:Only you can open this chat in Metro.");
  await app.waitFor("text:main chat words");
  expect(app.shows("text:thread only words")).toBe(false);
});

test("Reply on a Chat message opens its thread, with the message above", async () => {
  const f = world();
  f.state.events.set("chat:alice", []);
  f.post("chat:alice", { kind: "message", actor: "alice", actor_id: "alice", text: "anchor message here" });
  const anchor = f.state.events.get("chat:alice")!.at(-1)!;
  const app = await inChat(f);
  await app.waitFor(`id:reply:${anchor.id}`);
  await app.press(`id:reply:${anchor.id}`);
  await app.waitUntil(() => threadsOf(f).length === 1);
  const thread = threadsOf(f)[0]!;
  expect(thread.anchor_event_id).toBe(anchor.id);
  await app.waitFor("id:chat-thread-anchor");
  await app.waitFor(`id:anchor:${anchor.id}`);
  await app.waitFor("id:chat-view >> text:anchor message here");
  // In the thread, messages take no further threads.
  await app.type("id:composer", "a reply");
  await app.press("id:send");
  await app.waitFor("text:a reply");
  const reply = f.state.events.get(`chat:alice:${thread.id}`)!.find(e => e.text === "a reply")!;
  expect(app.shows(`id:reply:${reply.id}`)).toBe(false);
  // Reply again from the main Chat opens the same thread.
  await app.press("id:nav-chat");
  await app.waitFor(`id:reply:${anchor.id}`);
  await app.press(`id:reply:${anchor.id}`);
  await app.waitFor("id:chat-thread-anchor");
  expect(threadsOf(f)).toHaveLength(1);
});

test("a Chat thread is renamed and, after a confirmation, deleted from its menu", async () => {
  const f = world();
  f.state.chatThreads.set("alice", [{ id: "ct-1", title: "Old name" }]);
  f.state.events.set("chat:alice:ct-1", [{ id: 5, kind: "message", actor: "alice", actor_id: "alice", text: "inside the thread" }]);
  const app = await inChat(f);
  await app.waitFor("id:chat-thread:ct-1");
  await app.press("id:chat-thread:ct-1");
  await app.waitFor("text:inside the thread");

  await app.press("id:chat-thread-actions");
  await app.press("id:chat-header-rename:ct-1");
  await app.waitFor("id:rename-chat-thread-title");
  await app.clear("id:rename-chat-thread-title");
  await app.type("id:rename-chat-thread-title", "New name");
  await app.press("id:rename-chat-thread-save");
  await app.waitFor("id:chat-view >> text:New name");
  expect(threadsOf(f)[0]!.title).toBe("New name");

  // Cancel keeps it.
  await app.press("id:chat-thread-actions");
  await app.press("id:chat-header-delete:ct-1");
  await app.waitFor("id:delete-chat-thread-confirm");
  await app.press("id:delete-chat-thread-cancel");
  await app.waitFor("id:delete-chat-thread-confirm", { state: "gone" });
  expect(threadsOf(f)).toHaveLength(1);

  await app.press("id:chat-thread-actions");
  await app.press("id:chat-header-delete:ct-1");
  await app.press("id:delete-chat-thread-confirm");
  await app.waitFor("id:chat-thread:ct-1", { state: "gone" });
  expect(threadsOf(f)).toHaveLength(0);
  expect(f.posts().some(p => p.method === "DELETE" && p.path === "/v1/chat/threads/ct-1")).toBe(true);
  // Back in the main Chat.
  await app.waitFor("text:Only you can open this chat in Metro.");
  expect(app.shows("text:inside the thread")).toBe(false);
});

test("a server without Chat threads shows no thread controls", async () => {
  const f = world();
  f.state.noChatThreads = true;
  f.state.events.set("chat:alice", []);
  f.post("chat:alice", { kind: "message", actor: "alice", actor_id: "alice", text: "plain chat" });
  const app = await inChat(f);
  await app.waitFor("text:plain chat");
  await app.wait(300);
  expect(app.shows("id:new-chat-thread")).toBe(false);
  const plain = f.state.events.get("chat:alice")!.at(-1)!;
  expect(app.shows(`id:reply:${plain.id}`)).toBe(false);
});

test("Queue and Steer work in a thread, and a reply running there leaves the main Chat free", async () => {
  const f = world();
  const ok = (body: unknown, status = 200) => ({ status, body });
  const live = { id: "chat_live", name: "Metro", source: "agent" as const };
  f.state.events.set("chat:alice", []);
  f.state.chatThreads.set("alice", [{ id: "ct-1", title: "Side" }]);
  f.state.events.set("chat:alice:ct-1", []);
  f.post("chat:alice:ct-1", { kind: "activity", actor: "agent", text: "", activity: { ...live, phase: "started" } });
  const sent: { thread: string | null; body: Record<string, unknown> }[] = [];
  let n = 0;
  for (const method of ["GET", "POST"]) {
    const key = `${method} ${BASE}/v1/*`;
    const fallback = f.network[key]!;
    f.network[key] = req => {
      const url = new URL(req.url);
      if (url.pathname === "/v1/chat/spaces-access" && method === "GET") return ok({ enabled: true, notice_shown: true, excluded_space_ids: [] });
      if (url.pathname === "/v1/chat/messages" && method === "POST") {
        const body = JSON.parse(req.body!);
        const thread = url.searchParams.get("thread");
        sent.push({ thread, body });
        const ev = f.post(thread ? `chat:alice:${thread}` : "chat:alice", { kind: "message", actor: "alice", actor_id: "alice", text: body.text, audience: "agent" });
        return ok({ ...ev, actor_id: "alice", turn_id: `chat_sent${++n}` }, 202);
      }
      return fallback(req);
    };
  }
  const app = await open("../../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux", width: 1200, height: 800 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:chat-view");
  // The main Chat isn't held up by the thread's reply.
  await app.waitFor("id:send");
  await app.wait(300);
  expect(app.shows("id:chat-stop")).toBe(false);

  await app.waitFor("id:chat-thread:ct-1");
  await app.press("id:chat-thread:ct-1");
  await app.waitFor("id:chat-stop");
  await app.waitFor("id:chat-delivery");
  await app.type("id:composer", "after that");
  await app.submit("id:composer");
  await app.waitUntil(() => sent.length === 1);
  expect(sent[0]).toMatchObject({ thread: "ct-1" });
  expect(sent[0]!.body.delivery).toBeUndefined();
  const queued = f.state.events.get("chat:alice:ct-1")!.at(-1)!.id;
  await app.waitFor(`id:queued:${queued}`);

  await app.press("id:chat-delivery:steer");
  await app.type("id:composer", "and the weekend");
  await app.submit("id:composer");
  await app.waitUntil(() => sent.length === 2);
  expect(sent[1]).toMatchObject({ thread: "ct-1", body: { delivery: "steer" } });
  const steer = f.state.events.get("chat:alice:ct-1")!.at(-1)!.id;
  f.post("chat:alice:ct-1", { kind: "steered", actor: "agent", text: "", activity: { ...live, steered: steer } as any });
  await app.waitFor(`id:steered:${steer}`);

  // Back in the main Chat: nothing running, nothing queued.
  await app.press("id:nav-chat");
  await app.waitFor("text:Only you can open this chat in Metro.");
  await app.wait(300);
  expect(app.shows("id:chat-stop")).toBe(false);
  expect(app.shows(`id:queued:${queued}`)).toBe(false);
});

test("a thread deleted elsewhere keeps the unsent draft in the main Chat and says so", async () => {
  const f = world();
  f.state.chatThreads.set("alice", [{ id: "ct-1", title: "Side" }]);
  f.state.events.set("chat:alice:ct-1", []);
  const app = await inChat(f);
  await app.type("id:composer", "main words");
  await app.waitFor("id:chat-thread:ct-1");
  await app.press("id:chat-thread:ct-1");
  await app.waitFor("id:chat-view >> text:Side");
  await app.waitUntil(() => app.value("id:composer") === "");
  await app.type("id:composer", "not sent yet");
  // Another device deletes it.
  f.state.chatThreads.set("alice", []);
  await app.waitFor("text:That thread was deleted.");
  await app.waitFor("id:chat-thread:ct-1", { state: "gone" });
  await app.waitFor("text:Only you can open this chat in Metro.");
  // Neither draft is lost.
  expect(app.value("id:composer")).toBe("main words\n\nnot sent yet");
  expect(f.posts().some(p => p.path.startsWith("/v1/chat/messages"))).toBe(false);
});
