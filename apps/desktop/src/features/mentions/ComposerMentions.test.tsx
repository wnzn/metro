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

async function inAcme(network: ReturnType<typeof world>["network"], width = 1200, manualClock = false, height = 780) {
  const app = await open("../../../App.tsx", {
    modules: launched(), network: network as any, platform: "linux", width, height,
    ...(manualClock ? { clock: {} } : {}),
  });
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
    expect(JSON.parse(requests(app)[i].body!)).toEqual({ text, infer_text_mentions: false, defer_decisions: true, invoke_agent: false, ...recipient });
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
  await app.keyboard.press("tab");
  expect(app.shows("id:mention-chips")).toBe(false);
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
    text, infer_text_mentions: false, defer_decisions: true, invoke_agent: false,
    mentions: [{ kind: "user", id: "carol" }], spans: [span(text, "@Álex Rivera", 0)],
  });
});

test("narrow chooser follows the highlighted stable ID across a reorder, and never accepts a removed highlight", async () => {
  const f = world();
  const label = "Alex Rivera with a long display name";
  const original = ["bob", "carol", "member-c", "member-d", "member-e", "member-f", "member-g", "member-h"];
  let listed = original;
  const base = f.network[get]!;
  f.network[get] = req => {
    const path = new URL(req.url).pathname;
    const id = path.split("/").at(-1)!;
    if (path === membersPath) {
      return { status: 200, body: { members: [
        { user_id: "alice", role: "owner" }, ...listed.map(user_id => ({ user_id, role: "member" })),
      ] } };
    }
    if (path.startsWith("/v1/profiles/") && original.includes(id)) {
      return { status: 200, body: { id, display_name: label } };
    }
    return base(req);
  };
  const app = await inAcme(f.network, 720, true, 480);
  await waitDirectory(app);
  const draft = `@${label}, please check.`;
  await app.type("id:composer", draft);
  await app.waitFor("id:mention-ambiguous");
  await app.keyboard.press("tab");
  await app.keyboard.press("enter");
  expect(app.shows("id:mention-chips")).toBe(false);
  expect(requests(app)).toHaveLength(0);
  // Up with no prior highlight deliberately chooses the last row and reveals
  // it inside the bounded scroller (too many two-line rows to fit at once).
  await app.keyboard.press("up");
  await app.waitFor("id:pick:member-h:detail");
  expect(app.shows("id:pick:member-h >> text:@member-h")).toBe(true);
  await app.hover("id:pick:member-h:detail");

  listed = [...original].reverse();
  await app.clock!.runFor(10_100);
  await app.keyboard.press("tab");
  await app.waitFor("id:mention-chip:member-h");
  expect(app.shows("id:mention-chip:bob")).toBe(false);
  expect(requests(app)).toHaveLength(0);

  await app.clear("id:composer");
  await app.type("id:composer", draft);
  await app.waitFor("id:mention-ambiguous");
  await app.keyboard.press("down"); // member-h is now the first row.
  listed = listed.filter(id => id !== "member-h");
  await app.clock!.runFor(10_100);
  await app.waitFor("id:pick:member-h", { state: "gone" });
  await app.keyboard.press("tab");
  await app.keyboard.press("enter");
  expect(app.shows("id:mention-chips")).toBe(false);
  expect(app.value("id:composer")).toBe(draft);
  expect(requests(app)).toHaveLength(0);
  await app.keyboard.press("down");
  await app.keyboard.press("enter");
  await app.waitFor("id:mention-chip:member-g");
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
    text: after, invoke_agent: false, infer_text_mentions: false, defer_decisions: true,
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
  expect(JSON.parse(requests(app)[0].body!)).toEqual({ text, infer_text_mentions: false, defer_decisions: true, invoke_agent: false });
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
  expect(JSON.parse(requests(app)[0].body!)).toEqual({ text, infer_text_mentions: false, defer_decisions: true, invoke_agent: false });
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
    text, invoke_agent: false, infer_text_mentions: false, defer_decisions: true,
    mentions: [{ kind: "user", id: "bob" }], spans: [span(text, "@River Tan", 0)],
  });
});

test("a delayed upload rechecks recipient membership and retains draft and uploaded attachment on refusal", async () => {
  const w = attachmentWorld();
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
    text: "", infer_text_mentions: false, defer_decisions: true, invoke_agent: false, asset_ids: [w.l.assets[0]!.id],
  });
});

test("post-upload preflight detects a moved same-visit alias and keeps the draft until an explicit current pick", async () => {
  const w = attachmentWorld();
  const base = w.network[get]!;
  const original = { id: "ag-a", space_id: "acme", name: "Original", handle: "coder", is_default: true };
  const other = { id: "ag-b", space_id: "acme", name: "New Coder", handle: "review", is_default: false };
  let moved = false, renamedAfterPick = false, held = 0;
  w.network[get] = req => {
    if (new URL(req.url).pathname !== "/v1/spaces/acme/agents") return base(req);
    if (moved) held++;
    return { status: 200, delay: moved ? 4_000 : 0, body: { tools: [], agents: renamedAfterPick
      ? [original, { ...other, name: "Renamed after pick" }]
      : moved ? [{ ...original, handle: "retired" }, { ...other, handle: "coder" }] : [original, other] } };
  };
  const app = await inAcme(w.network, 720, true);
  await waitDirectory(app);
  const text = "🌿 @coder please check";
  await app.type("id:composer", text);
  await app.waitFor("id:mention-chip:agent:ag-a");
  app.onPick(() => [{ name: "note.txt", type: "text/plain", data: new TextEncoder().encode("note") }]);
  await app.press("id:attach");
  await app.waitFor("id:staged:note.txt");
  w.knobs.uploadDelay = 2_000;
  await app.press("id:send");
  await app.waitUntil(() => w.l.assets.length === 1);
  moved = true;
  await app.clock!.runFor(2_100);
  await app.waitUntil(() => held === 1, { timeout: 1_000 });
  expect(requests(app)).toHaveLength(0);
  expect(app.shows("id:mention-chip:agent:ag-a")).toBe(true);
  await app.clock!.runFor(4_100);
  await app.waitFor("text:/Pick who you mean\\. Nothing was sent/");
  expect(app.value("id:composer")).toBe(text);
  expect(app.shows("id:mention-chip:agent:ag-a")).toBe(false);
  expect(app.shows("id:staged:note.txt >> text:/in the Library/")).toBe(true);
  expect(requests(app)).toHaveLength(0);
  if (!app.shows("id:mention-picker")) await app.press("id:mention");
  await app.waitFor("id:mention-ambiguous");
  await app.press("id:pick-agent:ag-b");
  await app.waitFor("id:mention-chip:agent:ag-b");
  // A deliberate pick is still valid when both aliases change again.
  renamedAfterPick = true;
  await app.press("id:send");
  await app.clock!.runFor(4_100);
  await app.waitUntil(() => requests(app).length === 1);
  expect(w.l.assets).toHaveLength(1);
  expect(JSON.parse(requests(app)[0].body!)).toMatchObject({
    text: "🌿 @New Coder please check", infer_text_mentions: false, defer_decisions: true, mentions: [{ kind: "agent", agent_id: "ag-b" }],
    asset_ids: [w.l.assets[0]!.id],
  });
});

test("same-visit name moves use fresh labels for confirmation rows and insertion, and expire on roster change", async () => {
  for (const expire of [false, true]) {
    const f = world();
    const members = f.state.spaces.get("acme")!.members;
    members.add("carol");
    const base = f.network[get]!;
    let moved = false;
    f.network[get] = req => {
      const id = new URL(req.url).pathname.match(/^\/v1\/profiles\/(bob|carol)$/)?.[1];
      if (!id) return base(req);
      const display_name = id === "bob" ? moved ? "Robert Tan" : "River Tan" : moved ? "River Tan" : "Carol Tan";
      return { status: 200, body: { id, display_name } };
    };
    const app = await inAcme(f.network, 720, true);
    await waitDirectory(app);
    const text = "🌿 @River Tan please check";
    await app.type("id:composer", text);
    await app.waitFor("id:mention-chip:bob");
    moved = true;
    await app.press("id:send");
    await app.waitFor("text:/Pick who you mean\\. Nothing was sent/");
    expect(app.value("id:composer")).toBe(text);
    expect(requests(app)).toHaveLength(0);
    if (!app.shows("id:mention-picker")) await app.press("id:mention");
    await app.waitFor("label:Mention Robert Tan (@bob): notifies them only");
    await app.waitFor("label:Mention River Tan (@carol): notifies them only");
    expect(app.shows("label:Mention Carol Tan (@carol): notifies them only")).toBe(false);
    if (expire) {
      await app.keyboard.press("down");
      members.delete("carol");
      await app.clock!.runFor(10_100);
      await app.waitFor("id:mention-none");
      expect(app.shows("id:pick:bob")).toBe(false);
      expect(app.shows("id:pick:carol")).toBe(false);
      await app.keyboard.press("return");
      expect(app.value("id:composer")).toBe(text);
      expect(requests(app)).toHaveLength(0);
    } else {
      await app.press("id:pick:carol");
      await app.waitFor("id:mention-chip:carol");
      expect(app.value("id:composer")).toBe(text);
      await app.press("id:send");
      await app.waitUntil(() => requests(app).length === 1);
      expect(JSON.parse(requests(app)[0].body!)).toMatchObject({
        text, mentions: [{ kind: "user", id: "carol" }], spans: [span(text, "@River Tan", 0)],
      });
    }
  }
});

for (const refresh of ["pending", "identical", "moved"] as const) {
  test(`held send profiles merge ordered background name evidence: ${refresh}`, async () => {
    const f = world();
    f.state.spaces.get("acme")!.members.add("carol");
    const base = f.network[get]!;
    let racing = false, renamedAfterPick = false;
    const reads = { bob: 0, carol: 0 };
    f.network[get] = req => {
      const id = new URL(req.url).pathname.match(/^\/v1\/profiles\/(bob|carol)$/)?.[1] as "bob" | "carol" | undefined;
      if (!id) return base(req);
      const count = racing ? ++reads[id] : 0;
      const background = count > 1;
      const moved = background && refresh === "moved";
      const display_name = id === "bob" ? moved ? "Robert Tan" : "River Tan"
        : renamedAfterPick ? "Robert Tan" : moved ? "River Tan" : "Carol Tan";
      const delay = count === 1 && id === "bob" ? 8_000 : background && refresh === "pending" ? 8_000 : 0;
      return { status: 200, delay, body: { id, display_name } };
    };
    const app = await inAcme(f.network, 720, true);
    await waitDirectory(app);
    const text = "🌿 @River Tan please check";
    await app.type("id:composer", text);
    await app.waitFor("id:mention-chip:bob");
    await app.clock!.runFor(57_000);
    racing = true;
    await app.press("id:send");
    await app.waitUntil(() => reads.bob === 1 && reads.carol === 1, { timeout: 1_000 });
    await app.clock!.runFor(4_000);
    expect(reads.bob).toBe(2);
    expect(reads.carol).toBe(2);
    expect(requests(app)).toHaveLength(0);
    await app.clock!.runFor(4_100);
    if (refresh === "moved") {
      await app.waitFor("text:/Pick who you mean\\. Nothing was sent/");
      expect(app.value("id:composer")).toBe(text);
      expect(requests(app)).toHaveLength(0);
      if (!app.shows("id:mention-picker")) await app.press("id:mention");
      await app.waitFor("label:Mention Robert Tan (@bob): notifies them only");
      await app.waitFor("label:Mention River Tan (@carol): notifies them only");
      await app.press("id:pick:bob");
      const picked = text.replace("River Tan", "Robert Tan");
      expect(app.value("id:composer")).toBe(picked);
      racing = false;
      renamedAfterPick = true;
      await app.clock!.runFor(61_000);
      await app.waitFor("id:mention-chip:bob >> text:@River Tan");
      expect(app.value("id:composer")).toBe(picked);
      await app.press("id:send");
      await app.waitUntil(() => requests(app).length === 1);
      expect(JSON.parse(requests(app)[0].body!)).toMatchObject({
        text: picked, mentions: [{ kind: "user", id: "bob" }], spans: [span(picked, "@Robert Tan", 0)],
      });
    } else {
      await app.waitUntil(() => requests(app).length === 1);
      expect(JSON.parse(requests(app)[0].body!)).toMatchObject({ text, mentions: [{ kind: "user", id: "bob" }] });
    }
  });
}

test("a completed name swap expires an open confirmation without a membership change", async () => {
  const f = world();
  f.state.spaces.get("acme")!.members.add("carol");
  const base = f.network[get]!;
  let version = 0;
  const started = { bob: 0, carol: 0 };
  f.network[get] = req => {
    const id = new URL(req.url).pathname.match(/^\/v1\/profiles\/(bob|carol)$/)?.[1] as "bob" | "carol" | undefined;
    if (!id) return base(req);
    if (version === 2) started[id]++;
    const names = [{ bob: "River Tan", carol: "Carol Tan" },
      { bob: "Robert Tan", carol: "River Tan" }, { bob: "River Tan", carol: "Robert Tan" }];
    return { status: 200, delay: version === 2 ? 3_500 : 0, body: { id, display_name: names[version]![id] } };
  };
  const app = await inAcme(f.network, 720, true);
  await waitDirectory(app);
  const text = "🌿 @River Tan please check";
  await app.type("id:composer", text);
  await app.waitFor("id:mention-chip:bob");
  version = 1;
  await app.press("id:send");
  await app.waitFor("text:/Pick who you mean\\. Nothing was sent/");
  if (!app.shows("id:mention-picker")) await app.press("id:mention");
  await app.waitFor("label:Mention River Tan (@carol): notifies them only");
  await app.keyboard.press("down");
  version = 2;
  await app.clock!.runFor(61_000);
  expect(started.bob).toBe(1);
  expect(started.carol).toBe(1);
  expect(app.shows("label:Mention River Tan (@carol): notifies them only")).toBe(true);
  await app.clock!.runFor(3_600);
  await app.waitFor("id:mention-none");
  expect(app.shows("id:pick:carol")).toBe(false);
  expect(app.shows("id:pick:bob")).toBe(false);
  await app.keyboard.press("return");
  expect(app.value("id:composer")).toBe(text);
  expect(requests(app)).toHaveLength(0);
});

test("a fresh Space visit cannot seed aliases from a cached actor profile", async () => {
  const f = world();
  const base = f.network[get]!;
  let renamed = false, held = 0;
  f.network[get] = req => {
    if (new URL(req.url).pathname !== "/v1/profiles/bob") return base(req);
    if (renamed) held++;
    return { status: 200, delay: renamed ? 4_000 : 0,
      body: { id: "bob", display_name: renamed ? "Robert Tan" : "River Tan" } };
  };
  const app = await inAcme(f.network, 720, true);
  await waitDirectory(app);
  await app.press("id:nav-chat");
  await app.waitFor("id:chat-view");
  renamed = true;
  await app.press("id:space:acme");
  await app.waitUntil(() => held > 0, { timeout: 1_000 });
  await app.press("id:mention");
  await app.waitFor("id:mention-none");
  expect(app.shows("id:pick:bob")).toBe(false);
  await app.clock!.runFor(4_100);
  await app.waitFor("label:Mention Robert Tan (@bob): notifies them only");
  expect(app.shows("label:Mention River Tan (@bob): notifies them only")).toBe(false);
});

for (const change of ["pending", "identical", "revoked", "agent-alias"] as const) {
  test(`roster poll overlapping held send profiles: ${change}`, async () => {
    const w = spaceWorld();
    const f = w.f;
    const base = w.network[get]!;
    const agentsPath = "/v1/spaces/acme/agents";
    let preflighting = false, held = false, membersRead = 0, agentsRead = 0;
    w.network[get] = req => {
      const path = new URL(req.url).pathname;
      if (preflighting && path === "/v1/profiles/bob" && !held) {
        held = true;
        return { status: 200, delay: 4_000, body: { id: "bob", display_name: "River Tan" } };
      }
      if (path === "/v1/profiles/bob") return { status: 200, body: { id: "bob", display_name: "River Tan" } };
      const response = base(req);
      if (preflighting && path === membersPath) {
        membersRead++;
        if (membersRead === 2 && change === "pending") return { ...response as object, delay: 5_000 };
      }
      if (preflighting && path === agentsPath) {
        agentsRead++;
        if (agentsRead === 1 && change === "pending") return { ...response as object, delay: 5_000 };
      }
      return response;
    };
    const app = await inAcme(w.network, 720, true);
    await waitDirectory(app);
    const text = "🌿 @River Tan please check";
    await app.type("id:composer", text);
    await app.waitFor("id:mention-chip:bob");
    await app.clock!.runFor(8_000);
    preflighting = true;
    await app.press("id:send");
    await app.waitUntil(() => held, { timeout: 1_000 });
    expect(membersRead).toBe(1);
    if (change === "revoked") f.state.spaces.get("acme")!.members.delete("bob");
    if (change === "agent-alias") w.agent("acme", { name: "River Tan", handle: "river" });
    await app.clock!.runFor(2_200);
    expect(membersRead).toBe(2);
    expect(agentsRead).toBe(1);
    expect(requests(app)).toHaveLength(0);
    await app.clock!.runFor(2_000);
    if (change === "revoked" || change === "agent-alias") {
      await app.waitFor(change === "revoked" ? "text:/Could not verify the current recipient list/"
        : "text:/Pick who you mean\\. Nothing was sent/");
      expect(app.value("id:composer")).toBe(text);
      expect(requests(app)).toHaveLength(0);
    } else {
      await app.waitUntil(() => requests(app).length === 1, { timeout: 1_000 });
      expect(JSON.parse(requests(app)[0].body!)).toMatchObject({
        text, mentions: [{ kind: "user", id: "bob" }],
      });
    }
  });
}

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

test("a real-shaped Markdown message displays current names, leaves deleted spans unavailable, and copies Markdown", async () => {
  const w = spaceWorld();
  const coder = w.agent("acme", { handle: "coder", name: "Refactor Coach", description: "Reviews builds." });
  const base = w.network[get]!;
  w.network[get] = req => new URL(req.url).pathname === "/v1/profiles/alice"
    ? { status: 200, body: { id: "alice", display_name: "Alice Renamed", bio: "Rollout owner." } }
    : base(req);
  const callback = w.network[post]!;
  const saved = "🤖 **@alice**, *@coder*, @old and @agent.";
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
  // The headless text engine positions inline attachments at one character's
  // advance, regardless of their width. Exercise their keyboard path here.
  await app.keyboard.press("shift+tab");
  await app.keyboard.press("shift+tab");
  await app.keyboard.press("shift+tab");
  await app.keyboard.press("enter");
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
  await app.keyboard.press("tab");
  await app.keyboard.press("enter");
  await app.waitFor("id:identity-card-agent-settings");
  expect(app.shows("id:identity-card >> text:Reviews builds.")).toBe(true);
  await app.keyboard.press("escape");
  await app.waitFor("id:identity-card", { state: "gone" });
  await app.press(`id:copy:${id}`);
  expect(app.clipboard()).toBe("🤖 **@Alice Renamed**, *@Refactor Coach*, @old and @agent.");
});

test("authoritative empty and malformed span fields disable fallback and copy rewriting; only stored legacy spellings qualify", async () => {
  const w = spaceWorld();
  const coder = w.agent("acme", { handle: "coder", name: "Current Agent Name" });
  const modern = w.f.post("acme/lobby", {
    kind: "message", actor: "Alice", text: "Literal @agent and @bob.", audience: "mixed",
    mentions: [{ kind: "agent" }, { kind: "user", id: "bob" }], spans: [],
  } as any);
  const malformed = w.f.post("acme/lobby", {
    kind: "message", actor: "Alice", text: "Malformed @bob.", mentions: [{ kind: "user", id: "bob" }], spans: null,
  } as any);
  const legacy = w.f.post("acme/lobby", {
    kind: "message", actor: "Alice", text: "Legacy @coder.", mentions: [{ kind: "agent", agent_id: coder.id }],
  });
  const historical = w.f.post("acme/lobby", {
    kind: "message", actor: "Alice", text: "Historical @old-coder, not @coder.",
    mentions: [{ kind: "agent", agent_id: coder.id, handle: "old-coder" }],
  });
  const app = await inAcme(w.network);
  await waitDirectory(app);
  await app.waitFor(`id:mention-token:${historical.id}:0`);
  expect(app.shows("label:Current Agent Name, mentioned. Open card.")).toBe(true);
  expect(app.shows(`id:mention-token:${historical.id}:1`)).toBe(false);
  for (const row of [modern, malformed, legacy]) {
    expect(app.shows(`id:mention-token:${row.id}:0`)).toBe(false);
    await app.press(`id:copy:${row.id}`);
    expect(app.clipboard()).toBe(row.text);
  }
  await app.press(`id:copy:${historical.id}`);
  expect(app.clipboard()).toBe("Historical @Current Agent Name, not @coder.");
});

test("a delayed re-entry never exposes a removed person's cached card but current cards work once reverified", async () => {
  const f = world();
  f.state.spaces.get("acme")!.members.add("carol");
  const base = f.network[get]!;
  f.network[get] = req => {
    const id = new URL(req.url).pathname.split("/").at(-1);
    return new URL(req.url).pathname.startsWith("/v1/profiles/") && (id === "bob" || id === "carol")
      ? { status: 200, body: { id, display_name: id === "bob" ? "Robert" : "Carol", bio: `${id}'s information` } }
      : base(req);
  };
  const text = "Old names @Robert and @Carol.";
  const e = f.post("acme/lobby", {
    kind: "message", actor: "Alice", text,
    mentions: [{ kind: "user", id: "bob" }, { kind: "user", id: "carol" }],
    spans: [span(text, "@Robert", 0), span(text, "@Carol", 1)],
  } as any);
  const app = await inAcme(f.network, 720, true);
  await app.waitFor("label:Robert, mentioned. Open card.");
  await app.press(`id:mention-token:${e.id}:0`);
  await app.waitFor("id:identity-card >> text:@bob");
  await app.keyboard.press("escape");
  await app.press("id:nav-chat");
  await app.waitFor("id:chat-view");
  f.state.spaces.get("acme")!.members.delete("bob");
  const late = f.answerLate(membersPath, TOKENS.alice!, 4_000);
  await app.press("id:space:acme");
  await app.waitUntil(() => late.arrived() > 0, { timeout: 1_000 });
  expect(app.shows(`id:mention-token:${e.id}:0`)).toBe(true);
  expect(app.shows("label:Robert, mentioned. Open card.")).toBe(false);
  expect(app.shows("label:Carol, mentioned. Open card.")).toBe(false);
  expect(app.shows("id:identity-card-copy-id")).toBe(false);
  expect(app.shows("label:/Addressed to .*@bob/")).toBe(false);
  await app.clock!.runFor(4_100);
  await app.waitFor("label:Carol, mentioned. Open card.");
  expect(app.shows("label:Robert, mentioned. Open card.")).toBe(false);
  await app.press(`id:mention-token:${e.id}:1`);
  await app.waitFor("id:identity-card >> text:@carol");
  await app.press("id:identity-card-copy-id");
  expect(app.clipboard()).toBe("carol");
});

test("fast members do not verify a cached agent alias or card while the exact re-entry roster is held", async () => {
  const w = spaceWorld();
  const a = w.agent("acme", { handle: "coder", name: "Original Coder" });
  const b = w.agent("acme", { handle: "review", name: "Current Coder", description: "The new owner of the handle." });
  const gone = w.agent("acme", { handle: "gone", name: "Deleted Agent" });
  const text = "Earlier we asked @coder, @gone and @review with @bob.";
  const e = w.f.post("acme/lobby", {
    kind: "message", actor: "Alice", text,
    mentions: [
      { kind: "agent", agent_id: a.id }, { kind: "agent", agent_id: gone.id },
      { kind: "agent", agent_id: b.id }, { kind: "user", id: "bob" },
    ],
    spans: ["@coder", "@gone", "@review", "@bob"].map((name, i) => span(text, name, i)),
  });
  let delay = 0, oldReads = 0, newReads = 0;
  const base = w.network[get]!;
  w.network[get] = req => {
    const answer = base(req);
    if (new URL(req.url).pathname !== "/v1/spaces/acme/agents" || !delay) return answer;
    if (delay === 6500) oldReads++;
    else newReads++;
    return { ...answer as object, delay };
  };
  const app = await inAcme(w.network, 720, true);
  await app.waitFor("label:Deleted Agent, mentioned. Open card.");
  // A refresh launched on the old visit still holds a snapshot containing A
  // and the removed agent. It will arrive after the new visit's response.
  delay = 6500;
  await app.clock!.runFor(10_100);
  expect(oldReads).toBe(1);
  await app.press("id:nav-chat");
  await app.waitFor("id:chat-view");
  a.handle = "former";
  b.handle = "coder";
  w.s.agents.set("acme", w.s.agents.get("acme")!.filter(agent => agent.id !== gone.id));
  delay = 4000;
  await app.press("id:space:acme");
  await app.waitUntil(() => newReads === 1, { timeout: 1000 });
  // A usable member card proves that members finished before the agents.
  await app.waitFor("label:bob, mentioned. Open card.", { timeout: 1000 });
  expect(app.shows("label:Original Coder, mentioned. Open card.")).toBe(false);
  expect(app.shows("label:Current Coder, mentioned. Open card.")).toBe(false);
  expect(app.shows("label:Deleted Agent, mentioned. Open card.")).toBe(false);
  expect(app.shows("id:identity-card")).toBe(false);
  expect(app.shows("label:/Addressed to Original Coder/")).toBe(false);
  const unresolved = "@coder stays literal";
  await app.type("id:composer", unresolved);
  await app.waitFor("id:mention-unresolved", { timeout: 1000 });
  expect(app.shows(`id:mention-chip:agent:${a.id}`)).toBe(false);

  await app.clock!.runFor(4100);
  await app.waitFor("label:Current Coder, mentioned. Open card.");
  delay = 0;
  await app.clock!.runFor(2600); // old response cannot undo the successful new visit
  expect(app.shows("label:Deleted Agent, mentioned. Open card.")).toBe(false);
  await app.press(`id:mention-token:${e.id}:2`);
  await app.waitFor("id:identity-card >> text:The new owner of the handle.");
  expect(app.shows("id:identity-card >> text:@coder")).toBe(true);
  await app.keyboard.press("escape");
  await app.press("id:send");
  await app.waitUntil(() => requests(app).length === 1);
  expect(JSON.parse(requests(app)[0].body!)).toEqual({
    text: unresolved, infer_text_mentions: false, defer_decisions: true, invoke_agent: false,
  });
  await app.type("id:composer", "@coder please review");
  await app.waitFor(`id:mention-chip:agent:${b.id}`);
  expect(app.shows(`id:mention-chip:agent:${a.id}`)).toBe(false);
  await app.press("id:send");
  await app.waitUntil(() => requests(app).length === 2);
  expect(JSON.parse(requests(app)[1].body!)).toMatchObject({ mentions: [{ kind: "agent", agent_id: b.id }] });
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
    expect(JSON.parse(requests(app)[0].body!)).toMatchObject({ text, infer_text_mentions: false, defer_decisions: true, mentions: [{ kind: "user", id: "bob" }] });
  }
});
