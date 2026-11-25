import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../../test/fakeServer";
import { launched } from "../../../test/launched";
import { attachmentWorld } from "../attachments/test/fixtures";
import { spaceWorld } from "../space/test/fixtures";
import type { WireMentionSpan } from "./draft";

const get = `GET ${BASE}/v1/*`;
const post = `POST ${BASE}/v1/*`;
const lobby = "/v1/spaces/acme/rooms/lobby/messages";
const membersPath = "/v1/spaces/acme/members";
const utf8 = (text: string) => new TextEncoder().encode(text).length;
const span = (text: string, label: string, index: number): WireMentionSpan => {
  const at = text.indexOf(label);
  return { start: utf8(text.slice(0, at)), end: utf8(text.slice(0, at + label.length)), index };
};
const requests = (app: App) => app.requests().filter(r => r.method === "POST" && new URL(r.url).pathname.endsWith("/messages"));

async function inAcme(network: ReturnType<typeof world>["network"], width = 1200) {
  const app = await open("../../../App.tsx", { modules: launched(), network: network as any, platform: "linux", width, height: 780 });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:space:acme");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
  return app;
}

async function waitDirectory(app: App) {
  await app.press("id:mention");
  await app.waitFor("id:pick:bob");
  await app.keyboard.press("escape");
  await app.waitFor("id:mention-picker", { state: "gone" });
}

test("authoritative thread/room sends preserve mixed quoted-email and code text without asking @agent", async () => {
  const f = world();
  f.addRoom("acme", { id: "t1", name: "Follow-up", parent_room_id: "lobby" } as any);
  const base = f.network[get]!;
  f.network[get] = req => new URL(req.url).pathname === "/v1/profiles/bob"
    ? { status: 200, body: { id: "bob", display_name: "Bob Tan" } } : base(req);
  const app = await inAcme(f.network);
  await waitDirectory(app);
  const inputs = [
    '  Say "`quote`" then <"local"@agent>  ',
    '  Say "`quote"` then <"local"@agent>  ',
    'See `say "` then <"local"@agent>',
    'Use "first"@agent," second"@agent',
    'Use **"local"@agent** then carry on.',
    '🌿 Say "hello?" "@Bob Tan", then done.',
  ];
  for (const [i, text] of inputs.entries()) {
    if (i === 1) {
      await app.press("id:room:t1");
      await app.waitFor("id:open-parent");
      await waitDirectory(app);
    }
    await app.type("id:composer", text);
    await app.press("id:send");
    await app.waitUntil(() => requests(app).length === i + 1);
    await app.waitUntil(() => app.value("id:composer") === "");
    expect(new URL(requests(app)[i].url).pathname).toBe(i === 0 ? lobby : lobby.replace("lobby", "t1"));
    const recipient = text.includes("@Bob Tan") ? {
      mentions: [{ kind: "user", id: "bob" }], spans: [span(text, "@Bob Tan", 0)],
    } : {};
    expect(JSON.parse(requests(app)[i].body!)).toEqual({ text, infer_text_mentions: false, invoke_agent: false, ...recipient });
  }
});

test("ambiguity offers only the same stable candidates, never sends on Return, then uses the picked full label", async () => {
  const w = spaceWorld();
  w.f.state.spaces.get("acme")!.members.add("carol");
  const base = w.network[get]!;
  w.network[get] = req => {
    const path = new URL(req.url).pathname;
    if (path === "/v1/profiles/bob" || path === "/v1/profiles/carol") {
      return { status: 200, body: { id: path.split("/").at(-1), display_name: "Álex Rivera", bio: "A teammate" } };
    }
    return base(req);
  };
  const app = await inAcme(w.network, 720);
  await waitDirectory(app);
  await app.type("id:composer", "  🌿 @Álex Rivera, please look.");
  await app.waitFor("id:mention-ambiguous");
  expect(app.shows("text:Which Álex Rivera?")).toBe(true);
  expect(app.shows("id:pick:bob")).toBe(true);
  expect(app.shows("id:pick:carol")).toBe(true);
  expect(app.shows("id:pick:agent")).toBe(false);
  await app.submit("id:composer");
  expect(requests(app)).toHaveLength(0);
  await app.keyboard.press("escape");
  await app.waitFor("id:mention-picker", { state: "gone" });
  await app.submit("id:composer");
  await app.waitFor("id:mention-ambiguous");
  await app.press("id:pick:carol");
  await app.waitFor("id:mention-chip:carol");
  const text = "  🌿 @Álex Rivera, please look.";
  expect(app.value("id:composer")).toBe(text);
  await app.press("id:send");
  await app.waitUntil(() => requests(app).length === 1);
  expect(JSON.parse(requests(app)[0].body!)).toEqual({
    text, infer_text_mentions: false, invoke_agent: false,
    mentions: [{ kind: "user", id: "carol" }], spans: [span(text, "@Álex Rivera", 0)],
  });
});

test("a suffix edit reopens the chooser for an automatically captured agent, and a deliberate pick survives a room switch", async () => {
  const f = world();
  f.addRoom("acme", { id: "t1", name: "Follow-up", parent_room_id: "lobby" } as any);
  const app = await inAcme(f.network);
  await waitDirectory(app);
  const before = 'Say "hello " then @agent ';
  const after = `${before}please "@outside`;
  await app.type("id:composer", before);
  await app.waitFor("id:mention-chip:agent");
  await app.type("id:composer", 'please "@outside');
  await app.waitFor("id:mention-ambiguous");
  expect(app.shows("id:mention-chip:agent")).toBe(false);
  await app.submit("id:composer");
  expect(requests(app)).toHaveLength(0);
  await app.press("id:pick:agent");
  await app.waitFor("id:mention-chip:agent");
  await app.press("id:room:t1");
  await app.waitFor("id:open-parent");
  await app.press("id:open-parent");
  await app.waitFor("id:mention-chip:agent");
  expect(app.value("id:composer")).toBe(after);
  await app.press("id:send");
  await app.waitUntil(() => requests(app).length === 1);
  expect(JSON.parse(requests(app)[0].body!)).toEqual({
    text: after, invoke_agent: false, infer_text_mentions: false,
    mentions: [{ kind: "agent" }], spans: [span(after, "@agent", 0)],
  });
});

test("a completed token typed before the directory arrives stays unresolved after a delayed refresh", async () => {
  const f = world();
  f.answerLate(membersPath, TOKENS.alice!, 3000);
  const app = await inAcme(f.network);
  const text = "  @bob please check  ";
  await app.type("id:composer", text);
  await app.waitFor("id:mention-unresolved");
  await waitDirectory(app);
  expect(app.shows("id:mention-chip:bob")).toBe(false);
  await app.press("id:send");
  await app.waitUntil(() => requests(app).length === 1);
  expect(JSON.parse(requests(app)[0].body!)).toEqual({ text, infer_text_mentions: false, invoke_agent: false });
});

test("without a verified roster even a reserved typed token is plain, not a guessed recipient", async () => {
  const f = world();
  const base = f.network[get]!;
  f.network[get] = req => new URL(req.url).pathname === membersPath
    ? { status: 503, body: { error: "Directory is offline" } } : base(req);
  const app = await inAcme(f.network);
  await app.press("id:mention");
  await app.waitFor("text:/Directory is offline/");
  await app.keyboard.press("escape");
  const text = "@agent please check the draft";
  await app.type("id:composer", text);
  await app.waitFor("id:mention-unresolved");
  expect(app.shows("id:mention-chip:agent")).toBe(false);
  await app.press("id:send");
  await app.waitUntil(() => requests(app).length === 1);
  expect(JSON.parse(requests(app)[0].body!)).toEqual({ text, infer_text_mentions: false, invoke_agent: false });
});

test("span text cannot turn a people-only message into one for an agent whose handle is its prefix", async () => {
  const w = spaceWorld();
  w.agent("acme", { handle: "river", name: "River Bot" });
  const base = w.network[get]!;
  w.network[get] = req => new URL(req.url).pathname === "/v1/profiles/bob"
    ? { status: 200, body: { id: "bob", display_name: "River Tan", bio: "" } }
    : base(req);
  const app = await inAcme(w.network);
  await waitDirectory(app);
  // Using separate edits also exercises the complete-label boundary.
  await app.type("id:composer", "🌿 @River ");
  await app.type("id:composer", "Tan,");
  await app.type("id:composer", " please check.");
  await app.waitFor("id:mention-chip:bob");
  const text = "🌿 @River Tan, please check.";
  await app.press("id:send");
  await app.waitUntil(() => requests(app).length === 1);
  expect(JSON.parse(requests(app)[0].body!)).toEqual({
    text, invoke_agent: false, infer_text_mentions: false,
    mentions: [{ kind: "user", id: "bob" }], spans: [span(text, "@River Tan", 0)],
  });
});

test("a delayed upload rechecks recipient membership and retains draft and uploaded attachment on refusal", async () => {
  const w = attachmentWorld();
  const app = await inAcme(w.network);
  await waitDirectory(app);
  const text = "@bob please check";
  await app.type("id:composer", text);
  await app.waitFor("id:mention-chip:bob");
  app.onPick(() => [{ name: "note.txt", type: "text/plain", data: new TextEncoder().encode("note") }]);
  await app.press("id:attach");
  await app.waitFor("id:staged:note.txt");
  w.knobs.uploadDelay = 600;
  await app.press("id:send");
  await app.waitUntil(() => w.l.assets.length === 1);
  w.f.state.spaces.get("acme")!.members.delete("bob");
  await app.waitFor("text:/recipient is no longer in this Space/");
  expect(app.value("id:composer")).toBe(text);
  expect(requests(app)).toHaveLength(0);
  expect(app.shows("id:staged:note.txt >> text:/in the Library/")).toBe(true);
  await app.clear("id:composer");
  await app.press("id:send");
  await app.waitUntil(() => requests(app).length === 1);
  expect(JSON.parse(requests(app)[0].body!)).toEqual({
    text: "", infer_text_mentions: false, invoke_agent: false, asset_ids: [w.l.assets[0]!.id],
  });
});

test("switching to a thread during an upload never posts the old room's draft into either place", async () => {
  const w = attachmentWorld();
  w.f.addRoom("acme", { id: "t1", name: "Follow-up", parent_room_id: "lobby" } as any);
  const app = await inAcme(w.network);
  await waitDirectory(app);
  const text = "@bob please check";
  await app.type("id:composer", "@");
  await app.press("id:pick:bob");
  await app.type("id:composer", "please check");
  await app.waitFor("id:mention-chip:bob");
  app.onPick(() => [{ name: "note.txt", type: "text/plain", data: new TextEncoder().encode("note") }]);
  await app.press("id:attach");
  await app.waitFor("id:staged:note.txt");
  w.knobs.uploadDelay = 700;
  await app.press("id:send");
  await app.waitUntil(() => w.l.assets.length === 1);
  await app.press("id:room:t1");
  await app.waitFor("id:open-parent");
  expect(app.value("id:composer")).toBe("");
  await app.type("id:composer", "a separate thread draft");
  await app.waitUntil(() => app.enabled("id:send"));
  expect(requests(app)).toHaveLength(0);
  await app.press("id:open-parent");
  await app.waitFor("id:mention-chip:bob");
  expect(app.value("id:composer")).toBe(text);
  expect(app.shows("id:staged:note.txt >> text:/in the Library/")).toBe(true);
  await app.press("id:room:t1");
  await app.waitFor("id:open-parent");
  expect(app.value("id:composer")).toBe("a separate thread draft");
});

test("a real-shaped message response displays current names, leaves deleted spans unavailable, and copies current text", async () => {
  const w = spaceWorld();
  const coder = w.agent("acme", { handle: "coder", name: "Refactor Coach", description: "Reviews builds." });
  const base = w.network[get]!;
  w.network[get] = req => new URL(req.url).pathname === "/v1/profiles/alice"
    ? { status: 200, body: { id: "alice", display_name: "Alice Renamed", bio: "Rollout owner." } }
    : base(req);
  const callback = w.network[post]!;
  const saved = '🤖 @alice, @coder, @old and @agent.';
  let stored: { id: number } | undefined;
  w.network[post] = req => {
    if (new URL(req.url).pathname !== lobby) return callback(req);
    const message = w.f.post("acme/lobby", {
      kind: "message", actor: "Alice", actor_id: "alice", created_at: "2026-11-25T13:00:00Z",
      text: saved, audience: "mixed",
      mentions: [
        { kind: "user", id: "alice" }, { kind: "agent", agent_id: coder.id },
        { kind: "agent", agent_id: "ag-deleted" }, { kind: "agent" },
      ],
      spans: [span(saved, "@alice", 0), span(saved, "@coder", 1), span(saved, "@old", 2), span(saved, "@agent", 3)],
    } as any);
    stored = message;
    return { status: 202, body: { ...message, addressing_note: null, routing_note: null } };
  };
  const app = await inAcme(w.network, 720);
  await waitDirectory(app);
  await app.type("id:composer", "send the fixture");
  await app.press("id:send");
  await app.waitUntil(() => !!stored);
  const id = stored!.id;
  await app.waitFor(`id:mention-token:${id}:0`);
  expect(app.shows("id:mention-token-me")).toBe(true);
  expect(app.shows("label:Alice Renamed, mentioned. Open card.")).toBe(true);
  expect(app.shows("label:Refactor Coach, mentioned. Open card.")).toBe(true);
  expect(app.shows("label:@old, unavailable")).toBe(true);
  expect(app.shows(`id:mention-token:${id}:3`)).toBe(true);
  await app.press(`id:mention-token:${id}:0`);
  await app.waitFor("id:identity-card-copy-id");
  expect(app.shows("id:identity-card >> text:Rollout owner.")).toBe(true);
  await app.keyboard.press("enter");
  expect(app.clipboard()).toBe("alice");
  await app.keyboard.press("escape");
  await app.waitFor("id:identity-card", { state: "gone" });
  await app.keyboard.press("enter");
  await app.waitFor("id:identity-card");
  await app.keyboard.press("escape");
  await app.waitFor("id:identity-card", { state: "gone" });
  await app.press(`id:mention-token:${id}:1`);
  await app.waitFor("id:identity-card-agent-settings");
  expect(app.shows("id:identity-card >> text:Reviews builds.")).toBe(true);
  await app.keyboard.press("escape");
  await app.waitFor("id:identity-card", { state: "gone" });
  await app.press(`id:copy:${id}`);
  expect(app.clipboard()).toBe("🤖 @Alice Renamed, @Refactor Coach, @old and @agent.");
});

test("unsupported or uncertain Space POST is never replayed and keeps the exact draft", async () => {
  for (const code of [422, 503]) {
    const f = world();
    const base = f.network[post]!;
    f.network[post] = req => new URL(req.url).pathname === lobby
      ? { status: code, body: { error: code === 422 ? "unknown field `infer_text_mentions`" : "unavailable" } }
      : base(req);
    const app = await inAcme(f.network);
    await waitDirectory(app);
    const text = '  @bob, see <"local"@agent>  ';
    await app.type("id:composer", text);
    await app.press("id:send");
    await app.waitFor(code === 422 ? "text:/Update Metro Server to send Space messages/" : "text:/Check the conversation before sending again/");
    expect(app.value("id:composer")).toBe(text);
    expect(requests(app)).toHaveLength(1);
    expect(JSON.parse(requests(app)[0].body!)).toMatchObject({ text, infer_text_mentions: false, mentions: [{ kind: "user", id: "bob" }] });
  }
});
