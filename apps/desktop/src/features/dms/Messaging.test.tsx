import { expect, open, test } from "nsh/test";
import { BASE, TOKENS, world } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";

const peer = { id: "person-1", display_name: "Dana", avatar_version: null };
const duplicate = { ...peer, id: "person-2" };
const other = { id: "person-3", display_name: "Jordan", avatar_version: null };
const date = "2026-11-26T12:00:00.000Z";
const message = (id: number, actor: typeof peer, text: string, reply_to?: number) => ({
  id, kind: "message", actor: actor.display_name, actor_id: actor.id, text, reply_to, created_at: date,
});
const sent = { ...peer, id: "alice", display_name: "Alice" };
const dmPath = "/v1/dms/dm-one";

async function fixture(width = 1280) {
  const f = world();
  f.addRoom("side", { id: "work", name: "Workshop" });
  f.addRoom("side", { id: "thread", name: "Design review", parent_room_id: "work" });
  const original = f.post("acme/lobby", { kind: "message", actor: "bob", text: "**Source Markdown**",
    attachments: [{ removed: true }] } as any);
  const calls: { method: string; path: string; body: any }[] = [];
  const events: (ReturnType<typeof message> | { id: number; kind: string; actor: string; actor_id: string; text: string; reply_to?: number; created_at: string })[] = [];
  let started = false, floor = 0, read = 0, nextId = 20, gone = false;
  let forwardError: string | null = null;
  let directoryLimited = false;
  let reactions: { target_event_id: number; emoji: string; reactor_kind: string; reactor_id: string }[] = [];
  const dm = () => ({
    id: "dm-one", peer,
    last: events.filter(e => e.id > floor).at(-1),
    unread: events.filter(e => e.id > Math.max(floor, read) && e.actor_id !== "alice").length,
  });
  const network = { ...f.network };
  for (const method of ["GET", "POST", "PUT", "DELETE"]) {
    const key = `${method} ${BASE}/v1/*`;
    network[key] = req => {
      const u = new URL(req.url), path = u.pathname, body = JSON.parse(req.body ?? "{}");
      calls.push({ method, path: path + u.search, body });
      if (path === "/v1/directory") {
        if (directoryLimited) return { status: 429, body: {} };
        const q = u.searchParams.get("q")?.toLowerCase() ?? "";
        const cursor = u.searchParams.get("cursor");
        return { status: 200, body: q === "da" && !cursor ? { users: [peer, duplicate], next_cursor: "opaque/+ cursor" }
          : q === "da" ? { users: [other] }
          : { users: [peer, duplicate, other].filter(p => p.display_name.toLowerCase().startsWith(q) || p.id === q) },
        ...(q === "d" ? { delay: 1200 } : {}) };
      }
      if (path === "/v1/dms") {
        if (method === "GET") return { status: 200, body: { dms: started && !gone && (!floor || dm().last) ? [dm()] : [] } };
        if (body.user_id !== peer.id) return { status: 404, body: {} };
        const status = started ? 200 : 201;
        started = true;
        gone = false;
        return { status, body: dm() };
      }
      if (path.startsWith(dmPath)) {
        if (gone || !started) return { status: 404, body: {} };
        if (path === `${dmPath}/events`) {
          const page = events.filter(e => e.id > Math.max(floor, Number(u.searchParams.get("after")))).slice(0, 2);
          return { status: 200, body: { events: page, reactions: reactions.filter(r => page.some(e => e.id === r.target_event_id)) } };
        }
        if (path === `${dmPath}/messages`) {
          const event = message(nextId++, sent, body.text, body.reply_to);
          events.push(event);
          return { status: 201, body: event };
        }
        if (path === `${dmPath}/read`) read = Math.max(read, body.last_read);
        if (path === dmPath && method === "DELETE") floor = events.at(-1)?.id ?? 0;
        const doomed = path.match(/\/events\/(\d+)$/);
        if (doomed && method === "DELETE") {
          const ev = events.find(e => e.id === Number(doomed[1]) && e.kind === "message");
          if (!ev) return { status: 404, body: {} };
          if (ev.actor_id !== "alice") return { status: 403, body: { error: "you can delete only your own messages here" } };
          events.splice(events.indexOf(ev), 1);
          const marker = { id: nextId++, kind: "message_deleted", actor: "Alice", actor_id: "alice", created_at: date,
            text: JSON.stringify({ target_event_id: ev.id, tombstone: false }) };
          events.push(marker);
          return { status: 200, body: { deleted: true, events: [marker] } };
        }
        const target = path.match(/\/events\/(\d+)\/reactions$/);
        if (target) {
          reactions = reactions.filter(r => r.target_event_id !== Number(target[1]) || r.emoji !== body.emoji);
          if (method === "PUT") reactions.push({ target_event_id: Number(target[1]), emoji: body.emoji, reactor_kind: "user", reactor_id: "alice" });
          return { status: 200, body: { changed: true } };
        }
        return { status: 204 };
      }
      if (path === "/v1/forward") {
        if (forwardError) return { status: 404, body: { error: "not found", code: forwardError } };
        const forwarded = { ...message(nextId++, sent, original.text), forwarded: { author_name: "Bob", author_kind: "person", sent_at: date } };
        if (body.destination.kind === "dm") {
          started = true;
          if (body.comment) events.push(message(nextId++, sent, body.comment));
          events.push({ ...forwarded, id: nextId++ });
        }
        return { status: 201, body: { events: [forwarded], omitted_attachments: 1 } };
      }
      return f.network[key]!(req);
    };
  }
  const app = await open("../../../App.tsx", { modules: launched(), network: network as any, platform: "linux",
    width, height: 840, clock: {} });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:workspace");
  return {
    app, calls, original, events, fail: (code: string | null) => { forwardError = code; },
    rateLimit: (enabled: boolean) => { directoryLimited = enabled; },
    incoming: (text: string) => { started = true; const event = message(nextId++, peer, text); events.push(event); return event; },
    denyDm: () => { gone = true; },
  };
}

test("directory debounces, paginates by cursor, suppresses stale matches, and Return opens the selected stable id", async () => {
  const { app, calls } = await fixture();
  await app.press("id:new-dm");
  await app.waitFor("id:directory-notice");
  await app.type("id:directory-search", "d");
  await app.waitUntil(() => calls.some(c => c.path === "/v1/directory?q=d&limit=20"));
  await app.type("id:directory-search", "a");
  await app.waitFor("id:directory-person:person-2");
  await app.press("label:More people");
  await app.waitFor("id:directory-person:person-3");
  expect(calls.some(c => c.path.includes("cursor=opaque%2F%2B%20cursor"))).toBe(true);
  await app.clear("id:directory-search");
  await app.type("id:directory-search", "person-1");
  await app.waitFor("id:directory-person:person-1");
  await app.wait(1250);
  expect(app.shows("id:directory-person:person-2")).toBe(false);
  await app.keyboard.press("enter");
  await app.waitFor("id:dm-composer");
  expect(calls.find(c => c.method === "POST" && c.path === "/v1/dms")?.body).toEqual({ user_id: peer.id });
  await app.waitFor("text:This is the start of your conversation with Dana.");
  expect(app.shows("id:ask-agent")).toBe(false);
  expect(app.shows("id:attach")).toBe(false);
  await app.press("id:dm-peer-card");
  await app.waitFor("id:dm-profile");
  expect(app.shows("text:person-1")).toBe(true);
  await app.keyboard.press("escape");
  await app.waitFor("id:dm-profile", { state: "gone" });
  await app.press("id:new-dm");
  await app.waitFor("id:directory-search");
  expect(app.shows("id:directory-notice")).toBe(false);
});

test("a leading slash is ordinary text in a DM", async () => {
  const { app, calls, incoming } = await fixture();
  incoming("Hello");
  await app.waitFor("id:dm:dm-one");
  await app.press("id:dm:dm-one");
  await app.type("id:dm-composer", "/compact");
  expect(app.shows("id:command-suggestions")).toBe(false);
  await app.submit("id:dm-composer");
  await app.waitUntil(() => calls.some(c => c.path === `${dmPath}/messages` && c.body.text === "/compact"));
  expect(calls.some(c => c.path.endsWith("/compact"))).toBe(false);
});

test("DM unread, keyboard reply, markdown send, reactions, hide, reappearance and access loss", async () => {
  const { app, calls, incoming, denyDm } = await fixture();
  const old = incoming("A private **message**");
  const newer = incoming("From the same person");
  incoming("Across a page boundary");
  await app.waitFor("label:Dana, 3 unread", { timeout: 9000 });
  await app.press("id:dm:dm-one");
  await app.waitFor("text:Across a page boundary");
  await app.waitUntil(() => calls.some(c => c.path === `${dmPath}/read`));
  await app.waitFor("label:Dana");
  await app.press(`id:reply:${old.id}`);
  await app.waitFor("id:dm-reply-context");
  await app.type("id:dm-composer", "/compact **plain text, not a command**");
  await app.keyboard.press("enter");
  await app.waitFor("text:/compact plain text, not a command");
  expect(calls.find(c => c.path === `${dmPath}/messages`)?.body)
    .toEqual({ text: "/compact **plain text, not a command**", reply_to: old.id });
  await app.press(`id:react:${newer.id}`);
  await app.press(`id:pick-reaction:${newer.id}:👍`);
  await app.waitFor(`id:reaction:${newer.id}:👍`);
  await app.press(`id:reaction:${newer.id}:👍`);
  await app.waitFor(`id:reaction:${newer.id}:👍`, { state: "gone" });
  await app.press("id:dm-menu:dm-one");
  await app.press("id:dm-hide:dm-one");
  await app.waitFor("text:Dana keeps their copy. It comes back if either of you sends a message.");
  await app.press("id:dm-confirm-hide:dm-one:confirm");
  await app.waitFor("id:dm:dm-one", { state: "gone" });
  incoming("A new visible message");
  await app.waitFor("id:dm:dm-one", { timeout: 9000 });
  await app.press("id:dm:dm-one");
  await app.waitFor("text:A new visible message");
  expect(app.shows(`id:event:${old.id}`)).toBe(false);
  denyDm();
  await app.waitFor("text:This conversation is no longer available.");
  expect(app.shows("text:A new visible message")).toBe(false);
});

test("Forward distinguishes top-level rooms, explicit threads and people and sends exact destinations", async () => {
  const { app, calls, original } = await fixture();
  await app.press("id:space:acme");
  const openForward = () => app.press(`id:forward:${original.id}`);
  await openForward();
  await app.waitFor("id:forward-omitted");
  await app.press("id:forward-space");
  await app.press("label:Side project >> nth=0");
  await app.waitUntil(() => calls.some(c => c.path === "/v1/spaces/side/rooms?top_level_only=true"));
  await app.press("id:forward-room");
  expect(app.shows("label:Design review")).toBe(false);
  await app.press("label:Workshop");
  await app.type("id:forward-comment", "A short note");
  await app.press("id:forward-submit");
  await app.waitFor("id:forward-dialog", { state: "gone" });
  expect(calls.find(c => c.path === "/v1/forward")?.body).toEqual({
    source: { kind: "space", space_id: "acme", room_id: "lobby", event_id: original.id },
    destination: { kind: "space", space_id: "side", room_id: "work" }, comment: "A short note",
  });
  await openForward();
  await app.press("id:forward-target:threads");
  await app.press("id:forward-space");
  await app.press("label:Side project >> nth=0");
  await app.press("id:forward-room");
  await app.press("label:Design review");
  await app.press("id:forward-submit");
  await app.waitFor("id:forward-dialog", { state: "gone" });
  expect(calls.filter(c => c.path === "/v1/forward")[1]?.body.destination).toEqual({ kind: "space", space_id: "side", room_id: "thread" });
  await openForward();
  await app.press("id:forward-target:people");
  await app.waitFor("id:directory-person:person-1");
  await app.press("id:directory-person:person-1");
  await app.press("id:forward-submit");
  await app.waitFor("id:forward-dialog", { state: "gone" });
  expect(calls.filter(c => c.path === "/v1/forward")[2]?.body.destination).toEqual({ kind: "dm", user_id: peer.id });
  await app.waitFor("id:dm:dm-one");
  await app.press("id:dm:dm-one");
  await app.waitFor("text:Forwarded from Bob");
});

test("narrow Forward handles directory throttling and distinct denials, and Escape restores the invoking button", async () => {
  const { app, original, rateLimit, fail } = await fixture(700);
  await app.press("id:space:acme");
  await app.press(`id:react:${original.id}`);
  await app.keyboard.press("escape");
  await app.keyboard.press("right");
  await app.keyboard.press("right");
  await app.keyboard.press("enter");
  await app.waitFor("id:forward-dialog");
  rateLimit(true);
  await app.press("id:forward-target:people");
  await app.waitFor("id:directory-error");
  rateLimit(false);
  await app.press("label:Retry search");
  await app.waitFor("id:directory-person:person-1");
  fail("forward_destination_not_found");
  await app.press("id:directory-person:person-1");
  await app.type("id:forward-comment", "Keep this comment");
  await app.press("id:forward-submit");
  await app.waitFor("text:You can't post there anymore.");
  expect(app.value("id:forward-comment")).toBe("Keep this comment");
  fail("forward_source_not_found");
  await app.press("id:directory-person:person-1");
  await app.press("id:forward-submit");
  await app.waitFor("text:You can no longer read this message.");
  expect(app.shows("id:forward-omitted")).toBe(false);
  await app.keyboard.press("escape");
  await app.waitFor("id:forward-dialog", { state: "gone" });
  await app.wait(50);
  await app.keyboard.press("enter");
  await app.waitFor("id:forward-dialog");
});

test("collapsed navigation exposes unread messages; a DM can forward to a Space without losing its draft", async () => {
  const { app, calls, incoming } = await fixture();
  const direct = incoming("A message from the DM");
  await app.waitFor("label:Dana, 1 unread", { timeout: 9000 });
  await app.press("id:toggle-navigation");
  await app.waitFor("label:Messages, 1 unread");
  await app.press("id:rail-messages");
  await app.waitFor("id:dm-inbox-list");
  await app.press("id:inbox-dm:dm-one");
  await app.waitFor("text:A message from the DM");
  await app.type("id:dm-composer", "Keep this draft");
  await app.keyboard.press("mod+,");
  await app.waitFor("id:settings-dialog");
  await app.keyboard.press("escape");
  await app.waitFor("id:settings-dialog", { state: "gone" });
  expect(app.value("id:dm-composer")).toBe("Keep this draft");
  await app.press(`id:forward:${direct.id}`);
  await app.press("id:forward-space");
  await app.press("label:Side project");
  await app.press("id:forward-room");
  await app.press("label:Workshop");
  await app.press("id:forward-submit");
  await app.waitFor("id:forward-dialog", { state: "gone" });
  expect(calls.find(c => c.path === "/v1/forward")?.body).toEqual({
    source: { kind: "dm", dm_id: "dm-one", event_id: direct.id },
    destination: { kind: "space", space_id: "side", room_id: "work" },
  });
  expect(app.value("id:dm-composer")).toBe("Keep this draft");
});

test("only your own DM messages offer Delete; a confirmed delete removes it for both", async () => {
  const { app, calls, events, incoming } = await fixture();
  const theirs = incoming("From Dana");
  await app.waitFor("id:dm:dm-one", { timeout: 9000 });
  await app.press("id:dm:dm-one");
  await app.waitFor("text:From Dana");
  await app.type("id:dm-composer", "mine to delete");
  await app.keyboard.press("enter");
  await app.waitFor("text:mine to delete");
  const mine = events.find(e => e.text === "mine to delete")!;
  await app.waitFor(`id:event:${mine.id}`);
  expect(app.shows(`id:delete:${theirs.id}`)).toBe(false);
  await app.press(`id:delete:${mine.id}`);
  await app.waitFor(`id:confirm-delete:${mine.id}`);
  expect(app.shows("text:/removed for both of you/")).toBe(true);
  await app.press(`id:delete-dialog:${mine.id}:confirm`);
  await app.waitFor(`id:event:${mine.id}`, { state: "gone" });
  expect(calls.filter(c => c.method === "DELETE").map(c => c.path)).toEqual([`${dmPath}/events/${mine.id}`]);
  expect(app.shows("text:From Dana")).toBe(true);
  expect(app.shows("text:/target_event_id/")).toBe(false);
});
