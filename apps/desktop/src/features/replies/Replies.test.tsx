import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";
import { decodeEvent } from "../../api/client";
import { readPostedNotes } from "../senderNotices/response";

const parent = "/v1/spaces/acme/rooms/lobby";
const thread = "/v1/spaces/acme/rooms/reply-room";
const date = "2026-11-26T12:00:00.000Z";
const agent = { id: "coder", handle: "coder", name: "Coder", color: "#4f46e5" };

async function fixture(width = 1300) {
  const f = world();
  f.addRoom("acme", { id: "delegated", name: "Delegated", parent_room_id: "lobby" });
  const anchor = f.post("acme/lobby", {
    kind: "agent", actor: "agent", text: "The anchor response",
    activity: { id: "run-coder", name: "Coder", source: "agent", agent } as any,
  });
  const replyRoom = { id: "reply-room", name: `reply-${anchor.id}`, parent_room_id: "lobby", anchor_event_id: anchor.id };
  const sends: Record<string, unknown>[] = [];
  let opens = 0, denied = false;
  const members = () => f.state.spaces.get("acme")!.members.has("alice");
  const network = { ...f.network };
  network[`POST ${BASE}/v1/*`] = req => {
    const path = new URL(req.url).pathname;
    if (path === `${parent}/events/${anchor.id}/thread`) {
      if (!members() || denied) return { status: 404, body: { error: "no such room" } };
      const exists = f.state.events.has("acme/reply-room");
      if (!exists) f.addRoom("acme", replyRoom);
      opens++;
      return { status: exists ? 200 : 201, body: replyRoom };
    }
    if (path === `${thread}/messages`) {
      if (!members() || denied) return { status: 404, body: { error: "no such room" } };
      const body = JSON.parse(req.body ?? "{}");
      sends.push(body);
      const ev = f.post("acme/reply-room", {
        kind: "message", actor: "alice", actor_id: "alice", text: body.text, reply_to: body.reply_to, created_at: date,
      } as any);
      if (body.reply_to_agent !== false && body.reply_to === anchor.id) {
        f.post("acme/reply-room", {
          kind: "agent", actor: "agent", text: "New agent reply",
          activity: { id: "run-reply", name: "Coder", source: "agent", agent },
        } as any);
      }
      return { status: 200, body: ev };
    }
    return f.network[`POST ${BASE}/v1/*`]!(req);
  };
  network[`GET ${BASE}/v1/*`] = req => {
    const path = new URL(req.url).pathname;
    if (path === `${thread}/events` && denied) return { status: 404, body: { error: "no such room" } };
    const result = f.network[`GET ${BASE}/v1/*`]!(req) as { status: number; body: { events: any[] } };
    if (path !== `${parent}/events` || result.status !== 200 || !f.state.events.has("acme/reply-room")) return result;
    const messages = f.state.events.get("acme/reply-room")!.filter(e => e.kind === "message" || e.kind === "agent");
    const last = messages.at(-1);
    return { status: 200, body: { events: result.body.events.map(e => e.id !== anchor.id ? e : {
      ...e, replies: { room_id: replyRoom.id, count: messages.length, last_at: last ? date : null, participant_ids: last ? ["alice"] : [] },
    }) } };
  };
  const app = await open("../../../App.tsx", { modules: launched(), network: network as any, platform: "linux", width, height: 840, clock: {} });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor(`id:reply:${anchor.id}`);
  return { app, f, anchor, sends, opens: () => opens, deny: () => { denied = true; } };
}

test("creates and reopens the same thread, showing the anchor, count and participant without adding a sidebar room", async () => {
  const { app, anchor, opens, sends } = await fixture();
  await app.type("id:composer", "main room draft");
  await app.press(`id:react:${anchor.id}`);
  await app.keyboard.press("escape");
  await app.keyboard.press("right"); // React → Reply in the message toolbar
  await app.keyboard.press("enter");
  await app.waitFor(`id:thread-aside >> id:anchor:${anchor.id}`);
  expect(app.shows("id:resize-thread")).toBe(true);
  await app.waitFor("id:reply-agent-chip");
  expect(app.shows("label:Don't ask Coder to reply")).toBe(true);
  await app.type("id:composer", "Tell me more");
  await app.press("id:send");
  await app.waitFor("text:New agent reply");
  await app.waitFor(`label:/^Open thread, 2 replies/`);
  expect(sends[0]).toMatchObject({ reply_to: anchor.id, text: "Tell me more" });
  expect(sends[0]!.reply_to_agent).toBe(undefined);
  expect(app.shows("text:Coder · New agent reply")).toBe(true);
  await app.waitFor(`id:reply-participant:${anchor.id}:alice`);
  expect(app.shows("id:room:reply-room")).toBe(false);
  expect(app.shows("id:room:delegated")).toBe(true);
  await app.keyboard.press("escape");
  await app.waitFor("id:thread-aside", { state: "gone" });
  expect(app.value("id:composer")).toBe("main room draft");
  await app.keyboard.press("enter"); // focus returned to the Reply button
  await app.waitFor("id:thread-aside");
  await app.press("id:close-thread");
  await app.press(`id:replies:${anchor.id}`);
  await app.waitFor("text:New agent reply");
  expect(opens()).toBe(3);
});

test("removing the implicit agent keeps the exact quote and opts out without erasing the draft", async () => {
  const { app, anchor, sends, f } = await fixture();
  await app.press(`id:reply:${anchor.id}`);
  await app.waitFor("id:reply-agent-chip");
  await app.type("id:composer", "People only please");
  await app.press("id:reply-agent-chip");
  expect(app.shows("id:reply-agent-chip")).toBe(false);
  expect(app.shows("text:Coder · The anchor response")).toBe(true);
  expect(app.value("id:composer")).toBe("People only please");
  await app.press("id:close-thread");
  await app.press(`id:reply:${anchor.id}`);
  await app.waitFor("id:composer-reply-quote");
  expect(app.shows("id:reply-agent-chip")).toBe(false);
  await app.press("id:send");
  await app.waitFor("text:People only please");
  expect(sends[0]).toMatchObject({ reply_to: anchor.id, reply_to_agent: false });
  const reply = f.state.events.get("acme/reply-room")![0]!;
  await app.press(`id:reply:${reply.id}`);
  expect(app.shows("text:alice · People only please")).toBe(true);
  expect(app.shows("id:reply-agent-chip")).toBe(false);
});

test("an access denial closes the narrow thread immediately; resizing chooses the aside without losing the draft", async () => {
  const { app, anchor, deny, sends } = await fixture(850);
  await app.press(`id:reply:${anchor.id}`);
  await app.waitFor("id:thread-dialog");
  expect(app.shows("label:Close thread")).toBe(true);
  expect(app.shows("id:thread-aside")).toBe(false);
  await app.type("id:composer", "Unsaved thread draft");
  await app.setViewport(1300, 840);
  await app.waitFor("id:thread-aside");
  expect(app.value("id:composer")).toBe("Unsaved thread draft");
  await app.setViewport(850, 840);
  await app.waitFor("id:thread-dialog");
  deny();
  await app.waitFor("id:thread-dialog", { state: "gone" });
  expect(app.shows(`id:anchor:${anchor.id}`)).toBe(false);
  expect(app.value("id:composer")).not.toBe("Unsaved thread draft");
  expect(sends.length).toBe(0);
});

test("legacy messages still render and cancelling Forward sends nothing; modern attribution and no-reply activity are visible", async () => {
  const { app, f } = await fixture();
  const old = f.state.events.get("acme/lobby")![0]!;
  expect(app.shows(`id:via-metro:${old.id}`)).toBe(false);
  expect(app.shows(`id:replies:${old.id}`)).toBe(false);
  await app.press(`id:forward:${old.id}`);
  await app.waitFor("id:forward-dialog");
  await app.keyboard.press("escape");
  await app.waitFor("id:forward-dialog", { state: "gone" });
  expect(f.posts().some(p => p.path === "/v1/forward")).toBe(false);
  const modern = f.post("acme/lobby", { kind: "message", actor: "bob", actor_id: "bob", text: "A message from Chat", via: "metro",
    forwarded: { author_name: "Alex", author_kind: "person", sent_at: date, same_space: { room_id: "lobby", event_id: old.id } },
  } as any);
  await app.waitFor(`id:via-metro:${modern.id}`);
  expect(app.shows("label:Forwarded from Alex in #lobby")).toBe(true);
  f.post("acme/lobby", { kind: "activity", actor: "agent", text: "",
    activity: { id: "no-reply", name: "Coder", source: "agent", phase: "completed", outcome: "no_reply" },
  } as any);
  await app.waitFor("text:Coder chose not to reply");
});

test("focused message rows support R for reply and F for forward", async () => {
  const { app, anchor } = await fixture();
  const focusRow = async () => {
    await app.press(`id:react:${anchor.id}`);
    await app.keyboard.press("escape");
    await app.keyboard.press("shift+tab"); // First toolbar item → its message.
  };
  await focusRow();
  await app.keyboard.press("f");
  await app.waitFor("id:forward-dialog");
  await app.keyboard.press("escape");
  await app.waitFor("id:forward-dialog", { state: "gone" });
  await app.wait(50); // Focus returns after the dialog leaves the host's focus stack.
  await app.keyboard.press("r");
  await app.waitFor(`id:anchor:${anchor.id}`);
});

test("wire fields tolerate legacy and malformed metadata; deleted-agent notice binds to the exact quoted event", () => {
  const base = { id: 42, actor: "Alice", kind: "message", text: "Hello" };
  expect(decodeEvent(base)).toEqual(base);
  expect(decodeEvent({ ...base, reply_to: -1, replies: { count: 2 }, forwarded: { author_kind: "agent" } })).toEqual(base);
  const posted = {
    ...base, actor_id: "alice", created_at: date, audience: "room", mentions: [], reply_to: 7,
    addressing_note: { kind: "reply_agent_unavailable", message: "That agent has left this Space." },
  };
  const sent = { kind: "space", text: "Hello", reply: { eventId: 7, askAgent: true } } as const;
  expect(readPostedNotes(posted, sent)).toMatchObject({ replyTo: 7, addressing: posted.addressing_note });
  expect(readPostedNotes({ ...posted, reply_to: 8 }, sent)).toBe(null);
  expect(readPostedNotes(posted, { ...sent, reply: { ...sent.reply, askAgent: false } })).toBe(null);
});
