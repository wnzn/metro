// Saved servers and switching workspaces from the account menu, headless:
// two separate in-memory servers (A at the default loopback address, B at an
// https address), and a fake launcher hosting "On this computer". Fake
// tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, header, TOKENS, world } from "../test/fakeServer";
import { LAUNCHED_ENV, launched } from "../test/launched";

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
function launcher(opts: { refuse?: boolean } = {}) {
  const log: string[] = [];
  const ok = (body: unknown) => ({ status: 200, headers: { "cache-control": "no-store" }, body });
  const status = () => ({ bridge: 1, version: "test", instance: "primary", kinds: [], session: null, connections: [], local: "stopped" });
  const handle = (req: Req) => {
    const path = new URL(req.url).pathname;
    log.push(`${req.method} ${path}`);
    if (header(req as any, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return { status: 401, body: { error: { code: "unauthorized", message: "Unauthorized." } } };
    if (path === "/v1/status" || path === "/v1/session") return ok(status());
    if (path === "/v1/local-workspace/open") {
      if (opts.refuse) return { status: 503, body: { error: { code: "local_failed", message: "The local server didn't start." } } };
      return ok({ server_url: LOCAL, token: TOKENS.carol });
    }
    return { status: 404, body: { error: { code: "not_found", message: "Unknown path." } } };
  };
  const network: Record<string, (req: Req) => unknown> = {};
  for (const m of ["GET", "POST", "PUT", "DELETE"]) network[`${m} ${BRIDGE}/v1/*`] = handle;
  return { network, opens: () => log.filter((l) => l === "POST /v1/local-workspace/open").length };
}

function servers() {
  const a = world();
  const b = world();
  b.addSpace("beta", "Beta", "bob");
  const local = world();
  local.addSpace("solo", "Solo", "carol");
  return { a, b, local };
}

async function launch(s: ReturnType<typeof servers>, opts: { storage?: Record<string, string>; bridge?: ReturnType<typeof launcher>; manualClock?: boolean } = {}): Promise<App> {
  const network = { ...(s.a.network as any), ...at(REMOTE, s.b), ...at(LOCAL, s.local), ...(opts.bridge ? opts.bridge.network : {}) };
  return open("../App.tsx", { network, platform: "linux", width: 1200, height: 820,
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
const remotePaths = (s: ReturnType<typeof servers>) => s.b.state.log.map(r => r.path);

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
  s.b.answerLate("/v1/models/available?space_id=beta", TOKENS.bob!, 900);
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
  expect(rooms.arrived()).toBe(1);
  expect(remotePaths(s).some(p => p.includes("/events") || p.startsWith("/v1/models/available"))).toBe(false);
  await app.waitFor("text:First remote room message", { timeout: 340 });
  await app.waitUntil(() => remotePaths(s).includes(`${betaEvents}?after=${message.id}`), { timeout: 100 });
  const paths = remotePaths(s);
  for (const path of ["/v1/me", "/v1/spaces", betaRooms]) expect(paths.filter(p => p === path)).toHaveLength(1);
  expect(paths.filter(p => p.includes("/events?"))).toEqual([`${betaEvents}?after=0`, `${betaEvents}?after=${message.id}`]);
  expect(paths.some(p => p.startsWith("/v1/chat/") || p === "/v1/runs" || p === "/v1/models/available" || p === "/v1/providers")).toBe(false);
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
