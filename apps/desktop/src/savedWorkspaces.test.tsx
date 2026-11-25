// Saved servers and switching workspaces from the account menu, headless:
// two separate in-memory servers (A at the default loopback address, B at an
// https address), and a fake launcher hosting "On this computer". Fake
// tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, header, TOKENS, world } from "../test/fakeServer";
import { LAUNCHED_ENV, launched } from "../test/launched";
import { pagesFixture, TEAM } from "./features/pages/test/fixtureServer";
import { sampleData } from "./features/pages/test/sample";

type F = ReturnType<typeof world>;
type Req = { method: string; url: string; headers: Record<string, string>; body?: string | null };

const REMOTE = "https://metro.example.org";
const DOWN = "https://down.example.org";
const BRIDGE = "http://127.0.0.1:47123";
const BRIDGE_TOKEN = "cd".repeat(32);
const LOCAL = "http://127.0.0.1:41234";
const env = { process: { "default.env": { ...LAUNCHED_ENV, METRO_DESKTOP_BRIDGE_URL: BRIDGE, METRO_DESKTOP_BRIDGE_TOKEN: BRIDGE_TOKEN } } };

/** Serves `server` at `origin` instead of BASE. */
function at(origin: string, server: F) {
  const network: Record<string, (req: Req) => unknown> = {};
  for (const m of ["GET", "POST", "PUT", "PATCH", "DELETE"]) {
    const inner = (server.network as any)[`${m} ${BASE}/v1/*`];
    network[`${m} ${origin}/v1/*`] = (req: Req) => inner({ ...req, url: req.url.replace(origin, BASE) });
  }
  return network;
}

/** A launcher that hosts the local workspace (carol's, on its own server) or refuses to. */
function launcher(opts: { refuse?: boolean; statusDelay?: number } = {}) {
  const log: string[] = [];
  const ok = (body: unknown) => ({ status: 200, headers: { "cache-control": "no-store" }, body });
  const status = () => ({ bridge: 1, version: "test", instance: "primary", kinds: [], session: null, connections: [], local: "stopped" });
  const handle = (req: Req) => {
    const path = new URL(req.url).pathname;
    log.push(`${req.method} ${path}`);
    if (header(req as any, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return { status: 401, body: { error: { code: "unauthorized", message: "Unauthorized." } } };
    if (path === "/v1/status") return { ...ok(status()), delay: opts.statusDelay ?? 0 };
    if (path === "/v1/session") return ok(status());
    if (path === "/v1/local-workspace/open") {
      if (opts.refuse) return { status: 503, body: { error: { code: "local_failed", message: "The local server didn't start." } } };
      return ok({ server_url: LOCAL, token: TOKENS.carol });
    }
    return { status: 404, body: { error: { code: "not_found", message: "Unknown path." } } };
  };
  const network: Record<string, (req: Req) => unknown> = {};
  for (const m of ["GET", "POST", "PUT", "DELETE"]) network[`${m} ${BRIDGE}/v1/*`] = handle;
  return {
    network, opens: () => log.filter((l) => l === "POST /v1/local-workspace/open").length,
    statuses: () => log.filter((l) => l === "GET /v1/status").length,
  };
}

function servers() {
  const a = world();
  const b = world();
  b.addSpace("beta", "Beta", "bob");
  const local = world();
  local.addSpace("solo", "Solo", "carol");
  return { a, b, local };
}

async function launch(s: ReturnType<typeof servers>, opts: { storage?: Record<string, string>; bridge?: ReturnType<typeof launcher>; manualClock?: boolean; probe?: boolean } = {}): Promise<App> {
  const network = { ...(s.a.network as any), ...at(REMOTE, s.b), ...at(LOCAL, s.local), ...(opts.bridge ? opts.bridge.network : {}) };
  return open(opts.probe ? "../StartupAccessProbe.fixture.tsx" : "../App.tsx", { network, platform: "linux", width: 1200, height: 820,
    storage: opts.storage ?? {}, modules: (opts.bridge ? env : launched()) as any, ...(opts.manualClock ? { clock: {} } : {}) });
}

async function signIn(app: App, user: string, url = BASE, opts: { keep?: boolean } = {}) {
  await app.clear("id:server-url");
  await app.type("id:server-url", url);
  await app.type("id:access-token", TOKENS[user]!);
  if (opts.keep) await app.press("id:keep-signed-in");
  await app.press("id:connect");
  await app.waitFor("id:account-menu");
}

const saved = (app: App): { id: string; name: string; baseUrl: string }[] => JSON.parse(app.storage()["metro.savedServers"] ?? "[]");
const idOf = (app: App, baseUrl: string) => saved(app).find((s) => s.baseUrl === baseUrl)!.id;
const noTokensStored = (app: App) => {
  const all = JSON.stringify(app.storage());
  for (const t of Object.values(TOKENS)) expect(all).not.toContain(t);
};
/** Every request to `origin` carried only `user`'s token. */
const onlyTokenFor = (app: App, origin: string, user: string) => {
  const reqs = app.requests().filter((r: any) => r.url.startsWith(origin));
  for (const r of reqs as any[]) {
    const auth = Object.entries(r.headers ?? {}).find(([k]) => k.toLowerCase() === "authorization")?.[1];
    expect(auth).toBe(`Bearer ${TOKENS[user]}`);
  }
  return reqs.length;
};

async function addServer(app: App, url: string, token: string, name = "", opts: { keep?: boolean } = {}) {
  await app.press("id:account-menu");
  await app.press("id:add-server");
  await app.waitFor("id:workspace-dialog");
  if (name) await app.type("id:add-server-name", name);
  await app.type("id:add-server-url", url);
  await app.type("id:add-server-token", token);
  if (opts.keep) await app.press("id:add-server-keep");
  await app.press("id:add-server-submit");
}

async function menuSwitch(app: App, itemID: string) {
  await app.press("id:account-menu");
  await app.press(`id:${itemID}`);
}

test("connecting saves the server's name and address, never its token; the menu lists it as the one in use", async () => {
  const s = servers();
  const app = await launch(s);
  expect(app.shows("id:server-url")).toBe(true); // a first visit: the form as always
  expect(app.shows("id:saved-servers")).toBe(false);
  await signIn(app, "alice");
  await app.waitUntil(() => saved(app).length === 1);
  expect(saved(app)[0]).toMatchObject({ name: "127.0.0.1:8787", baseUrl: BASE });
  noTokensStored(app);
  await app.press("id:account-menu");
  await app.waitFor(`id:workspace-server:${idOf(app, BASE)}`);
  expect(app.shows("id:add-server")).toBe(true);
  expect(app.shows("id:workspace-local")).toBe(false); // no launcher here
});

test("add a server, then switch back and forth without signing out; each token goes only to its own address", async () => {
  const s = servers();
  const app = await launch(s);
  // Late answers from A while switching: delayed polls must never carry B's token.
  s.a.state.delays.set("/v1/chat/events", 300);
  s.a.state.delays.set("/v1/runs", 300);
  await signIn(app, "alice", BASE, { keep: true });
  await addServer(app, REMOTE, TOKENS.bob!, "Beta team", { keep: true });
  await app.waitFor("id:space:beta");
  expect(app.shows("id:space:acme")).toBe(true); // bob is in Acme on B too; Beta proves it's B
  await app.waitFor("id:workspace-dialog", { state: "gone" });
  expect(saved(app).map((x) => x.name)).toEqual(["127.0.0.1:8787", "Beta team"]);
  // Adoption consumes the identity and Spaces that just passed verification.
  // A second check after tearing down A would strand the user on transient errors.
  for (const path of ["/v1/me", "/v1/spaces"]) {
    expect(app.requests().filter((r: any) => r.method === "GET" && r.url === REMOTE + path)).toHaveLength(1);
  }

  // Back to A straight from the menu: no token asked (kept in memory this run, as asked).
  await menuSwitch(app, `workspace-server:${idOf(app, BASE)}`);
  await app.waitUntil(() => !app.shows("id:space:beta"));
  await app.waitFor("id:space:acme");
  expect(app.shows("id:workspace-dialog")).toBe(false);
  // And to B again.
  await menuSwitch(app, `workspace-server:${idOf(app, REMOTE)}`);
  await app.waitFor("id:space:beta");
  await app.wait(700); // let A's delayed requests land

  expect(onlyTokenFor(app, REMOTE, "bob")).toBeGreaterThan(0);
  expect(onlyTokenFor(app, BASE, "alice")).toBeGreaterThan(0);
  noTokensStored(app);
});

test("a refused token, or a server that doesn't answer, leaves the current session as it was", async () => {
  const s = servers();
  const app = await launch(s);
  await signIn(app, "alice");
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");

  await addServer(app, REMOTE, "tok-wrong");
  await app.waitFor("id:workspace-error");
  expect(app.texts().join(" ")).toContain("The access token was refused.");
  expect(app.shows("id:workspace-dialog")).toBe(true); // still asking
  expect(app.shows("text:hello from bob in acme")).toBe(true); // still in A's Acme
  expect(saved(app).map((x) => x.baseUrl)).toEqual([BASE]); // never signed in: not kept
  await app.back();
  await app.waitFor("id:workspace-dialog", { state: "gone" });

  await addServer(app, DOWN, "tok-any");
  await app.waitFor("id:workspace-error");
  expect(app.shows("text:hello from bob in acme")).toBe(true);
  expect(app.shows("id:connect-screen")).toBe(false);
  expect(saved(app).map((x) => x.baseUrl)).toEqual([BASE]);
});

test("an unsent message is never dropped without asking", async () => {
  const s = servers();
  const app = await launch(s);
  await signIn(app, "alice", BASE, { keep: true });
  await addServer(app, REMOTE, TOKENS.bob!, "", { keep: true });
  await app.waitFor("id:space:beta");
  await menuSwitch(app, `workspace-server:${idOf(app, BASE)}`);
  await app.waitUntil(() => !app.shows("id:space:beta"));
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme");
  await app.type("id:composer", "half-written");

  await menuSwitch(app, `workspace-server:${idOf(app, REMOTE)}`);
  await app.waitFor("id:workspace-keep-draft");
  await app.press("id:workspace-keep-draft");
  await app.waitFor("id:workspace-dialog", { state: "gone" });
  expect(app.value("id:composer")).toBe("half-written");
  expect(app.shows("id:space:beta")).toBe(false);

  await menuSwitch(app, `workspace-server:${idOf(app, REMOTE)}`);
  await app.press("id:workspace-discard-switch");
  await app.waitFor("id:space:beta");
});

test("Sign out forgets that server's token; another saved server switched away from is still one press", async () => {
  const s = servers();
  const app = await launch(s);
  await signIn(app, "alice", BASE, { keep: true });
  await addServer(app, REMOTE, TOKENS.bob!, "", { keep: true });
  await app.waitFor("id:space:beta");
  await menuSwitch(app, "disconnect");
  await app.waitFor("id:connect-screen");
  await app.waitFor("id:saved-servers");

  // B (signed out of): its address goes in the form, nothing is sent there.
  const before = app.requests().filter((r: any) => r.url.startsWith(REMOTE)).length;
  await app.press(`id:saved-server:${idOf(app, REMOTE)}`);
  expect(app.value("id:server-url")).toBe(REMOTE);
  expect(app.value("id:access-token")).toBe("");
  await app.wait(200);
  expect(app.requests().filter((r: any) => r.url.startsWith(REMOTE)).length).toBe(before);

  // A (left by switching): straight in.
  await app.press(`id:saved-server:${idOf(app, BASE)}`);
  await app.waitFor("id:account-menu");
  await app.waitFor("id:space:acme");
  expect(app.shows("id:space:beta")).toBe(false);
});

test("after a restart the saved list is there and no token is: the first screen is quiet until a server is chosen", async () => {
  const s = servers();
  const list = JSON.stringify([{ id: "srv-b", name: "Beta team", baseUrl: REMOTE }]);
  const app = await launch(s, { storage: { "metro.savedServers": list } });
  await app.waitFor("id:saved-server:srv-b");
  expect(app.shows("id:server-url")).toBe(false);
  expect(app.shows("id:show-server-form")).toBe(true);
  await app.press("id:saved-server:srv-b");
  expect(app.value("id:server-url")).toBe(REMOTE);
  await app.type("id:access-token", TOKENS.bob!);
  await app.press("id:connect");
  await app.waitFor("id:space:beta");
  expect(saved(app)).toEqual([{ id: "srv-b", name: "Beta team", baseUrl: REMOTE }]);
  noTokensStored(app);
});

test("a saved list with a bad address is read without it", async () => {
  const s = servers();
  const list = JSON.stringify([{ id: "x", name: "Bad", baseUrl: "ftp://example.org" }, { id: "y", name: "Ok", baseUrl: REMOTE }, "junk"]);
  const app = await launch(s, { storage: { "metro.savedServers": list } });
  await app.waitFor("id:saved-server:y");
  expect(app.shows("id:saved-server:x")).toBe(false);
});

test("On this computer straight from a signed-in menu, and back to a saved server", async () => {
  const s = servers();
  const bridge = launcher();
  const app = await launch(s, { bridge });
  await app.waitFor("id:space:solo");
  await menuSwitch(app, "disconnect");
  await app.waitFor("id:connect-screen");
  await signIn(app, "alice", BASE, { keep: true });
  await menuSwitch(app, "workspace-local");
  await app.waitFor("text:On this device");
  await app.waitFor("id:space:solo");
  expect(bridge.opens()).toBe(2);
  // The local workspace isn't added to the saved list, and its bearer is never kept.
  expect(saved(app).map((x) => x.baseUrl)).toEqual([BASE]);
  noTokensStored(app);

  await menuSwitch(app, `workspace-server:${idOf(app, BASE)}`);
  await app.waitFor("id:space:acme");
  expect(app.shows("id:space:solo")).toBe(false);
  expect(onlyTokenFor(app, LOCAL, "carol")).toBeGreaterThan(0);
  expect(onlyTokenFor(app, BASE, "alice")).toBeGreaterThan(0);
});

test("the local workspace's account menu and Chat composer keep the names the native packaged check reads", async () => {
  // release.yml (desktop-windows) finds AutomationId account-menu, requires its
  // name to end with ", on this device. Account menu", and an enabled composer.
  const app = await launch(servers(), { bridge: launcher() });
  await app.waitFor("id:space:solo");
  await app.press("id:nav-chat");
  await app.waitFor("id:composer");
  expect(app.shows("id:account-menu")).toBe(true);
  expect(app.shows("label:/^carol, Connection: Online, on this device\\. Account menu$/")).toBe(true);
  expect(app.enabled("id:composer")).toBe(true);
});

test("when this computer's workspace can't open, the current session stays", async () => {
  const s = servers();
  const app = await launch(s, { bridge: launcher({ refuse: true }) });
  await app.waitFor("id:local-error");
  await signIn(app, "alice");
  await menuSwitch(app, "workspace-local");
  await app.waitFor("id:workspace-error");
  expect(app.texts().join(" ")).toContain("The local server didn't start.");
  expect(app.shows("id:space:acme")).toBe(true);
  expect(app.shows("id:connect-screen")).toBe(false);
});

const choices = JSON.stringify([
  { id: "srv-a", name: "Work", baseUrl: BASE },
  { id: "srv-b", name: "Team B", baseUrl: REMOTE },
]);

const rememberedRemote = {
  "metro.savedServers": choices,
  "metro.startupServer": "srv-b",
  "metro.lastSpace": JSON.stringify({ server: REMOTE, space: "beta" }),
};
const betaRooms = "/v1/spaces/beta/rooms";
const betaEvents = `${betaRooms}/lobby/events`;
const acmeRooms = "/v1/spaces/acme/rooms";
const remotePaths = (s: ReturnType<typeof servers>) => s.b.state.log.map(r => r.path);
const firstAcme = `${acmeRooms}/lobby/events?after=0`;
const firstBeta = `${betaEvents}?after=0`;
const firstRoomPages = (s: ReturnType<typeof servers>) => remotePaths(s).filter(p => p.includes("/rooms/") && p.endsWith("/events?after=0"));
const chatStarted = (s: ReturnType<typeof servers>) => remotePaths(s)
  .some(p => p.startsWith("/v1/chat/") || p === "/v1/runs" || p === "/v1/models/available" || p === "/v1/providers");
const chatPolled = (s: ReturnType<typeof servers>) => remotePaths(s)
  .some(p => p.startsWith("/v1/chat/") || p === "/v1/runs");

async function submitSavedStartup(app: App) {
  await app.waitFor("text:Sign in to Team B");
  await app.type("id:access-token", TOKENS.bob!);
  await app.press("id:connect");
}

test("saved remote verifies before showing the scoped Shell and room hydration never boots Chat", async () => {
  const s = servers(), bridge = launcher();
  const who = s.b.answerLate("/v1/me", TOKENS.bob!, 40);
  const spaces = s.b.answerLate("/v1/spaces", TOKENS.bob!, 70);
  const rooms = s.b.answerLate(betaRooms, TOKENS.bob!, 300);
  // A nonempty first page intentionally causes one catch-up request.
  const message = s.b.post("beta/lobby", { kind: "message", actor: "bob", text: "First remote room message" });
  const models = s.b.answerLate("/v1/models/available?space_id=beta", TOKENS.bob!, 900);
  const app = await launch(s, { bridge, storage: rememberedRemote, manualClock: true });
  await app.waitFor("text:Sign in to Team B");
  expect(remotePaths(s)).toEqual([]);
  await submitSavedStartup(app);
  await app.waitUntil(() => who.arrived() === 1 && spaces.arrived() === 1, { timeout: 25 });
  expect(app.shows("id:workspace")).toBe(false);
  expect(rooms.arrived()).toBe(0);
  await app.waitFor("id:rooms-loading", { timeout: 150 });
  expect(app.shows("id:space:beta")).toBe(true);
  expect(app.shows("id:chat-view")).toBe(false);
  expect(app.shows("id:composer")).toBe(false);
  expect(app.shows("text:No room selected")).toBe(false);
  expect(app.shows("text:Loading rooms…")).toBe(true);
  expect(rooms.arrived()).toBe(1);
  expect(remotePaths(s).some(p => p.includes("/events") || p.startsWith("/v1/models/available"))).toBe(false);
  await app.waitFor("text:First remote room message", { timeout: 340 });
  await app.waitUntil(() => remotePaths(s).includes(`${betaEvents}?after=${message.id}`), { timeout: 100 });
  const paths = remotePaths(s);
  for (const path of ["/v1/me", "/v1/spaces", betaRooms]) expect(paths.filter(p => p === path)).toHaveLength(1);
  expect(models.arrived()).toBe(1);
  expect(paths.filter(p => p.includes("/events?"))).toEqual([`${betaEvents}?after=0`, `${betaEvents}?after=${message.id}`]);
  expect(chatStarted(s)).toBe(false);
  expect(bridge.opens()).toBe(0);
});

test("a 401 during remote room hydration clears the Shell without committing a new saved connection", async () => {
  const s = servers();
  const get = s.b.network[`GET ${BASE}/v1/*`]!;
  s.b.network[`GET ${BASE}/v1/*`] = (req) => new URL(req.url).pathname === betaRooms
    ? { status: 401, body: { error: "invalid token" }, delay: 300 }
    : get(req);
  const app = await launch(s, { manualClock: true, storage: { "metro.lastSpace": rememberedRemote["metro.lastSpace"] } });
  await app.clear("id:server-url");
  await app.type("id:server-url", REMOTE);
  await app.type("id:access-token", TOKENS.bob!);
  await app.press("id:keep-signed-in");
  await app.press("id:connect");
  await app.waitFor("id:rooms-loading", { timeout: 120 });
  await app.waitFor("id:connect-error", { timeout: 350 });
  expect(app.shows("id:workspace")).toBe(false);
  expect(app.shows("id:space:beta")).toBe(false);
  expect(app.value("id:access-token")).toBe("");
  expect(saved(app)).toEqual([]);
  expect(remotePaths(s).some(p => p.includes("/events"))).toBe(false);
});

test("a delayed room refusal removes the Space without undoing deliberate navigation", async () => {
  const s = servers();
  const get = s.b.network[`GET ${BASE}/v1/*`]!;
  s.b.network[`GET ${BASE}/v1/*`] = req => {
    if (new URL(req.url).pathname === betaRooms) s.b.state.spaces.get("beta")!.members.delete("bob");
    return get(req);
  };
  s.b.answerLate(betaRooms, TOKENS.bob!, 300);
  const app = await launch(s, { storage: rememberedRemote, manualClock: true });
  await submitSavedStartup(app);
  await app.waitFor("id:rooms-loading", { timeout: 100 });
  await app.press("id:nav-library");
  await app.waitFor("id:library-view", { timeout: 150 });
  await app.waitFor("id:space:beta", { state: "gone", timeout: 350 });
  expect(app.shows("id:library-view")).toBe(true);
  expect(app.shows("id:chat-view")).toBe(false);
  expect(remotePaths(s).some(p => p.includes("/events"))).toBe(false);
});

test("clicking a different Space during startup wins before the remembered room replies", async () => {
  const s = servers();
  s.b.post("beta/lobby", { kind: "message", actor: "bob", text: "Old startup target" });
  const old = s.b.answerLate(betaRooms, TOKENS.bob!, 700);
  const acme = s.b.answerLate(acmeRooms, TOKENS.bob!, 80);
  const app = await launch(s, { storage: rememberedRemote, manualClock: true });
  await submitSavedStartup(app);
  await app.waitFor("id:rooms-loading", { timeout: 100 });
  expect(old.arrived()).toBe(1);
  await app.press("id:space:beta"); // a repeat click on the pending Space isn't another read
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme", { timeout: 200 });
  await app.clock!.runFor(750);
  expect(old.arrived()).toBe(1);
  expect(acme.arrived()).toBe(1);
  expect(firstRoomPages(s)).toEqual([firstAcme]);
  expect(app.shows("text:hello from bob in acme")).toBe(true);
  expect(app.shows("text:Old startup target")).toBe(false);
  expect(app.shows("id:space:beta")).toBe(true);
  expect(chatStarted(s)).toBe(false);
  expect(remotePaths(s)).not.toContain("/v1/models/available?space_id=beta");
});

/** The first room request observes revoked access; later requests read current membership. */
function holdBetaDenial(s: ReturnType<typeof servers>, status: 403 | 404) {
  const get = s.b.network[`GET ${BASE}/v1/*`]!;
  const members = s.b.state.spaces.get("beta")!.members;
  let captured = false;
  s.b.network[`GET ${BASE}/v1/*`] = req => {
    if (!captured && new URL(req.url).pathname === betaRooms) {
      captured = true;
      members.delete("bob");
      return { status, body: { error: "Space access denied" }, delay: 650 };
    }
    return get(req);
  };
  return { rejoin: () => members.add("bob") };
}

test("the latest delayed denial for A removes only A while B is still loading", async () => {
  const s = servers();
  holdBetaDenial(s, 404);
  const acme = s.b.answerLate(acmeRooms, TOKENS.bob!, 850);
  const app = await launch(s, { storage: rememberedRemote, manualClock: true });
  await submitSavedStartup(app);
  await app.waitFor("id:rooms-loading", { timeout: 100 });
  await app.press("id:space:acme");
  await app.waitUntil(() => acme.arrived() === 1, { timeout: 100 });
  await app.waitFor("id:space:beta", { state: "gone", timeout: 700 });
  expect(app.shows("id:rooms-loading")).toBe(true);
  expect(app.shows("id:rooms-empty")).toBe(false);
  expect(firstRoomPages(s)).toEqual([]);
  await app.waitFor("text:hello from bob in acme", { timeout: 350 });
  expect(firstRoomPages(s)).toEqual([firstAcme]);
  expect(chatStarted(s)).toBe(false);
});

test("a held denial can't remove a rejoined Space while its newer room entry is pending", async () => {
  const s = servers();
  const beta = holdBetaDenial(s, 403);
  s.b.post("beta/lobby", { kind: "message", actor: "bob", text: "Rejoined Beta" });
  const app = await launch(s, { storage: rememberedRemote, manualClock: true });
  await submitSavedStartup(app);
  await app.waitFor("id:rooms-loading", { timeout: 100 });
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme", { timeout: 180 });
  expect(firstRoomPages(s)).toEqual([firstAcme]);
  beta.rejoin();
  const newRooms = s.b.answerLate(betaRooms, TOKENS.bob!, 900);
  await app.press("id:space:beta");
  await app.waitUntil(() => newRooms.arrived() === 1, { timeout: 100 });
  await app.clock!.runFor(700);
  expect(app.shows("id:rooms-loading")).toBe(true);
  expect(firstRoomPages(s)).toEqual([firstAcme]);
  expect(app.shows("id:space:beta")).toBe(true);
  await app.waitFor("text:Rejoined Beta", { timeout: 350 });
  expect(firstRoomPages(s)).toEqual([firstAcme, firstBeta]);
  expect(chatStarted(s)).toBe(false);
});

for (const dispatchFirst of [true, false]) {
  for (const confirmed of [true, false]) {
    test(`startup denial ${dispatchFirst ? "after" : "before"} a newer ${confirmed ? "verified" : "inconclusive"} Dispatch room read`, async () => {
      const s = servers();
      const beta = holdBetaDenial(s, 404);
      const delay = dispatchFirst ? 40 : 900;
      if (confirmed) {
        s.b.answerLate(betaRooms, TOKENS.bob!, delay);
      } else {
        const get = s.b.network[`GET ${BASE}/v1/*`]!;
        let calls = 0;
        s.b.network[`GET ${BASE}/v1/*`] = req => new URL(req.url).pathname === betaRooms && ++calls === 2
          ? { status: 503, body: { error: "Try again later" }, delay }
          : get(req);
      }
      const app = await launch(s, { storage: rememberedRemote, manualClock: true });
      await submitSavedStartup(app);
      await app.waitFor("id:rooms-loading", { timeout: 100 });
      await app.press("id:nav-chat"); // deliberate Chat; only Dispatch reads Beta again
      await app.press("id:dispatch-new");
      beta.rejoin();
      await app.press("id:dispatch-space:beta");
      if (dispatchFirst) await app.waitFor(confirmed ? "id:dispatch-room:lobby" : "id:dispatch-error", { timeout: 150 });
      else {
        await app.clock!.runFor(700);
        expect(app.shows("id:space:beta")).toBe(true); // an older denial can't invalidate a pending check
        expect(app.shows("id:dispatch-room:lobby")).toBe(false);
      }
      await app.clock!.runFor(350 + (dispatchFirst ? 350 : 0));
      expect(app.shows("id:space:beta")).toBe(confirmed);
      expect(app.shows("id:dispatch-room:lobby")).toBe(confirmed);
      expect(firstRoomPages(s)).toEqual([]);
      expect(app.requests().filter(r => r.url === REMOTE + betaRooms)).toHaveLength(2);
      await app.press("id:space:acme");
      await app.waitFor("text:hello from bob in acme", { timeout: 180 });
      expect(firstRoomPages(s)).toEqual([firstAcme]);
    });
  }
}

for (const betaFirst of [true, false]) {
  test(`membership sync still runs in Library when ${betaFirst ? "A" : "B"}'s superseded entry replies first`, async () => {
    const s = servers();
    const beta = s.b.answerLate(betaRooms, TOKENS.bob!, betaFirst ? 700 : 950);
    const acme = s.b.answerLate(acmeRooms, TOKENS.bob!, betaFirst ? 950 : 700);
    const app = await launch(s, { storage: rememberedRemote, manualClock: true });
    await submitSavedStartup(app);
    await app.waitFor("id:rooms-loading", { timeout: 100 });
    await app.press("id:space:acme");
    await app.waitUntil(() => acme.arrived() === 1, { timeout: 100 });
    await app.press("id:nav-library");
    await app.waitFor("id:library-view", { timeout: 100 });
    s.b.state.spaces.get("beta")!.members.delete("bob");
    await app.clock!.runFor(11_500);
    expect(app.shows("id:space:beta")).toBe(false);
    expect(app.shows("id:space:acme")).toBe(true);
    expect(app.shows("id:library-view")).toBe(true);
    expect(remotePaths(s).filter(p => p === "/v1/spaces")).toHaveLength(2);
    expect(beta.arrived()).toBe(1);
    expect(acme.arrived()).toBe(1);
    expect(firstRoomPages(s)).toEqual([]);
    // Library loads its own model capabilities; there must be no Chat UI or poll.
    expect(app.shows("id:chat-view")).toBe(false);
    expect(remotePaths(s).some(p => p.startsWith("/v1/chat/") || p === "/v1/runs")).toBe(false);
  });
}

test("a held membership omission can't erase a newer pending Dispatch read after rejoin", async () => {
  const s = servers();
  const app = await launch(s, { storage: { ...rememberedRemote, "metro.lastSpace": "" }, manualClock: true });
  await submitSavedStartup(app);
  await app.waitFor("id:chat-empty", { timeout: 100 });
  const omitted = s.b.answerLate("/v1/spaces", TOKENS.bob!, 500);
  const members = s.b.state.spaces.get("beta")!.members;
  members.delete("bob");
  await app.waitUntil(() => omitted.arrived() === 1, { timeout: 11_500 });
  members.add("bob");
  const rooms = s.b.answerLate(betaRooms, TOKENS.bob!, 800);
  await app.press("id:dispatch-new");
  await app.press("id:dispatch-space:beta");
  await app.waitUntil(() => rooms.arrived() === 1, { timeout: 100 });
  await app.clock!.runFor(550);
  expect(app.shows("id:space:beta")).toBe(true);
  expect(app.shows("id:dispatch-room:lobby")).toBe(false);
  await app.waitFor("id:dispatch-room:lobby", { timeout: 350 });
  expect(app.shows("id:space:beta")).toBe(true);
  expect(firstRoomPages(s)).toEqual([]);
});

test("a newer Dispatch refusal wins over an older successful room snapshot", async () => {
  const s = servers();
  const held = s.b.answerLate(betaRooms, TOKENS.bob!, 700);
  const app = await launch(s, { storage: rememberedRemote, manualClock: true });
  await submitSavedStartup(app);
  await app.waitFor("id:rooms-loading", { timeout: 100 });
  expect(held.arrived()).toBe(1);
  s.b.state.spaces.get("beta")!.members.delete("bob");
  s.b.answerLate(betaRooms, TOKENS.bob!, 30);
  await app.press("id:nav-chat");
  await app.press("id:dispatch-new");
  await app.press("id:dispatch-space:beta");
  await app.waitFor("id:space:beta", { state: "gone", timeout: 150 });
  await app.clock!.runFor(750);
  expect(app.shows("id:space:beta")).toBe(false);
  expect(app.shows("id:dispatch-room:lobby")).toBe(false);
  expect(firstRoomPages(s)).toEqual([]);
});

async function launchWithoutBeta(s: ReturnType<typeof servers>) {
  s.b.state.spaces.get("beta")!.members.delete("bob");
  const app = await launch(s, { storage: { ...rememberedRemote, "metro.lastSpace": "" }, manualClock: true, probe: true });
  await submitSavedStartup(app);
  await app.waitFor("id:workspace", { timeout: 120 });
  expect(app.shows("id:space:beta")).toBe(false);
  return app;
}

async function startMembershipProbe(s: ReturnType<typeof servers>, app: App, delay: number) {
  const request = s.b.answerLate("/v1/spaces", TOKENS.bob!, delay);
  await app.press("id:probe-refresh");
  await app.waitUntil(() => request.arrived() === 1, { timeout: 100 });
}

for (const scenario of ["chat", "library", "newer original-room proof"] as const) {
  const covered = scenario === "newer original-room proof";
  const destination = scenario === "chat" ? "chat" : "library";
  test(`an obsolete denial probe ${
    covered ? "does not repeat a check when the original room has newer access evidence" : `in ${destination} still checks the original missing room`
  }`, async () => {
    const s = servers();
    const thread = { id: "probe-thread", name: "Probe thread", parent_room_id: "lobby" };
    s.b.addRoom("beta", thread);
    s.b.post("beta/lobby", { kind: "message", actor: "bob", text: "Beta lobby stays" });
    const post = s.b.network[`POST ${BASE}/v1/*`]!;
    s.b.network[`POST ${BASE}/v1/*`] = req => new URL(req.url).pathname === `${betaRooms}/${thread.id}/messages`
      ? { status: 404, body: { error: "Original room no longer visible" } }
      : post(req);
    const app = await launch(s, { storage: rememberedRemote, manualClock: true, probe: true });
    await submitSavedStartup(app);
    await app.waitFor("text:Beta lobby stays");
    await app.press(`id:room:${thread.id}`);
    const held = s.b.answerLate("/v1/spaces", TOKENS.bob!, 650);
    await app.type("id:composer", "Denied in the original thread");
    await app.press("id:send");
    await app.waitUntil(() => held.arrived() === 1, { timeout: 150 });
    await app.press("id:space:acme");
    await app.waitFor("text:hello from bob in acme", { timeout: 150 });
    await app.press(`id:nav-${destination}`);
    await app.waitFor(`id:${destination}-view`);
    if (destination === "chat") await app.type("id:composer", "Keep the personal draft");
    if (!covered) s.b.state.rooms.set("beta", s.b.state.rooms.get("beta")!.filter(r => r.id !== thread.id));
    s.b.addSpace("fresh", "New membership snapshot", "bob");
    await startMembershipProbe(s, app, 0);
    await app.waitFor("id:space:fresh", { timeout: 150 });
    const roomReads = () => remotePaths(s).filter(path => path === betaRooms).length;
    expect(roomReads()).toBe(1);
    if (covered) {
      s.b.post(`beta/${thread.id}`, { kind: "message", actor: "bob", text: "The original room is still visible" });
      await app.press("id:space:beta");
      await app.waitFor("text:The original room is still visible", { timeout: 150 });
      await app.press("id:nav-library");
      await app.clock!.runFor(670);
      expect(roomReads()).toBe(1);
    } else {
      await app.waitUntil(() => roomReads() === 2, { timeout: 700 });
    }
    expect(app.shows(`id:${destination}-view`)).toBe(true);
    expect(app.shows("id:action-error")).toBe(false);
    if (destination === "chat") expect(app.value("id:composer")).toBe("Keep the personal draft");
    else expect(chatPolled(s)).toBe(false);
    expect(remotePaths(s).filter(path => path === acmeRooms)).toHaveLength(1);
    if (covered) return;
    await app.press("id:space:beta");
    await app.waitFor("text:Beta lobby stays");
    expect(app.shows(`id:room:${thread.id}`)).toBe(false);
    expect(roomReads()).toBe(2); // returning used the corrected cache, not a fresh list
  });
}

for (const outcome of ["transcript first", "omission first with empty transcript", "timeout", "session abort"] as const) {
  test(`held membership omission respects a pending rejoined transcript: ${outcome}`, async () => {
    const s = servers();
    const app = await launch(s, { storage: rememberedRemote, manualClock: true, probe: true });
    await submitSavedStartup(app);
    await app.waitUntil(() => firstRoomPages(s).includes(firstBeta), { timeout: 150 });
    await app.press("id:nav-library");
    await app.waitFor("id:library-view");
    const members = s.b.state.spaces.get("beta")!.members;
    members.delete("bob");
    await startMembershipProbe(s, app, 500);
    members.add("bob");
    const cutoff = outcome === "timeout";
    const first = outcome === "transcript first";
    if (first) s.b.post("beta/lobby", { kind: "message", actor: "bob", text: "After rejoining Beta" });
    const events = s.b.answerLate(betaEvents, TOKENS.bob!, cutoff ? 20_000 : first ? 80 : 800);
    await app.press("id:space:beta"); // use the cached room list; only its transcript proves the rejoin
    await app.waitUntil(() => events.arrived() === 1, { timeout: 100 });
    if (first) await app.waitFor("text:After rejoining Beta", { timeout: 150 });
    else await app.press("id:nav-library");
    await app.clock!.runFor(560);
    expect(app.shows("id:space:beta")).toBe(true);
    expect(events.arrived()).toBe(1);
    if (cutoff) {
      // Keep the next periodic snapshot from re-granting this test Space.
      members.delete("bob");
      await app.waitFor("id:space:beta", { state: "gone", timeout: 10_000 });
      expect(app.shows("id:library-view")).toBe(true);
    } else if (outcome === "session abort") {
      await menuSwitch(app, "disconnect");
      await app.waitFor("id:connect-screen");
      members.add("alice");
      await app.press(`id:saved-server:${idOf(app, REMOTE)}`);
      await signIn(app, "alice", REMOTE);
      await app.clock!.runFor(350);
      expect(app.shows("id:space:beta")).toBe(true);
    } else {
      await app.clock!.runFor(350);
      expect(app.shows("id:space:beta")).toBe(true);
      if (first) expect(app.shows("text:After rejoining Beta")).toBe(true);
      else expect(app.shows("id:library-view")).toBe(true);
    }
    expect(remotePaths(s).filter(p => p === betaRooms)).toHaveLength(first || outcome === "session abort" ? 2 : 1);
    expect(chatPolled(s)).toBe(false);
  });
}

test("an older room-list omission waits for the exact pending transcript, not a sibling", async () => {
  const s = servers();
  s.b.addRoom("beta", { id: "stale-sibling", name: "Stale sibling" });
  const app = await launch(s, { storage: rememberedRemote, manualClock: true, probe: true });
  await submitSavedStartup(app);
  await app.waitUntil(() => firstRoomPages(s).includes(firstBeta), { timeout: 150 });
  const original = s.b.state.rooms.get("beta")!;
  s.b.state.rooms.set("beta", []);
  const list = s.b.answerLate(betaRooms, TOKENS.bob!, 500);
  await app.press("id:probe-refresh");
  await app.waitUntil(() => list.arrived() === 1, { timeout: 100 });
  s.b.state.rooms.set("beta", original.filter(r => r.id !== "stale-sibling"));
  const events = s.b.answerLate(betaEvents, TOKENS.bob!, 800);
  await app.press("id:nav-library");
  await app.press("id:space:beta");
  await app.waitUntil(() => events.arrived() === 1, { timeout: 100 });
  await app.clock!.runFor(550);
  expect(app.shows("id:room:lobby")).toBe(true);
  expect(app.shows("id:room:stale-sibling")).toBe(false);
  await app.clock!.runFor(350);
  expect(app.shows("id:room:lobby")).toBe(true);
  expect(app.shows("id:room:stale-sibling")).toBe(false);
  expect(chatPolled(s)).toBe(false);
});

async function launchInEmptyBeta(s: ReturnType<typeof servers>) {
  s.b.state.rooms.set("beta", []);
  const app = await launch(s, { storage: rememberedRemote, manualClock: true, probe: true });
  await submitSavedStartup(app);
  await app.waitFor("id:rooms-empty", { timeout: 150 });
  return app;
}

const createdName = "Startup proof";
function createdSpace(s: ReturnType<typeof servers>) {
  return [...s.b.state.spaces.values()].find(space => space.name === createdName);
}

async function startCreate(app: App) {
  await app.press("id:new-space-toggle");
  await app.type("id:new-space-name", createdName);
  await app.press("id:create-space");
}

async function submitCreate(s: ReturnType<typeof servers>, app: App) {
  await startCreate(app);
  await app.waitUntil(() => !!createdSpace(s), { timeout: 120 });
  return createdSpace(s)!;
}

function holdCreateResponse(s: ReturnType<typeof servers>) {
  const post = s.b.network[`POST ${BASE}/v1/*`]!;
  s.b.network[`POST ${BASE}/v1/*`] = req => new URL(req.url).pathname === "/v1/spaces"
    ? { ...(post(req) as object), delay: 750 }
    : post(req);
}

function holdCreateCommit(s: ReturnType<typeof servers>) {
  const post = s.b.network[`POST ${BASE}/v1/*`]!;
  let commit: (() => void) | undefined;
  s.b.network[`POST ${BASE}/v1/*`] = req => {
    const response = post(req);
    if (new URL(req.url).pathname !== "/v1/spaces") return response;
    // Stage the fake's created row outside the visible membership map until
    // the test commits it, then let the delayed acknowledgement arrive.
    const space = createdSpace(s)!;
    s.b.state.spaces.delete(space.id);
    commit = () => { s.b.state.spaces.set(space.id, space); };
    return { ...(response as object), delay: 750 };
  };
  return { arrived: () => !!commit, release: () => commit!() };
}

function addPages(s: ReturnType<typeof servers>, roomLink = false) {
  const pages = pagesFixture();
  pages.state.linkedPage = TEAM;
  if (roomLink) pages.entries.set(TEAM, { ...pages.entries.get(TEAM)!, pinned: true });
  for (const method of ["GET", "POST"]) {
    const key = `${method} ${BASE}/v1/*`;
    const other = s.b.network[key]!;
    s.b.network[key] = req => {
      const path = new URL(req.url).pathname;
      if (roomLink && method === "POST" && path === `/v1/pages/${TEAM}/data`) {
        return { status: 200, body: { ...sampleData,
          actions: { ...sampleData.actions, room: { enabled: true, space_id: "acme", room_id: "lobby" } } } };
      }
      return path === "/v1/pages" || path.startsWith("/v1/pages/") ? pages.network[key]!(req) : other(req);
    };
  }
}

test("successful Create outvotes a pre-create membership omission even if its first room read fails", async () => {
  const s = servers();
  const get = s.b.network[`GET ${BASE}/v1/*`]!;
  let failRooms = true;
  s.b.network[`GET ${BASE}/v1/*`] = req => failRooms && new URL(req.url).pathname === `/v1/spaces/${createdSpace(s)?.id}/rooms`
    ? { status: 503, body: { error: "Try again later" }, delay: 40 }
    : get(req);
  const app = await launchInEmptyBeta(s);
  await startMembershipProbe(s, app, 650);
  s.b.answerLate("/v1/spaces", TOKENS.bob!, 0); // only hold the captured GET, not the later POST
  const space = await submitCreate(s, app);
  const roomsPath = `/v1/spaces/${space.id}/rooms`;
  await app.waitFor("id:rooms-error", { timeout: 150 });
  expect(app.texts().join(" ")).toContain(`Couldn't open ${createdName}:`);
  expect(app.shows(`id:space:${space.id}`)).toBe(true);
  await app.clock!.runFor(700);
  expect(app.shows(`id:space:${space.id}`)).toBe(true);
  expect(app.shows("id:rooms-empty")).toBe(true); // still in the original Beta
  expect(firstRoomPages(s)).toEqual([]);
  expect(chatStarted(s)).toBe(false);
  expect(remotePaths(s).filter(p => p === "/v1/spaces")).toHaveLength(3); // two GETs, one POST

  failRooms = false;
  await app.press(`id:space:${space.id}`);
  await app.waitUntil(() => firstRoomPages(s).length === 1, { timeout: 160 });
  expect(firstRoomPages(s)).toEqual([`${roomsPath}/lobby/events?after=0`]);
  expect(app.requests().filter(r => r.url === REMOTE + roomsPath)).toHaveLength(2);
});

for (const move of [false, true]) {
  test(move ? "moving to Library during Create revalidation wins even when the Space is verified"
    : "a newer membership snapshot taken before Create commits is rechecked before the Space appears", async () => {
    const s = servers();
    const create = holdCreateCommit(s);
    const app = await launchInEmptyBeta(s);
    await startCreate(app);
    await app.waitUntil(create.arrived, { timeout: 120 });
    await startMembershipProbe(s, app, 35);
    await app.clock!.runFor(65);
    expect(createdSpace(s)).toBeUndefined(); // the newer snapshot settled before the server committed

    create.release();
    const verification = s.b.answerLate("/v1/spaces", TOKENS.bob!, 180);
    await app.waitUntil(() => verification.arrived() === 1, { timeout: 850 });
    const space = createdSpace(s)!;
    expect(app.shows(`id:space:${space.id}`)).toBe(false);
    expect(firstRoomPages(s)).toEqual([]);
    if (move) await app.press("id:nav-library");
    await app.waitFor(`id:space:${space.id}`, { timeout: 250 });
    const roomsPath = `/v1/spaces/${space.id}/rooms`;
    if (move) {
      expect(app.shows("id:library-view")).toBe(true);
      expect(remotePaths(s)).not.toContain(roomsPath);
      expect(firstRoomPages(s)).toEqual([]);
      expect(remotePaths(s).some(p => p.startsWith("/v1/chat/") || p === "/v1/runs")).toBe(false);
    } else {
      await app.waitUntil(() => firstRoomPages(s).length === 1, { timeout: 150 });
      expect(firstRoomPages(s)).toEqual([`${roomsPath}/lobby/events?after=0`]);
      expect(app.requests().filter(r => r.url === REMOTE + roomsPath)).toHaveLength(1);
      expect(chatStarted(s)).toBe(false);
    }
  });
}

test("Create cannot resurrect a Space deleted before acknowledgement while the user is in Library", async () => {
  const s = servers();
  holdCreateResponse(s);
  const app = await launchInEmptyBeta(s);
  const space = await submitCreate(s, app);
  await app.press("id:nav-library");
  await app.waitFor("id:library-view", { timeout: 120 });
  s.b.state.spaces.delete(space.id); // another client deletes the Space after the POST committed
  await startMembershipProbe(s, app, 35);
  await app.clock!.runFor(65);

  const verification = s.b.answerLate("/v1/spaces", TOKENS.bob!, 180);
  await app.waitUntil(() => verification.arrived() === 1, { timeout: 850 });
  expect(app.shows(`id:space:${space.id}`)).toBe(false);
  await app.waitFor("id:new-space-name", { state: "gone", timeout: 250 });
  expect(app.shows(`id:space:${space.id}`)).toBe(false);
  expect(app.shows("id:library-view")).toBe(true);
  expect(remotePaths(s)).not.toContain(`/v1/spaces/${space.id}/rooms`);
  expect(firstRoomPages(s)).toEqual([]);
  expect(remotePaths(s).some(p => p.startsWith("/v1/chat/") || p === "/v1/runs")).toBe(false);
  expect(s.b.posts().filter(r => r.path === "/v1/spaces")).toHaveLength(1);
});

test("an inconclusive overlap check neither publishes the Space nor retries the accepted Create", async () => {
  const s = servers();
  const create = holdCreateCommit(s);
  const get = s.b.network[`GET ${BASE}/v1/*`]!;
  let checks = 0;
  let fail = false;
  s.b.network[`GET ${BASE}/v1/*`] = req => {
    if (!fail || new URL(req.url).pathname !== "/v1/spaces") return get(req);
    checks++;
    return { status: 503, body: { error: "Try again later" }, delay: 80 };
  };
  const app = await launchInEmptyBeta(s);
  await startCreate(app);
  await app.waitUntil(create.arrived, { timeout: 120 });
  await startMembershipProbe(s, app, 35);
  await app.clock!.runFor(65);
  fail = true;
  create.release();
  await app.waitFor("id:new-space-name", { state: "gone", timeout: 950 });
  const space = createdSpace(s)!;
  expect(checks).toBe(1);
  expect(app.shows(`id:space:${space.id}`)).toBe(false);
  expect(app.shows("id:rooms-empty")).toBe(true);
  expect(app.shows("id:rooms-error")).toBe(false);
  expect(remotePaths(s)).not.toContain(`/v1/spaces/${space.id}/rooms`);
  expect(firstRoomPages(s)).toEqual([]);
  expect(s.b.posts().filter(r => r.path === "/v1/spaces")).toHaveLength(1);
});

test("a delayed Create acknowledgement cannot restore a Space already lost to a newer exact denial", async () => {
  const s = servers();
  holdCreateResponse(s);
  const app = await launchInEmptyBeta(s);
  const space = await submitCreate(s, app);
  await startMembershipProbe(s, app, 40); // discover the new Space before the POST response
  await app.waitFor(`id:space:${space.id}`, { timeout: 130 });
  space.members.delete("bob");
  await app.press(`id:space:${space.id}`);
  await app.waitFor(`id:space:${space.id}`, { state: "gone", timeout: 140 });
  await app.clock!.runFor(800);
  expect(app.shows(`id:space:${space.id}`)).toBe(false);
  expect(app.shows("id:rooms-empty")).toBe(true);
  expect(app.requests().filter(r => r.url === `${REMOTE}/v1/spaces/${space.id}/rooms`)).toHaveLength(1);
  expect(firstRoomPages(s)).toEqual([]);
  expect(chatStarted(s)).toBe(false);
});

test("a delayed Create acknowledgement lists the Space without overriding a move to Library", async () => {
  const s = servers();
  holdCreateResponse(s);
  const app = await launchInEmptyBeta(s);
  const space = await submitCreate(s, app);
  await app.press("id:nav-library");
  await app.waitFor("id:library-view", { timeout: 120 });
  await app.waitFor(`id:space:${space.id}`, { timeout: 850 });
  expect(app.shows("id:library-view")).toBe(true);
  expect(remotePaths(s)).not.toContain(`/v1/spaces/${space.id}/rooms`);
  expect(firstRoomPages(s)).toEqual([]);
  expect(remotePaths(s).some(p => p.startsWith("/v1/chat/") || p === "/v1/runs")).toBe(false);
});

test("a delayed Create from a signed-out account doesn't enter or publish in the next session", async () => {
  const s = servers();
  holdCreateResponse(s);
  const app = await launchInEmptyBeta(s);
  const space = await submitCreate(s, app);
  await menuSwitch(app, "disconnect");
  await app.waitFor("id:saved-server:srv-b", { timeout: 120 });
  await app.press("id:saved-server:srv-b");
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:chat-view", { timeout: 150 });
  await app.clock!.runFor(800);
  expect(app.shows("id:chat-view")).toBe(true);
  expect(app.shows(`id:space:${space.id}`)).toBe(false);
  expect(remotePaths(s)).not.toContain(`/v1/spaces/${space.id}/rooms`);
  expect(firstRoomPages(s)).toEqual([]);
});

for (const source of ["POST", "revalidation"] as const) {
  for (const switchAccount of [false, true]) {
    test(`${source} Create 401 ${switchAccount ? "cannot sign out the next account" : "signs out the current account after a move to Library"}`, async () => {
      const s = servers();
      const staged = source === "revalidation" ? holdCreateCommit(s) : undefined;
      const method = staged ? "GET" : "POST";
      const key = `${method} ${BASE}/v1/*`;
      const route = s.b.network[key]!;
      let refuse = !staged, refusals = 0;
      s.b.network[key] = req => {
        if (refuse && new URL(req.url).pathname === "/v1/spaces" && header(req, "authorization") === `Bearer ${TOKENS.bob}`) {
          refusals++;
          return { status: 401, body: { error: "expired" }, delay: 450 };
        }
        return route(req);
      };
      const app = await launchInEmptyBeta(s);
      await startCreate(app);
      await app.waitUntil(staged?.arrived ?? (() => refusals === 1), { timeout: 120 });
      await app.press("id:nav-library");
      await app.waitFor("id:library-view", { timeout: 120 });
      if (staged) {
        await startMembershipProbe(s, app, 35);
        await app.clock!.runFor(65);
        refuse = true;
        staged.release();
        await app.waitUntil(() => refusals === 1, { timeout: 850 });
        expect(app.shows(`id:space:${createdSpace(s)!.id}`)).toBe(false);
      }

      if (switchAccount) {
        await menuSwitch(app, "disconnect");
        await app.waitFor("id:saved-server:srv-b", { timeout: 120 });
        await app.press("id:saved-server:srv-b");
        await app.type("id:access-token", TOKENS.alice!);
        await app.press("id:connect");
        await app.waitFor("id:chat-view", { timeout: 150 });
        await app.clock!.runFor(500);
        expect(app.shows("id:chat-view")).toBe(true);
        expect(app.shows("id:connect-error")).toBe(false);
        const space = createdSpace(s);
        if (space) expect(app.shows(`id:space:${space.id}`)).toBe(false);
      } else {
        expect(app.shows("id:library-view")).toBe(true);
        await app.waitFor("id:connect-error", { timeout: 500 });
        expect(app.texts().join(" ")).toContain("The server rejected the access token (HTTP 401).");
        expect(app.shows("id:workspace")).toBe(false);
      }
      expect(refusals).toBe(1);
      expect(app.requests().filter(r => r.method === "POST" && r.url === REMOTE + "/v1/spaces")).toHaveLength(1);
      expect(firstRoomPages(s)).toEqual([]);
    });
  }
}

for (const returnToOriginal of [false, true]) {
  test(`Create does not override a real same-place Page link${returnToOriginal ? " and return to the original Page" : ""}`, async () => {
    const s = servers();
    addPages(s);
    holdCreateResponse(s);
    const app = await launchInEmptyBeta(s);
    await app.press("id:pinned-page:pg_bob");
    await app.waitFor("text:Bob’s dashboard", { timeout: 130 });
    await app.waitFor("id:page-action:other", { timeout: 150 });
    const space = await submitCreate(s, app);
    await app.press("id:page-action:other");
    await app.waitFor("text:Team dashboard", { timeout: 130 });
    if (returnToOriginal) {
      await app.press("id:pinned-page:pg_bob");
      await app.waitFor("text:Bob’s dashboard", { timeout: 130 });
    }
    await app.waitFor(`id:space:${space.id}`, { timeout: 850 });
    expect(app.shows("id:page-view")).toBe(true);
    expect(app.shows(returnToOriginal ? "text:Bob’s dashboard" : "text:Team dashboard")).toBe(true);
    expect(remotePaths(s)).not.toContain(`/v1/spaces/${space.id}/rooms`);
    expect(firstRoomPages(s)).toEqual([]);
    expect(chatStarted(s)).toBe(false);
  });
}

test("a verified Page room link can open before superseded startup hydration replies", async () => {
  const s = servers();
  addPages(s, true);
  const old = s.b.answerLate(betaRooms, TOKENS.bob!, 900);
  const app = await launch(s, { storage: rememberedRemote, manualClock: true });
  await submitSavedStartup(app);
  await app.waitFor("id:rooms-loading", { timeout: 120 });
  expect(old.arrived()).toBe(1);
  await app.press(`id:pinned-page:${TEAM}`);
  await app.waitFor("id:page-action:room", { timeout: 160 });
  await app.press("id:page-action:room");
  await app.waitFor("text:hello from bob in acme", { timeout: 170 });
  await app.clock!.runFor(950);
  expect(app.shows("text:hello from bob in acme")).toBe(true);
  expect(old.arrived()).toBe(1);
  expect(firstRoomPages(s)).toEqual([firstAcme]);
  expect(chatStarted(s)).toBe(false);
});

test("a newer complete list can omit a Space that an older list discovered midflight", async () => {
  const s = servers();
  const app = await launchWithoutBeta(s);
  const members = s.b.state.spaces.get("beta")!.members;
  members.add("bob");
  await startMembershipProbe(s, app, 250);
  members.delete("bob");
  await startMembershipProbe(s, app, 650);
  await app.waitFor("id:space:beta", { timeout: 300 });
  await app.waitFor("id:space:beta", { state: "gone", timeout: 500 });
  expect(remotePaths(s).filter(p => p === "/v1/spaces")).toHaveLength(3);
  expect(remotePaths(s)).not.toContain(betaRooms);
  expect(firstRoomPages(s)).toEqual([]);
});

test("a late older complete list cannot discover a Space omitted by an already settled newer list", async () => {
  const s = servers();
  const app = await launchWithoutBeta(s);
  const members = s.b.state.spaces.get("beta")!.members;
  members.add("bob");
  await startMembershipProbe(s, app, 650);
  members.delete("bob");
  await startMembershipProbe(s, app, 150);
  await app.clock!.runFor(750);
  expect(app.shows("id:space:beta")).toBe(false);
  expect(remotePaths(s).filter(p => p === "/v1/spaces")).toHaveLength(3);
  expect(firstRoomPages(s)).toEqual([]);
});

test("a list started before discovery can't resurrect a Space after its newer exact denial", async () => {
  const s = servers();
  const app = await launchWithoutBeta(s);
  const members = s.b.state.spaces.get("beta")!.members;
  members.add("bob");
  await startMembershipProbe(s, app, 250);
  await startMembershipProbe(s, app, 750); // also captured Beta, but the client doesn't know it yet
  await app.waitFor("id:space:beta", { timeout: 300 });
  members.delete("bob");
  await app.press("id:space:beta");
  await app.waitFor("id:space:beta", { state: "gone", timeout: 150 });
  await app.clock!.runFor(600);
  expect(app.shows("id:space:beta")).toBe(false);
  expect(firstRoomPages(s)).toEqual([]);
  expect(app.requests().filter(r => r.url === REMOTE + betaRooms)).toHaveLength(1);

  members.add("bob");
  await startMembershipProbe(s, app, 30);
  await app.waitFor("id:space:beta", { timeout: 150 }); // a real newer rejoin is allowed
});

test("a late background Space denial doesn't cancel an unrelated personal Chat file pick", async () => {
  const s = servers();
  holdBetaDenial(s, 404);
  const app = await launch(s, { storage: rememberedRemote, manualClock: true, probe: true });
  await submitSavedStartup(app);
  await app.waitFor("id:rooms-loading", { timeout: 120 });
  await app.press("id:nav-chat");
  await app.waitFor("id:chat-view", { timeout: 120 });
  await app.press("id:probe-hold-pick");
  await app.waitFor("id:space:beta", { state: "gone", timeout: 700 });
  await app.press("id:probe-finish-pick");
  await app.waitFor("id:staged:personal.txt", { timeout: 150 });
  expect(app.shows("id:chat-view")).toBe(true);
  expect(firstRoomPages(s)).toEqual([]);
  expect(app.requests().filter(r => r.method === "POST" && r.url.includes("/v1/library/assets"))).toHaveLength(0);
});

test("a held Dispatch read can't replace a newer room choice in the same Space", async () => {
  const s = servers();
  s.b.addRoom("beta", { id: "other", name: "other" });
  const app = await launch(s, { storage: { ...rememberedRemote, "metro.lastSpace": "" }, manualClock: true });
  await submitSavedStartup(app);
  await app.waitFor("id:chat-empty", { timeout: 120 });
  await app.press("id:dispatch-new");
  const old = s.b.answerLate(betaRooms, TOKENS.bob!, 650);
  await app.press("id:dispatch-space:beta");
  await app.waitUntil(() => old.arrived() === 1, { timeout: 100 });
  s.b.answerLate(betaRooms, TOKENS.bob!, 30);
  await app.press("id:dispatch-space:beta");
  await app.waitFor("id:dispatch-room:other", { timeout: 140 });
  await app.press("id:dispatch-room:other");
  await app.clock!.runFor(700);
  expect(app.texts()).toContain("To: agent in Beta › #other");
  expect(app.shows("id:dispatch-error")).toBe(false);
  expect(firstRoomPages(s)).toEqual([]);
});

test("a room-list failure stays visible when returning to a settled empty Space", async () => {
  const s = servers();
  s.b.state.rooms.set("beta", []);
  const get = s.b.network[`GET ${BASE}/v1/*`]!;
  s.b.network[`GET ${BASE}/v1/*`] = req => new URL(req.url).pathname === acmeRooms
    ? { status: 503, body: { error: "Try again later" }, delay: 90 }
    : get(req);
  const app = await launch(s, { storage: rememberedRemote, manualClock: true });
  await submitSavedStartup(app);
  await app.waitFor("id:rooms-empty", { timeout: 100 });
  await app.press("id:space:acme");
  await app.waitFor("id:rooms-loading", { timeout: 50 });
  expect(app.shows("text:No room selected")).toBe(false);
  await app.waitFor("id:rooms-error", { timeout: 180 });
  expect(app.texts().join(" ")).toContain("Couldn't open Acme");
  expect(app.shows("id:rooms-empty")).toBe(true); // Beta, not the failed Acme read
  expect(app.shows("id:rooms-loading")).toBe(false);
  expect(app.shows("id:space:acme")).toBe(true);
  expect(firstRoomPages(s)).toEqual([]);
  expect(chatStarted(s)).toBe(false);
});

test("signing out during remote hydration aborts the old room and never saves its token", async () => {
  const s = servers();
  const rooms = s.b.answerLate(betaRooms, TOKENS.bob!, 500);
  const app = await launch(s, { storage: rememberedRemote, manualClock: true });
  await app.waitFor("text:Sign in to Team B");
  await app.press("id:keep-signed-in");
  await submitSavedStartup(app);
  await app.waitFor("id:rooms-loading", { timeout: 100 });
  expect(rooms.arrived()).toBe(1);
  await menuSwitch(app, "disconnect");
  await app.waitFor("id:connect-screen", { timeout: 100 });
  await app.clock!.runFor(550);
  expect(app.shows("id:workspace")).toBe(false);
  expect(remotePaths(s).some(p => p.includes("/events"))).toBe(false);
  await app.press("id:saved-server:srv-b");
  expect(app.value("id:access-token")).toBe("");
  expect(remotePaths(s).filter(p => p === "/v1/me")).toHaveLength(1);
});

test("manual sign-in wins before the shared startup status resolves", async () => {
  const s = servers(), bridge = launcher({ statusDelay: 400 });
  const app = await launch(s, { bridge, manualClock: true, storage: {
    "metro.savedServers": choices, "metro.serverUrl": REMOTE,
  } });
  await app.press("id:startup-choose-manually");
  await app.waitFor("id:server-url");
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  expect(s.a.state.log).toHaveLength(0);
  expect(s.b.state.log).toHaveLength(0);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:space:acme", { timeout: 120 });
  await app.clock!.runFor(450);
  expect(bridge.statuses()).toBe(1);
  expect(bridge.opens()).toBe(0);
  expect(remotePaths(s)).toEqual([]);
  expect(s.local.state.log).toHaveLength(0);
  expect(app.shows("id:space:acme")).toBe(true);
});

async function pickStartup(app: App, id: string) {
  await menuSwitch(app, "startup-server");
  await app.press(`id:startup-server:${id}`);
  await app.waitFor("id:workspace-dialog", { state: "gone" });
}

test("set startup without switching; a fresh launch signs in only to that server, and a reset opens local", async () => {
  const s = servers();
  const app = await launch(s, { bridge: launcher(), storage: { "metro.savedServers": choices } });
  await app.waitFor("id:space:solo");
  await pickStartup(app, "srv-b");
  expect(app.shows("id:space:solo")).toBe(true);
  expect(s.a.state.log).toHaveLength(0);
  expect(s.b.state.log).toHaveLength(0);

  const fresh = servers(), bridge = launcher();
  const boot = await launch(fresh, { bridge, storage: { ...app.storage(), "metro.serverUrl": BASE } });
  await boot.waitFor("text:Sign in to Team B");
  expect(boot.value("id:server-url")).toBe(REMOTE);
  expect(boot.value("id:access-token")).toBe("");
  expect(fresh.b.state.log).toHaveLength(0);
  await boot.type("id:access-token", "wrong-for-b");
  await boot.press("id:connect");
  await boot.waitFor("id:connect-error");
  expect(boot.shows("text:Sign in to Team B")).toBe(true);
  expect(boot.value("id:access-token")).toBe("");
  expect(bridge.opens()).toBe(0);
  await boot.type("id:access-token", TOKENS.bob!);
  await boot.press("id:connect");
  await boot.waitFor("id:space:beta");
  expect(fresh.a.state.log).toHaveLength(0);
  noTokensStored(boot);
  await pickStartup(boot, "local");
  expect(boot.shows("id:space:beta")).toBe(true);
  const last = servers();
  const again = await launch(last, { bridge: launcher(), storage: { ...boot.storage() } });
  await again.waitFor("id:space:solo");
  expect(last.b.state.log).toHaveLength(0);
});

test("the startup server survives renaming and ordinary switches; forgetting it restores local", async () => {
  const app = await launch(servers(), { bridge: launcher(), storage: { "metro.savedServers": choices } });
  await app.waitFor("id:space:solo");
  await pickStartup(app, "srv-b");
  await menuSwitch(app, "manage-servers");
  await app.clear("id:rename-server:srv-b");
  await app.type("id:rename-server:srv-b", "Renamed");
  await app.press("id:rename-server-save:srv-b");
  await app.waitUntil(() => saved(app).some(s => s.name === "Renamed"));
  await app.press("id:manage-servers-done");
  await menuSwitch(app, "workspace-server:srv-a");
  await app.type("id:workspace-token", TOKENS.alice!);
  await app.press("id:workspace-sign-in");
  await app.waitFor("id:space:acme");
  const renamed = await launch(servers(), { bridge: launcher(), storage: { ...app.storage() } });
  await renamed.waitFor("text:Sign in to Renamed");
  expect(renamed.value("id:server-url")).toBe(REMOTE);
  await menuSwitch(app, "manage-servers");
  await app.press("id:forget-server:srv-b");
  await app.waitUntil(() => !saved(app).some(s => s.id === "srv-b"));
  const restarted = servers();
  const again = await launch(restarted, { bridge: launcher(), storage: { ...app.storage() } });
  await again.waitFor("id:space:solo");
  expect(restarted.a.state.log).toHaveLength(0);
  expect(restarted.b.state.log).toHaveLength(0);
});

test("saved servers can be renamed and forgotten", async () => {
  const s = servers();
  const app = await launch(s);
  await signIn(app, "alice", BASE, { keep: true });
  await addServer(app, REMOTE, TOKENS.bob!);
  await app.waitFor("id:space:beta");
  const b = idOf(app, REMOTE);
  await menuSwitch(app, "manage-servers");
  await app.clear(`id:rename-server:${b}`);
  await app.type(`id:rename-server:${b}`, "Beta");
  await app.press(`id:rename-server-save:${b}`);
  await app.waitUntil(() => saved(app).find((x) => x.id === b)?.name === "Beta");
  const a = idOf(app, BASE);
  await app.press(`id:forget-server:${a}`);
  await app.waitUntil(() => saved(app).length === 1);
  await app.press("id:manage-servers-done");
  // A was forgotten, so its token went with it: coming back asks again.
  await menuSwitch(app, "disconnect");
  await app.waitFor("id:connect-screen");
  expect(app.shows(`id:saved-server:${a}`)).toBe(false);
  expect(app.shows(`id:saved-server:${b}`)).toBe(true);
});

test("without asking to stay signed in, switching back asks for the token; a refused one changes nothing", async () => {
  const s = servers();
  const app = await launch(s);
  await signIn(app, "alice"); // saved, token not kept
  await addServer(app, REMOTE, TOKENS.bob!);
  await app.waitFor("id:space:beta");
  await menuSwitch(app, `workspace-server:${idOf(app, BASE)}`);
  await app.waitFor("id:workspace-token");
  await app.type("id:workspace-token", "tok-wrong");
  await app.press("id:workspace-sign-in");
  await app.waitFor("id:workspace-error");
  expect(app.shows("id:space:beta")).toBe(true); // still on B, as bob
  await app.clear("id:workspace-token");
  await app.type("id:workspace-token", TOKENS.alice!);
  await app.press("id:workspace-sign-in");
  await app.waitUntil(() => !app.shows("id:space:beta"));
  await app.waitFor("id:space:acme");
  expect(onlyTokenFor(app, REMOTE, "bob")).toBeGreaterThan(0);
  // The wrong token went only to A, and A's real one only to A.
  const toA = app.requests().filter((r: any) => r.url.startsWith(BASE)) as any[];
  const auths = new Set(toA.map((r) => Object.entries(r.headers ?? {}).find(([k]) => k.toLowerCase() === "authorization")?.[1]));
  expect([...auths].sort()).toEqual(["Bearer tok-alice", "Bearer tok-wrong"]);
  noTokensStored(app);
});
