import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";

const ok = (body: unknown, status = 200) => ({ status, body });
const turn = (id: string, phase?: string) => ({ id, name: "Metro", source: "agent" as const, phase });
const liveId = "chat_live";

/** A Chat with a running turn; sent messages wait (no reply), each with its own turn id. */
function setup(running = true) {
  const f = world();
  f.state.events.set("chat:alice", []);
  const addChat = (e: Parameters<typeof f.post>[1]) => f.post("chat:alice", e);
  if (running) addChat({ kind: "activity", actor: "agent", text: "", activity: turn(liveId, "started") });
  const sent: Record<string, unknown>[] = [], stopped: string[] = [];
  let n = 0;
  for (const method of ["GET", "POST"]) {
    const key = `${method} ${BASE}/v1/*`;
    const fallback = f.network[key]!;
    f.network[key] = req => {
      const p = new URL(req.url).pathname;
      if (p === "/v1/chat/spaces-access" && method === "GET") return ok({ enabled: true, notice_shown: true, excluded_space_ids: [] });
      if (p === "/v1/chat/messages" && method === "POST") {
        const body = JSON.parse(req.body!);
        sent.push(body);
        const ev = addChat({ kind: "message", actor: "alice", actor_id: "alice", text: body.text, audience: "agent" } as any);
        return ok({ ...ev, actor_id: "alice", turn_id: `chat_sent${++n}` }, 202);
      }
      const stop = /^\/v1\/chat\/turns\/([^/]+)\/stop$/.exec(p);
      if (stop && method === "POST") {
        stopped.push(stop[1]!);
        return ok({ turn_id: stop[1], status: "cancelled" });
      }
      return fallback(req);
    };
  }
  return { ...f, addChat, sent, stopped };
}

async function launch(f: ReturnType<typeof world>) {
  const app = await open("../../../App.tsx", { modules: launched(), network: f.network as any, platform: "linux", width: 1200, height: 800 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:chat-view");
  return app;
}

test("while Metro replies, Send stays beside Stop and queues by default; a queued message can be cancelled", async () => {
  const f = setup();
  const app = await launch(f);
  await app.waitFor("id:chat-stop");
  expect(app.shows("id:send")).toBe(true);
  await app.type("id:composer", "after that");
  await app.submit("id:composer");
  await app.waitUntil(() => f.sent.length === 1);
  expect(f.sent[0]!.delivery).toBeUndefined();
  expect(app.value("id:composer")).toBe("");
  const id = (f.state.events.get("chat:alice")!.at(-1) as { id: number }).id;
  await app.waitFor(`id:queued:${id}`);
  expect(app.shows("id:chat-stop")).toBe(true);
  await app.press(`id:unqueue:${id}`);
  await app.waitFor(`id:queued:${id}`, { state: "gone" });
  expect(f.stopped).toEqual(["chat_sent1"]);
  // The running turn keeps its Stop.
  expect(app.shows("id:chat-stop")).toBe(true);
});

test("Steer sends into the running reply and marks the message once it was delivered", async () => {
  const f = setup();
  const app = await launch(f);
  await app.waitFor("id:chat-delivery");
  expect(app.shows("id:chat-delivery:queue")).toBe(true);
  await app.press("id:chat-delivery:steer");
  await app.type("id:composer", "also the weekend");
  await app.submit("id:composer");
  await app.waitUntil(() => f.sent.length === 1);
  expect(f.sent[0]!.delivery).toBe("steer");
  const id = (f.state.events.get("chat:alice")!.at(-1) as { id: number }).id;
  // A steer that hasn't been delivered yet isn't called queued.
  await app.wait(300);
  expect(app.shows(`id:queued:${id}`)).toBe(false);
  const marker = f.addChat({ kind: "steered", actor: "agent", text: "", activity: { ...turn(liveId), steered: id } } as any);
  await app.waitFor(`id:steered:${id}`);
  // The marker itself is no transcript row, and the reply keeps running.
  expect(app.shows(`id:event:${marker.id}`)).toBe(false);
  expect(app.shows("id:chat-stop")).toBe(true);
  // The choice stays for the next message.
  await app.type("id:composer", "and Monday");
  await app.submit("id:composer");
  await app.waitUntil(() => f.sent.length === 2);
  expect(f.sent[1]!.delivery).toBe("steer");
});

test("a steer the running reply never took shows as queued behind the next turn", async () => {
  const f = setup();
  const app = await launch(f);
  await app.waitFor("id:chat-delivery");
  await app.type("id:composer", "queued first");
  await app.submit("id:composer");
  await app.waitUntil(() => f.sent.length === 1);
  const first = (f.state.events.get("chat:alice")!.at(-1) as { id: number }).id;
  await app.press("id:chat-delivery:steer");
  await app.type("id:composer", "steer late");
  await app.submit("id:composer");
  await app.waitUntil(() => f.sent.length === 2);
  const steer = (f.state.events.get("chat:alice")!.at(-1) as { id: number }).id;
  await app.waitFor(`id:queued:${first}`);
  expect(app.shows(`id:queued:${steer}`)).toBe(false);
  // The reply ends without taking it; the queued turn starts, and the
  // steer now waits its turn.
  f.addChat({ kind: "activity", actor: "agent", text: "", activity: turn(liveId, "completed") });
  f.addChat({ kind: "activity", actor: "agent", text: "", activity: turn("chat_sent1", "started") });
  await app.waitFor(`id:queued:${steer}`);
  await app.waitFor(`id:queued:${first}`, { state: "gone" });
});

test("with nothing running there is no Queue or Steer choice", async () => {
  const f = setup(false);
  const app = await launch(f);
  await app.waitFor("id:send");
  expect(app.shows("id:chat-delivery")).toBe(false);
  expect(app.shows("id:chat-stop")).toBe(false);
});
