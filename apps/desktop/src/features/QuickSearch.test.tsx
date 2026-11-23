// Quick results under the top search box, against the shared fake server
// and the library fixtures, plus a test double of POST /v1/search/quick
// (crates/server/src/quick_search.rs's wire shape). Answers can be delayed to
// test late ones. Fake tokens only; headless engine runs.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, header, TOKENS } from "../../test/fakeServer";
import { libraryWorld } from "./library/test/fixtures";

type Req = { method: string; url: string; headers: unknown; body?: string | null };
type Wire = { assets: unknown[]; messages: unknown[] };

/** A request body as one comparable string, whatever its key order. */
const canonical = (body: Record<string, unknown>) => JSON.stringify(Object.keys(body).sort().map((k) => [k, body[k]]));

function quickWorld() {
  const w = libraryWorld();
  const q = {
    calls: [] as { user: string; body: any }[],
    /** What the next lookups answer, by query (else nothing found). */
    answers: new Map<string, (user: string) => Wire>(),
    /** Answers delayed by query (ms of test time), to test late ones; `answered` records when each came back. */
    delay: new Map<string, number>(),
    answered: [] as { query: string; body: string; at: number }[],
    /** Answers to these queries wait for `release()`. */
    hold: new Set<string>(),
    held: [] as (() => void)[],
    status: 200,
  };
  const userOf = (req: Req) => Object.keys(TOKENS).find((u) => header(req as any, "authorization") === `Bearer ${TOKENS[u]}`) ?? "";
  const network: Record<string, (req: Req) => unknown> = { ...(w.network as any) };
  const inner = network[`POST ${BASE}/v1/*`]!;
  network[`POST ${BASE}/v1/*`] = (req: Req) => {
    if (new URL(req.url).pathname !== "/v1/search/quick") return inner(req);
    const body = JSON.parse(req.body ?? "{}");
    const user = userOf(req);
    q.calls.push({ user, body });
    if (q.status !== 200) return { status: q.status, body: { error: "no" } };
    const answer = () => {
      q.answered.push({ query: body.query, body: canonical(body), at: Date.now() });
      return { status: 200, body: q.answers.get(body.query)?.(user) ?? { assets: [], messages: [] } };
    };
    if (q.hold.has(body.query)) return new Promise((resolve) => q.held.push(() => resolve(answer())));
    const ms = q.delay.get(body.query);
    if (ms === undefined) return answer();
    return new Promise((resolve) => setTimeout(() => resolve(answer()), ms));
  };
  const wireAsset = (a: any) => {
    const { bytes: _b, ...rest } = a;
    return rest;
  };
  const release = () => {
    for (const r of q.held.splice(0)) r();
  };
  return { ...w, q, network, wireAsset, release };
}
type QW = ReturnType<typeof quickWorld>;

async function signIn(w: QW, user = "alice", app?: App): Promise<App> {
  const a = app ?? (await open("../../App.tsx", { network: w.network as any, platform: "linux", width: 1280, height: 860 }));
  await a.clear("id:server-url");
  await a.type("id:server-url", BASE);
  await a.type("id:access-token", TOKENS[user]!);
  await a.press("id:connect");
  await a.waitFor("id:top-search");
  return a;
}

const text = (app: App) => app.texts().join(" | ");

/**
 * Waits on a condition outside the app's steps. The fake network is served
 * while a step runs, so a held answer is raced by starting steps without
 * awaiting them (a long `app.wait` keeps the app running) and polling here.
 */
async function poll(cond: () => boolean, ms = 10_000) {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error("condition not reached: " + cond.toString());
    await new Promise((r) => setTimeout(r, 10));
  }
}
/**
 * Waits until Metro was asked exactly this (query, scope, mode) and its answer
 * came back, whatever partial queries typing asked on the way; the list then
 * shows that answer, the newest one.
 */
async function lookedUp(app: App, w: QW, body: Record<string, unknown>) {
  const want = canonical(body);
  await app.waitUntil(() => w.q.answered.some((a) => a.body === want), { timeout: 10_000 });
  expect(w.q.calls.at(-1)!.body).toEqual(body); // nothing asked after it
}

const semantic = (w: QW) => w.f.posts().filter((p) => p.path === "/v1/search" || p.path === "/v1/search/index");
const quickCalls = (w: QW) => w.q.calls.length;

/** A Chat page lookup that finds a personal photo and Bob's message in Acme. */
function seedRoadmap(w: QW) {
  const photo = w.addAsset("alice", null, "Roadmap Q3.jpg", "jpeg-bytes");
  const ev = w.f.post("acme/lobby", { kind: "message", actor: "bob", text: "the roadmap review moved to Thursday" });
  w.q.answers.set("roadmap", () => ({
    assets: [w.wireAsset(photo)],
    messages: [{ space_id: "acme", room_id: "lobby", event_id: ev.id, actor: "bob", text: "the roadmap review moved to Thursday" }],
  }));
  return { photo, ev };
}

test("typing shows loading, then results with the file's own name and extension and the message's text; More is last", async () => {
  const w = quickWorld();
  const { photo, ev } = seedRoadmap(w);
  const app = await signIn(w);
  w.q.delay.set("roadmap", 600);
  await app.type("id:top-search", "roadmap");
  await app.waitFor("id:quick-loading");
  expect(app.shows("id:quick-more")).toBe(true);
  await app.waitFor(`id:quick-asset:${photo.id}`);
  expect(app.shows(`id:quick-asset:${photo.id} >> text:Roadmap Q3.jpg`)).toBe(true);
  expect(app.shows(`id:quick-asset:${photo.id} >> text:.jpg · Your personal library`)).toBe(true);
  expect(app.shows(`id:quick-message:acme:lobby:${ev.id} >> text:the roadmap review moved to Thursday`)).toBe(true);
  expect(app.shows(`id:quick-message:acme:lobby:${ev.id} >> text:bob · Acme`)).toBe(true);
  // Global on the Chat page (typing may have asked for partial queries on the way); no semantic search, no indexing, no model.
  expect(w.q.calls.at(-1)!.body).toEqual({ query: "roadmap", scope: "all", mode: "all" });
  expect(w.q.calls.every((c) => c.body.scope === "all" && c.body.mode === "all")).toBe(true);
  expect(semantic(w)).toEqual([]);
  expect(app.shows("id:quick-loading")).toBe(false);
});

test("nothing found says so in place, with More results still last", async () => {
  const w = quickWorld();
  const app = await signIn(w);
  await app.type("id:top-search", "zebra");
  await app.waitFor("id:quick-empty");
  expect(text(app)).toContain("No quick matches. Files and messages in everything you can read.");
  expect(app.shows("id:quick-more")).toBe(true);
  expect(app.shows("id:search-view")).toBe(false); // not a separate page
});

test("a message row opens its conversation; a file row opens it in the Library viewer", async () => {
  const w = quickWorld();
  const { photo, ev } = seedRoadmap(w);
  const app = await signIn(w);
  await app.type("id:top-search", "roadmap");
  await app.waitFor(`id:quick-message:acme:lobby:${ev.id}`);
  await app.press(`id:quick-message:acme:lobby:${ev.id}`);
  await app.waitFor("text:the roadmap review moved to Thursday");
  await app.waitFor("id:quick-results", { state: "gone" });
  expect(app.shows("text:# lobby")).toBe(true);

  await app.clear("id:top-search");
  await app.type("id:top-search", "roadmap");
  await app.waitFor(`id:quick-asset:${photo.id}`);
  await app.press(`id:quick-asset:${photo.id}`);
  await app.waitFor("id:library-viewer");
  // The file is read through the authenticated content route, with its access check.
  expect(w.l.log.some((l) => l.path === `/v1/library/assets/${photo.id}/content` && l.user === "alice")).toBe(true);
  expect(semantic(w)).toEqual([]);
});

test("More results opens full Search with the query and runs it once; no further quick lookup", async () => {
  const w = quickWorld();
  seedRoadmap(w);
  const app = await signIn(w);
  await app.type("id:top-search", "roadmap");
  await app.waitFor("id:quick-message:acme:lobby:" + w.f.state.events.get("acme/lobby")!.at(-1)!.id);
  const before = quickCalls(w);
  await app.press("id:quick-more");
  await app.waitFor("id:search-view");
  await app.waitUntil(() => semantic(w).length === 1);
  expect(semantic(w)[0]).toMatchObject({ path: "/v1/search", body: { query: "roadmap", scope: "all" } });
  expect(quickCalls(w)).toBe(before);
  expect(app.shows("id:quick-results")).toBe(false);
  expect(app.value("id:top-search")).toBe("roadmap");
});

test("a global lookup that found only files opens More in Documents & media, with its own filters", async () => {
  const w = quickWorld();
  const doc = w.addAsset("alice", null, "budget notes.md", "# budget");
  w.q.answers.set("budget", () => ({ assets: [w.wireAsset(doc)], messages: [] }));
  const app = await signIn(w);
  await app.type("id:top-search", "budget");
  await app.waitFor(`id:quick-asset:${doc.id}`);
  expect(app.shows(`id:quick-asset:${doc.id} >> text:.md · Your personal library`)).toBe(true);
  await app.press("id:quick-more");
  await app.waitFor("id:search-view");
  await app.waitUntil(() => w.l.log.some((l) => l.path === "/v1/library/search"));
  expect(app.value("id:top-search")).toBe("budget");
  expect(semantic(w)).toEqual([]); // not message search
});

test("keyboard: Down and Up move through the rows and More; Return opens the highlighted one; Escape closes; Return with none runs the full search", async () => {
  const w = quickWorld();
  const { photo, ev } = seedRoadmap(w);
  const app = await signIn(w);
  await app.keyboard.press("mod+k");
  await app.keyboard.press("r");
  for (const k of "oadmap") await app.keyboard.press(k);
  expect(app.value("id:top-search")).toBe("roadmap");
  await app.waitFor(`id:quick-asset:${photo.id}`);

  await app.keyboard.press("escape");
  await app.waitFor("id:quick-results", { state: "gone" });
  expect(app.value("id:top-search")).toBe("roadmap");

  await app.keyboard.press("backspace");
  await app.keyboard.press("p");
  await app.waitFor(`id:quick-message:acme:lobby:${ev.id}`);
  await app.keyboard.press("up"); // wraps to More
  await app.keyboard.press("up");
  await app.keyboard.press("down");
  await app.keyboard.press("up"); // the message
  await app.keyboard.press("enter");
  await app.waitFor("text:# lobby");
  expect(semantic(w)).toEqual([]);

  // Nothing highlighted: Return is the full search, as before.
  await app.clear("id:top-search");
  await app.type("id:top-search", "roadmap");
  await app.waitFor(`id:quick-asset:${photo.id}`);
  await app.submit("id:top-search");
  await app.waitFor("id:search-view");
  await app.waitUntil(() => semantic(w).length === 1);
  expect(app.shows("id:quick-results")).toBe(false);
});

test("a late answer for an older query is never shown", async () => {
  const w = quickWorld();
  const { photo } = seedRoadmap(w);
  w.q.answers.set("road", () => ({ assets: [w.wireAsset(photo)], messages: [] }));
  const app = await signIn(w);
  w.q.hold.add("road");
  await app.type("id:top-search", "road");
  const running = app.wait(2500);
  await poll(() => w.q.held.length === 1); // asked; the answer is held
  const typing = app.type("id:top-search", "x"); // "roadx": the first answer is stale now
  await poll(() => app.value("id:top-search") === "roadx");
  w.release(); // the answer for "road" arrives after the edit
  await poll(() => w.q.answered.some((a) => a.query === "road"));
  expect(app.shows(`id:quick-asset:${photo.id}`)).toBe(false);
  await Promise.all([running, typing]);
  expect(app.shows(`id:quick-asset:${photo.id}`)).toBe(false);
  await lookedUp(app, w, { query: "roadx", scope: "all", mode: "all" });
  await app.waitFor("id:quick-empty");
  const at = (q: string) => w.q.answered.findIndex((a) => a.query === q);
  expect(at("road")).toBeLessThan(at("roadx")); // the stale answer came first and was dropped
  expect(app.shows(`id:quick-asset:${photo.id}`)).toBe(false);
});

test("an answer that arrives after signing out never reaches the next session", async () => {
  const w = quickWorld();
  const { photo } = seedRoadmap(w);
  const app = await signIn(w);
  w.q.hold.add("roadmap");
  await app.type("id:top-search", "roadmap");
  const running = app.wait(3000);
  await poll(() => w.q.held.length === 1);
  const menu = app.press("id:account-menu");
  await poll(() => app.shows("id:disconnect"));
  const out = app.press("id:disconnect");
  await poll(() => app.shows("id:connect"));
  w.release(); // Alice's answer arrives after she signed out
  await Promise.all([running, menu, out]);
  expect(app.shows("id:quick-results")).toBe(false);
  expect(app.shows("id:connect-screen")).toBe(true);
  expect(w.q.answered.some((a) => a.query === "roadmap")).toBe(true);
  await signIn(w, "bob", app);
  await app.wait(200);
  expect(app.shows(`id:quick-asset:${photo.id}`)).toBe(false);
  expect(app.shows("id:quick-results")).toBe(false);
  expect(app.value("id:top-search")).toBe("");
});

test("losing a Space drops its results from the open list", async () => {
  const w = quickWorld();
  const { ev } = seedRoadmap(w);
  const app = await signIn(w);
  await app.type("id:top-search", "roadmap");
  await app.waitFor(`id:quick-message:acme:lobby:${ev.id}`);
  w.f.state.spaces.get("acme")!.members.delete("alice");
  await app.waitUntil(() => !app.shows(`id:quick-message:acme:lobby:${ev.id}`), { timeout: 15_000 });
  expect(text(app)).not.toContain("the roadmap review moved to Thursday");
});

test("Search and Library keep their own mode and filters for the lookup", async () => {
  const w = quickWorld();
  const app = await signIn(w);
  // Search, Conversations, scoped to the personal Chat.
  await app.submit("id:top-search");
  await app.waitFor("id:search-view");
  await app.press("id:search-scope:chat");
  await app.type("id:top-search", "plan");
  await lookedUp(app, w, { query: "plan", scope: "chat", mode: "conversations" });
  await app.waitFor("id:quick-empty");
  await app.keyboard.press("escape");

  // The Library page: Documents & media, everything by default.
  await app.press("id:nav-library");
  await app.waitFor("id:library-view");
  await app.clear("id:top-search");
  await app.type("id:top-search", "plan");
  await lookedUp(app, w, { query: "plan", scope: "all", mode: "library" });
  await app.waitFor("id:quick-empty");
});

test("a server without quick search says so, and More results still searches in full", async () => {
  const w = quickWorld();
  w.q.status = 404;
  const app = await signIn(w);
  await app.type("id:top-search", "anything");
  await app.waitFor("id:quick-unavailable");
  await app.press("id:quick-more");
  await app.waitFor("id:search-view");
  await app.waitUntil(() => semantic(w).length === 1);
});

test("Tab leaves the box and closes the list; focus alone opens nothing", async () => {
  const w = quickWorld();
  seedRoadmap(w);
  const app = await signIn(w);
  await app.type("id:top-search", "roadmap");
  await app.waitFor("id:quick-results");
  await app.keyboard.press("tab");
  await app.waitFor("id:quick-results", { state: "gone" });
  const calls = quickCalls(w);
  await app.keyboard.press("mod+k");
  await app.wait(400);
  expect(app.shows("id:quick-results")).toBe(false);
  expect(quickCalls(w)).toBe(calls);
});

test("a query longer than quick search takes sends no lookup; More and Return still search in full", async () => {
  const w = quickWorld();
  const app = await signIn(w);
  const long = "é".repeat(201); // Unicode characters, as the server counts them
  await app.type("id:top-search", long);
  await app.waitFor("id:quick-too-long");
  expect(text(app)).toContain("Open full search for longer queries.");
  await app.wait(400);
  expect(quickCalls(w)).toBe(0);
  await app.press("id:quick-more");
  await app.waitFor("id:search-view");
  await app.waitUntil(() => semantic(w).length === 1);
});

test("Down walks a full list of eight to More, which is last", async () => {
  const w = quickWorld();
  const files = Array.from({ length: 8 }, (_, i) => w.addAsset("alice", null, `note ${i}.txt`, "x"));
  w.q.answers.set("note", () => ({ assets: files.map(w.wireAsset), messages: [] }));
  const app = await open("../../App.tsx", { network: w.network as any, platform: "linux", width: 900, height: 520 });
  await signIn(w, "alice", app);
  await app.type("id:top-search", "note");
  await app.waitFor(`id:quick-asset:${files[7]!.id}`);
  for (let i = 0; i < 9; i++) await app.keyboard.press("down");
  await app.keyboard.press("enter"); // the ninth row: More results
  await app.waitFor("id:search-view");
  await app.waitUntil(() => w.l.log.some((l) => l.path === "/v1/library/search")); // only files: Documents & media
});

test("a Chat message and a Space message with the same event id are two rows; the global box says files and messages", async () => {
  const w = quickWorld();
  w.q.answers.set("same", () => ({
    assets: [],
    messages: [
      { space_id: null, room_id: null, event_id: 5, actor: "alice", text: "same in my Chat" },
      { space_id: "acme", room_id: "lobby", event_id: 5, actor: "bob", text: "same in Acme" },
    ],
  }));
  const app = await signIn(w);
  expect(app.shows("label:Search files and messages in your Chat, library and Spaces; Return searches")).toBe(true);
  expect(app.shows("text:Search files and messages")).toBe(true);
  await app.type("id:top-search", "same");
  await app.waitFor("id:quick-message:chat:chat:5");
  expect(app.shows("id:quick-message:chat:chat:5 >> text:same in my Chat")).toBe(true);
  expect(app.shows("id:quick-message:acme:lobby:5 >> text:same in Acme")).toBe(true);
  await app.keyboard.press("escape");
  // The tabs keep their own names.
  await app.submit("id:top-search");
  await app.waitFor("id:search-view");
  expect(app.shows("label:Search messages in your Chat and Spaces; Return searches")).toBe(true);
  await app.press("id:nav-library");
  await app.waitFor("id:library-view");
  expect(app.shows("label:Search your documents and media; Return searches")).toBe(true);
});

const libSearches = (w: QW) => w.l.log.filter((l) => l.path === "/v1/library/search" && l.method === "POST").map((l) => JSON.parse(l.body!));

test("the Library page looks where its picker shows, and More and Return search there", async () => {
  const w = quickWorld();
  const app = await signIn(w);
  await app.press("id:nav-library");
  await app.waitFor("id:library-view");

  // Personal: the quick lookup and More's full search both stay in the personal library.
  // "Show files from": Everything, Your personal library, then each Space (Acme, Side project).
  await app.press("id:library-list-scope");
  await app.keyboard.press("down");
  await app.keyboard.press("enter");
  await app.waitUntil(() => w.l.log.some((l) => l.url.endsWith("/v1/library/assets?scope=personal")));
  await app.type("id:top-search", "plan");
  await lookedUp(app, w, { query: "plan", scope: "personal", mode: "library" });
  await app.waitFor("id:quick-empty");
  await app.press("id:quick-more");
  await app.waitFor("id:search-view");
  await app.waitUntil(() => libSearches(w).length === 1);
  expect(libSearches(w)[0]).toEqual({ query: "plan", scope: "personal", limit: 10 });

  // A Space: the quick lookup and an unselected Return stay in it.
  await app.press("id:nav-library");
  await app.waitFor("id:library-view");
  await app.press("id:library-list-scope");
  await app.keyboard.press("down");
  await app.keyboard.press("enter");
  await app.waitUntil(() => w.l.log.some((l) => l.url.endsWith("scope=space&space_id=acme")));
  await app.clear("id:top-search");
  await app.type("id:top-search", "plan");
  await lookedUp(app, w, { query: "plan", scope: "space", space_id: "acme", mode: "library" });
  await app.waitFor("id:quick-empty");
  await app.submit("id:top-search");
  await app.waitFor("id:search-view");
  await app.waitUntil(() => libSearches(w).length === 2);
  expect(libSearches(w)[1]).toEqual({ query: "plan", scope: "space", space_id: "acme", limit: 10 });
});

test("More from the general bar searches everything in its corpus, keeping the query, never an old Space or type filter", async () => {
  const w = quickWorld();
  const doc = w.addAsset("alice", null, "budget notes.md", "# budget");
  w.q.answers.set("budget", () => ({ assets: [w.wireAsset(doc)], messages: [] }));
  const { ev } = seedRoadmap(w);
  const app = await signIn(w);
  // Earlier full searches left narrow filters behind: Side and Images for files, the personal Chat for messages.
  await app.submit("id:top-search");
  await app.waitFor("id:search-view");
  await app.press("id:search-scope:chat");
  await app.press("id:search-surface:library");
  await app.press("id:library-search-scope:space:side");
  await app.press("id:library-search-kind:image");
  await app.press("id:search-surface:conversations");
  await app.press("id:nav-chat");

  // Files only: Documents & media, everything, any type.
  await app.type("id:top-search", "budget");
  await lookedUp(app, w, { query: "budget", scope: "all", mode: "all" });
  await app.waitFor(`id:quick-asset:${doc.id}`);
  await app.press("id:quick-more");
  await app.waitFor("id:search-view");
  await app.waitUntil(() => libSearches(w).length === 1);
  expect(libSearches(w)[0]).toEqual({ query: "budget", scope: "all", limit: 10 });
  expect(app.value("id:top-search")).toBe("budget");

  // Files and messages: the box's own mode (back to Conversations), everything.
  await app.press("id:search-surface:conversations");
  await app.press("id:nav-chat");
  await app.clear("id:top-search");
  await app.type("id:top-search", "roadmap");
  await app.waitFor(`id:quick-message:acme:lobby:${ev.id}`);
  await app.press("id:quick-more");
  await app.waitFor("id:search-view");
  await app.waitUntil(() => semantic(w).length === 1);
  expect(semantic(w)[0]!.body).toEqual({ query: "roadmap", scope: "all", limit: 20 });
  expect(app.value("id:top-search")).toBe("roadmap");
});
