// "Open local workspace" on the connect screen, headless: a fake launcher
// bridge (contract v1 plus POST /v1/local-workspace/open) and two separate
// in-memory servers, one playing the private local server and one a remote
// server. Fake tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, fakeServer, header, TOKENS, world } from "../test/fakeServer";

const BRIDGE = "http://127.0.0.1:47123";
const BRIDGE_TOKEN = "cd".repeat(32);
const LOCAL = "http://127.0.0.1:41234";
/** The local server's session bearer (the fake server knows carol's token). */
const LOCAL_TOKEN = TOKENS.carol!;

type F = ReturnType<typeof fakeServer>;
type Req = { method: string; url: string; headers: Record<string, string>; body?: string | null };

function fakeLauncher(opts: { local?: string; open?: (req: Req) => unknown } = {}) {
  const state = { log: [] as { method: string; path: string; body: any; auth: string | undefined }[], local: opts.local ?? "stopped" };
  const ok = (body: unknown) => ({ status: 200, headers: { "cache-control": "no-store" }, body });
  const err = (status: number, code: string, message: string) => ({ status, body: { error: { code, message } } });
  const status = () => ({ bridge: 1, version: "test", instance: "primary", kinds: [], session: null, connections: [], local: state.local });
  const handle = (req: Req) => {
    const path = new URL(req.url).pathname;
    const body = req.body ? JSON.parse(req.body) : null;
    const auth = header(req as any, "authorization");
    state.log.push({ method: req.method, path, body, auth });
    if (auth !== `Bearer ${BRIDGE_TOKEN}`) return err(401, "unauthorized", "Unauthorized.");
    if (path === "/v1/status") return ok(status());
    if (path === "/v1/session") return ok(status());
    if (path === "/v1/local-workspace/open" && req.method === "POST") {
      if (opts.open) return opts.open(req);
      state.local = "running";
      return ok({ server_url: LOCAL, token: LOCAL_TOKEN });
    }
    return err(404, "not_found", "Unknown path.");
  };
  const network: Record<string, (req: Req) => unknown> = {};
  for (const m of ["GET", "POST", "PUT", "DELETE"]) network[`${m} ${BRIDGE}/v1/*`] = handle;
  return { state, network, opens: () => state.log.filter((l) => l.path === "/v1/local-workspace/open") };
}

/** Serves `server` at LOCAL: a separate server with its own accounts and data. */
function atLocal(server: F) {
  const network: Record<string, (req: Req) => unknown> = {};
  for (const m of ["GET", "POST", "PUT", "PATCH", "DELETE"]) {
    const inner = (server.network as any)[`${m} ${BASE}/v1/*`];
    network[`${m} ${LOCAL}/v1/*`] = (req: Req) => inner({ ...req, url: req.url.replace(LOCAL, BASE) });
  }
  return network;
}

const env = { process: { "default.env": { METRO_DESKTOP_BRIDGE_URL: BRIDGE, METRO_DESKTOP_BRIDGE_TOKEN: BRIDGE_TOKEN } } };

async function launch(remote: F, local: F, bridge: ReturnType<typeof fakeLauncher> | null, storage: Record<string, string> = {}, manualClock = false) {
  const network = { ...(remote.network as any), ...atLocal(local), ...(bridge ? bridge.network : {}) };
  return open("../App.tsx", { network, platform: "linux", width: 1200, height: 800, storage, ...(bridge ? { modules: env as any } : {}), ...(manualClock ? { clock: {} } : {}) });
}

const text = (app: App) => app.texts().join(" | ");
const lastLocalSpace = { "metro.lastSpace": JSON.stringify({ server: LOCAL, space: "solo" }) };
const localRooms = "/v1/spaces/solo/rooms";
const lobbyEvents = `${localRooms}/lobby/events`;
const roomEventPaths = (server: F) => server.state.log
  .map((r) => r.path)
  .filter((path) => path.startsWith("/v1/spaces/") && path.includes("/events?"));

function expectNoTokens(app: App) {
  const shown = text(app);
  const stored = JSON.stringify(app.storage());
  for (const s of [LOCAL_TOKEN, BRIDGE_TOKEN]) {
    expect(shown).not.toContain(s);
    expect(stored).not.toContain(s);
  }
  expect(app.clipboard() ?? "").not.toContain(LOCAL_TOKEN);
}

test("without the packaged launcher there is no local option, and connecting works as before", async () => {
  const remote = world();
  const app = await launch(remote, fakeServer(), null);
  expect(app.shows("id:connect-screen")).toBe(true);
  expect(app.shows("id:local-workspace")).toBe(false);
  expect(app.shows("id:open-local")).toBe(false);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:workspace");
});

test("a new install opens local Chat without a click and leaves remote settings untouched", async () => {
  const remote = world();
  const local = fakeServer();
  local.addSpace("solo", "Solo", "carol");
  const bridge = fakeLauncher();
  const app = await launch(remote, local, bridge, { "metro.serverUrl": "https://metro.example.com" });
  await app.waitFor("id:workspace");
  await app.waitFor("id:space:solo");

  // The bridge was asked with its bearer; the local server got the local bearer.
  expect(bridge.opens()).toHaveLength(1);
  expect(bridge.opens()[0]!.auth).toBe(`Bearer ${BRIDGE_TOKEN}`);
  const toLocal = app.requests().filter((r: any) => r.url.startsWith(LOCAL));
  expect(toLocal.length).toBeGreaterThan(0);
  for (const r of toLocal) expect(header(r as any, "authorization")).toBe(`Bearer ${LOCAL_TOKEN}`);
  // Nothing went to the remote server, and its URL is still the remembered one.
  expect(remote.state.log).toHaveLength(0);
  expect(app.requests().some((r: any) => r.url.startsWith("https://metro.example.com"))).toBe(false);
  expect(app.storage()["metro.serverUrl"]).toBe("https://metro.example.com");
  expectNoTokens(app);

  // Leaving it (Sign out, in the account menu) shows the connect screen with the remote server as it was.
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  expect(app.value("id:server-url")).toBe("https://metro.example.com");
  expect(bridge.opens()).toHaveLength(1);
  expectNoTokens(app);
});

test("a slow remembered room starts one initial transcript request before the normal poll", async () => {
  const local = fakeServer();
  local.addSpace("solo", "Solo", "carol");
  const message = local.post("solo/lobby", { kind: "message", actor: "carol", text: "First local message" });
  const rooms = local.answerLate(localRooms, LOCAL_TOKEN, 80);
  local.answerLate(lobbyEvents, LOCAL_TOKEN, 300);
  const app = await launch(world(), local, fakeLauncher(), lastLocalSpace, true);
  await app.waitFor("id:workspace", { timeout: 50 });
  expect(rooms.arrived()).toBe(1);
  expect(roomEventPaths(local)).toEqual([]);

  await app.waitUntil(() => roomEventPaths(local).length > 0, { timeout: 150 });
  await app.clock!.runFor(150);
  expect(roomEventPaths(local)).toEqual([`${lobbyEvents}?after=0`]);
  expect(app.shows("text:First local message")).toBe(false);
  await app.waitFor("text:First local message", { timeout: 300 });
  await app.waitUntil(() => roomEventPaths(local).length >= 2, { timeout: 150 });
  // The intentional catch-up page advances the cursor, never re-fetching page one.
  expect(roomEventPaths(local)).toEqual([`${lobbyEvents}?after=0`, `${lobbyEvents}?after=${message.id}`]);
});

test("a remembered Space with no rooms makes no transcript request", async () => {
  const local = fakeServer();
  local.addSpace("solo", "Solo", "carol");
  local.state.rooms.set("solo", []);
  local.answerLate(localRooms, LOCAL_TOKEN, 80);
  const app = await launch(world(), local, fakeLauncher(), lastLocalSpace, true);
  await app.waitFor("id:workspace");
  await app.clock!.runFor(1700);
  expect(app.shows("id:space:solo")).toBe(true);
  expect(app.shows("text:No room selected")).toBe(true);
  expect(roomEventPaths(local)).toEqual([]);
});

test("membership refused by the rooms endpoint never starts that Space's transcript", async () => {
  const local = fakeServer();
  local.addSpace("solo", "Solo", "alice", ["carol"]);
  const get = local.network[`GET ${BASE}/v1/*`]!;
  local.network[`GET ${BASE}/v1/*`] = (req) => {
    // Membership was present in /spaces but revoked by the next request.
    if (new URL(req.url).pathname === localRooms) local.state.spaces.get("solo")!.members.delete("carol");
    return get(req);
  };
  local.answerLate(localRooms, LOCAL_TOKEN, 80);
  const app = await launch(world(), local, fakeLauncher(), lastLocalSpace, true);
  await app.waitUntil(() => text(app).includes("Couldn't open Solo"), { timeout: 200 });
  expect(app.shows("id:space:solo")).toBe(false);
  expect(roomEventPaths(local)).toEqual([]);
  await app.waitUntil(() => local.state.log.some((r) => r.path === "/v1/chat/events?after=0"), { timeout: 200 });
});

test("switching accounts during remembered room load cannot start the old transcript", async () => {
  const local = fakeServer();
  local.addSpace("solo", "Solo", "carol");
  const oldRooms = local.answerLate(localRooms, LOCAL_TOKEN, 1000);
  const remote = world();
  const app = await launch(remote, local, fakeLauncher(), lastLocalSpace, true);
  await app.waitUntil(() => oldRooms.arrived() === 1, { timeout: 200 });
  await app.press("id:account-menu");
  await app.press("id:add-server");
  await app.type("id:add-server-url", BASE);
  await app.type("id:add-server-token", TOKENS.alice!);
  await app.press("id:add-server-submit");
  await app.waitFor("id:space:acme", { timeout: 200 });
  await app.press("id:space:acme");
  await app.waitFor("text:hello from bob in acme", { timeout: 200 });
  await app.clock!.runFor(1100);
  expect(app.shows("text:hello from bob in acme")).toBe(true);
  expect(app.shows("id:space:solo")).toBe(false);
  expect(roomEventPaths(local)).toEqual([]);
  expect(roomEventPaths(remote).filter((path) => path.endsWith("?after=0"))).toEqual([
    "/v1/spaces/acme/rooms/lobby/events?after=0",
  ]);
});

for (const openExisting of [false, true]) {
  test(`${openExisting ? "opening another Space" : "creating a Space"} during remembered room load does not duplicate transcript requests`, async () => {
    const local = fakeServer();
    local.addSpace("solo", "Solo", "carol");
    local.addSpace("other", "Other", "carol");
    const oldRooms = local.answerLate(localRooms, LOCAL_TOKEN, 700);
    const app = await launch(world(), local, fakeLauncher(), lastLocalSpace, true);
    await app.waitUntil(() => oldRooms.arrived() === 1, { timeout: 200 });

    await app.press("id:new-space-toggle");
    await app.type("id:new-space-name", "New Space");
    await app.press("id:create-space");
    await app.waitUntil(() => roomEventPaths(local).length > 0, { timeout: 200 });
    const created = [...local.state.spaces.values()].find((space) => space.name === "New Space")!;
    const expected = [`/v1/spaces/${created.id}/rooms/lobby/events?after=0`];

    if (openExisting) {
      // Ordinary Space selection is blocked while a selection is pending.
      // Creating a Space lets us move again before the original reply arrives.
      await app.press("id:space:other");
      expected.push("/v1/spaces/other/rooms/lobby/events?after=0");
      await app.waitUntil(() => roomEventPaths(local).length >= 2, { timeout: 200 });
    }
    expect(roomEventPaths(local)).toEqual(expected);

    // Let the remembered Space's reply settle, but not the normal 1.5 s poll.
    await app.clock!.runFor(750);
    expect(roomEventPaths(local)).toEqual(expected);
  });
}

test("an in-session switch to a remembered empty transcript requests it only once before the next poll", async () => {
  const remote = fakeServer();
  remote.addSpace("target", "Target", "alice");
  const rooms = remote.answerLate("/v1/spaces/target/rooms", TOKENS.alice!, 80);
  const app = await launch(remote, fakeServer(), fakeLauncher(), {
    "metro.lastSpace": JSON.stringify({ server: BASE, space: "target" }),
  }, true);
  await app.waitFor("id:workspace");
  await app.press("id:account-menu");
  await app.press("id:add-server");
  await app.type("id:add-server-url", BASE);
  await app.type("id:add-server-token", TOKENS.alice!);
  await app.press("id:add-server-submit");
  await app.waitUntil(() => rooms.arrived() === 1, { timeout: 200 });
  expect(roomEventPaths(remote)).toEqual([]);
  await app.waitUntil(() => roomEventPaths(remote).length > 0, { timeout: 150 });
  await app.clock!.runFor(400);
  expect(roomEventPaths(remote)).toEqual(["/v1/spaces/target/rooms/lobby/events?after=0"]);
});

test("legacy last-used markers never override the factory startup preference", async () => {
  const local = fakeServer();
  const bridge = fakeLauncher();
  const app = await launch(world(), local, bridge, { "metro.workspace": "local" });
  await app.waitFor("id:workspace");
  expect(bridge.opens()).toHaveLength(1);
  expectNoTokens(app);

  const other = fakeLauncher();
  const app2 = await launch(world(), fakeServer(), other, { "metro.workspace": "remote" });
  await app2.waitFor("id:workspace");
  expect(other.opens()).toHaveLength(1);
});

test("a failure shows the helper's own message and never falls back to a remote server", async () => {
  const remote = world();
  const message = "Your local workspace uses port 41234 on this computer, and another program is using it now. Close that program, then try again.";
  const bridge = fakeLauncher({ open: () => ({ status: 502, body: { error: { code: "local_failed", message } } }) });
  const app = await launch(remote, fakeServer(), bridge, { "metro.workspace": "local", "metro.serverUrl": BASE });
  await app.waitFor("id:local-error");
  expect(text(app)).toContain(message);
  expect(app.shows("id:workspace")).toBe(false);
  expect(remote.state.log).toHaveLength(0);
  expect(app.storage()["metro.workspace"]).toBe("local");
  // No retry on its own: trying again is the user's choice.
  await app.wait(1000);
  expect(bridge.opens()).toHaveLength(1);
  await app.press("id:open-local");
  await app.waitUntil(() => bridge.opens().length === 2);
  await app.waitFor("id:local-error");
  expect(app.shows("id:workspace")).toBe(false);
});

test("an install without a bundled server says so and offers nothing to press", async () => {
  const bridge = fakeLauncher({ local: "unavailable" });
  const app = await launch(world(), fakeServer(), bridge, { "metro.workspace": "local" });
  await app.waitFor("id:local-unavailable");
  expect(bridge.opens()).toHaveLength(0);
  expect(app.enabled("id:open-local")).toBe(false);
  expect(app.shows("id:workspace")).toBe(false);
});
