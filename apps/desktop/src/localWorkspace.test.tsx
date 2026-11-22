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

async function launch(remote: F, local: F, bridge: ReturnType<typeof fakeLauncher> | null, storage: Record<string, string> = {}) {
  const network = { ...(remote.network as any), ...atLocal(local), ...(bridge ? bridge.network : {}) };
  return open("../App.tsx", { network, platform: "linux", width: 1200, height: 800, storage, ...(bridge ? { modules: env as any } : {}) });
}

const text = (app: App) => app.texts().join(" | ");

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

test("one press opens the local workspace: no URL, no token, and the remote settings are untouched", async () => {
  const remote = world();
  const local = fakeServer();
  local.addSpace("solo", "Solo", "carol");
  const bridge = fakeLauncher();
  const app = await launch(remote, local, bridge, { "metro.serverUrl": "https://metro.example.com" });
  await app.waitFor("id:open-local");
  expect(app.shows("id:local-workspace")).toBe(true);
  // The remote form is still there, as its own explicit choice.
  expect(app.shows("id:server-url")).toBe(true);

  await app.press("id:open-local");
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
  await app.waitUntil(() => app.storage()["metro.workspace"] === "local");
  expect(app.storage()["metro.serverUrl"]).toBe("https://metro.example.com");
  expectNoTokens(app);

  // Leaving it shows the connect screen with the remote server as it was.
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  expect(app.value("id:server-url")).toBe("https://metro.example.com");
  expectNoTokens(app);
});

test("the last choice is restored at launch: local reopens, remote waits for the user", async () => {
  const local = fakeServer();
  const bridge = fakeLauncher();
  const app = await launch(world(), local, bridge, { "metro.workspace": "local" });
  await app.waitFor("id:workspace");
  expect(bridge.opens()).toHaveLength(1);
  expectNoTokens(app);

  const other = fakeLauncher();
  const app2 = await launch(world(), fakeServer(), other, { "metro.workspace": "remote" });
  await app2.waitFor("id:open-local");
  expect(app2.shows("id:workspace")).toBe(false);
  expect(other.opens()).toHaveLength(0);
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
