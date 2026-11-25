// Local workspace tools in Manage Space, against the fake server
// (test/fakeServer.ts) and the fake desktop bridge, extended with the
// `/v1/local-tools…` routes the launcher forwards to desktop_tools. Fake
// tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, header, TOKENS, world } from "../../../test/fakeServer";
import { BRIDGE, BRIDGE_TOKEN, bridgeEnv, fakeBridge } from "../../device/test/fakeBridge";
import { localToolsBadge } from "./LocalToolsCard";
import { launched } from "../../../test/launched";

type F = ReturnType<typeof world>;
type Req = { method: string; url: string; headers: Record<string, string>; body?: string | null };

/** What the bridge holds for a pairing; the UI must never show it. */
const LOCAL_RUNNER_SECRET = "metro_rt_local_fixture_secret";
const FOLDER = "/home/me/.config/metro-desktop/local-tools/accounts/a1/spaces/solo/workspace";

/** The fake bridge plus local tools. `available`: is this session the computer's own workspace? */
function bridgeWithTools(f: F, available = true, opts: { startingReads?: number } = {}) {
  /** After enable, this many status reads still report the worker as starting. */
  let starting = 0;
  const b = fakeBridge(f);
  const tools = new Map<string, any>();
  const log: { method: string; path: string; body: any; auth: string | undefined }[] = [];
  const handleTools = (req: Req) => {
    const path = new URL(req.url).pathname;
    const body = req.body ? JSON.parse(req.body) : null;
    log.push({ method: req.method, path, body, auth: header(req as any, "authorization") });
    if (header(req as any, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return { status: 401, body: { error: { code: "unauthorized", message: "Unauthorized." } } };
    if (path === "/v1/local-tools") {
      for (const e of tools.values()) {
        if (e.worker === "starting" && starting-- <= 0) e.worker = "online";
      }
      return available
        ? { status: 200, body: { available: true, reason: null, spaces: [...tools.values()] } }
        : { status: 200, body: { available: false, reason: "Local tools work only in this computer's own Metro workspace.", spaces: [] } };
    }
    const m = /^\/v1\/local-tools\/spaces\/([^/]+)\/(enable|disable)$/.exec(path);
    if (!m || req.method !== "POST") return { status: 404, body: { error: { code: "not_found", message: "unknown path" } } };
    const space = f.state.spaces.get(m[1]!)!;
    if (!available) return { status: 409, body: { error: { code: "not_local", message: "not local" } } };
    if (m[2] === "enable") {
      // The guarded pairing: never replaces a runner the Space already has.
      if (space.runnerToken || f.state.attached.has(space.id)) {
        return { status: 409, body: { error: { code: "runner_present", message: "This Space already has a dedicated runner token. Metro won't replace it." } } };
      }
      space.runnerToken = LOCAL_RUNNER_SECRET;
      const startingFor = opts.startingReads;
      if (startingFor !== undefined) starting = startingFor;
      const e = { space_id: space.id, enabled: true, state: "on", worker: startingFor === undefined ? "online" : "starting", detail: null, folder: FOLDER, tools: ["workspace.list", "workspace.read"] };
      tools.set(space.id, e);
      return { status: 200, body: e };
    }
    if (tools.has(space.id)) space.runnerToken = null;
    tools.delete(space.id);
    return { status: 200, body: { space_id: space.id, enabled: false, state: "off", worker: "stopped", detail: null, folder: FOLDER } };
  };
  const network: Record<string, (req: Req) => unknown> = {};
  for (const method of ["GET", "POST", "PUT", "DELETE"]) {
    const inner = (b.network as any)[`${method} ${BRIDGE}/v1/*`];
    network[`${method} ${BRIDGE}/v1/*`] = (req: Req) => (new URL(req.url).pathname.startsWith("/v1/local-tools") ? handleTools(req) : inner(req));
  }
  return { b, tools, log, network };
}

async function openWith(f: F, t: ReturnType<typeof bridgeWithTools> | null) {
  const network = { ...(f.network as any), ...(t ? t.network : {}) };
  return open("../../../App.tsx", { network, platform: "linux", width: 1280, height: 900, modules: (t ? bridgeEnv() : launched()) as any });
}

async function signIn(app: App, user: string) {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
}

async function manage(app: App, sid: string) {
  await app.waitFor(`id:space:${sid}`);
  await app.press(`id:space:${sid}`);
  await app.waitFor("id:manage-space");
  await app.press("id:manage-space");
  await app.waitFor("id:runner-card");
}

const text = (app: App) => app.texts().join(" | ");

test("owner turns local tools on and off for a Space without a runner; no token is ever shown", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const t = bridgeWithTools(f);
  const app = await openWith(f, t);
  await signIn(app, "alice");
  await manage(app, "solo");
  await app.waitFor("id:local-tools-card");
  expect(app.shows("text:Off")).toBe(true);

  await app.press("id:local-tools-enable");
  await app.waitFor("id:local-tools-on");
  await app.waitFor("id:local-tools-folder");
  expect(text(app)).toContain(FOLDER);
  expect(t.log.filter((l) => l.method === "POST").map((l) => [l.path, l.body])).toEqual([["/v1/local-tools/spaces/solo/enable", {}]]);
  // The Runner card now shows the pairing as this Space's dedicated runner.
  await app.waitFor("id:dedicated-runner");

  await app.press("id:local-tools-copy-folder");
  expect(app.clipboard()).toBe(FOLDER);

  await app.press("id:local-tools-disable");
  await app.press("id:local-tools-confirm-disable");
  await app.waitFor("id:local-tools-on", { state: "gone" });
  await app.waitFor("id:attach-runner-list");
  expect(f.state.spaces.get("solo")!.runnerToken).toBe(null);

  // The bridge bearer goes only to the bridge; the pairing token never shows.
  for (const r of app.requests()) if (!r.url.startsWith(BRIDGE)) expect(JSON.stringify(r)).not.toContain(BRIDGE_TOKEN);
  expect(text(app)).not.toContain(LOCAL_RUNNER_SECRET);
  expect(JSON.stringify(app.storage())).not.toContain(LOCAL_RUNNER_SECRET);
  expect(JSON.stringify(app.storage())).not.toContain(BRIDGE_TOKEN);
});

test("a Space that already has a runner is never offered a replacement", async () => {
  const f = world(); // Acme has a dedicated token already
  const t = bridgeWithTools(f);
  const app = await openWith(f, t);
  await signIn(app, "alice");
  await manage(app, "acme");
  await app.waitFor("id:local-tools-card");
  await app.waitFor("id:local-tools-runner-present");
  expect(app.shows("id:local-tools-enable")).toBe(false);
  expect(t.log.filter((l) => l.method === "POST")).toEqual([]);
  expect(f.state.spaces.get("acme")!.runnerToken).toBe("runner-acme");
});

test("the bridge's refusal is shown as is, and nothing changes", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const t = bridgeWithTools(f);
  const app = await openWith(f, t);
  await signIn(app, "alice");
  await manage(app, "solo");
  await app.waitFor("id:local-tools-enable");
  // Someone pairs a runner after the card loaded: the guarded request refuses.
  f.state.spaces.get("solo")!.runnerToken = "operator-token";
  await app.press("id:local-tools-enable");
  await app.waitFor("id:local-tools-error");
  expect(text(app)).toContain("won't replace it");
  expect(f.state.spaces.get("solo")!.runnerToken).toBe("operator-token");
});

test("hidden unless this session is the computer's own workspace, and without the launcher", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const remote = bridgeWithTools(f, false);
  const app = await openWith(f, remote);
  await signIn(app, "alice");
  await manage(app, "solo");
  await app.waitUntil(() => remote.log.some((l) => l.path === "/v1/local-tools"));
  expect(app.shows("id:local-tools-card")).toBe(false);

  const plain = await openWith(world(), null);
  await signIn(plain, "alice");
  await manage(plain, "acme");
  expect(plain.shows("id:local-tools-card")).toBe(false);
  expect(plain.requests().some((r) => r.url.includes("/v1/local-tools"))).toBe(false);
});

test("members never see the switch", async () => {
  const f = world();
  const t = bridgeWithTools(f);
  const app = await openWith(f, t);
  await signIn(app, "alice");
  await app.waitFor("id:space:side"); // Bob owns Side
  await app.press("id:space:side");
  await app.waitFor("text:side lobby only");
  expect(app.shows("id:manage-space")).toBe(false);
  expect(app.shows("id:local-tools-card")).toBe(false);
});

const statusReads = (t: ReturnType<typeof bridgeWithTools>) => t.log.filter((l) => l.method === "GET" && l.path === "/v1/local-tools").length;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

test("after turning on, the card follows a starting worker to On without reopening the page", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const t = bridgeWithTools(f, true, { startingReads: 2 });
  const app = await openWith(f, t);
  await signIn(app, "alice");
  await manage(app, "solo");
  await app.press("id:local-tools-enable");
  await app.waitFor("text:Starting…");
  const before = statusReads(t);
  await app.waitFor("text:On", { timeout: 10_000 });
  expect(app.shows("text:Starting…")).toBe(false);
  // Exactly the reads needed, then it stops.
  const after = statusReads(t);
  expect(after - before).toBe(3);
  await sleep(2000);
  expect(statusReads(t)).toBe(after);
  // Guard semantics unchanged: one guarded enable, nothing else posted.
  expect(t.log.filter((l) => l.method === "POST").map((l) => l.path)).toEqual(["/v1/local-tools/spaces/solo/enable"]);
});

test("a worker stuck at starting is followed only while the page is open, and never past the limit", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const t = bridgeWithTools(f, true, { startingReads: 1_000 });
  const app = await openWith(f, t);
  await signIn(app, "alice");
  await manage(app, "solo");
  const opened = statusReads(t);
  await app.press("id:local-tools-enable");
  await app.waitUntil(() => statusReads(t) >= opened + 2, { timeout: 10_000 });
  // Leaving the page ends it.
  await app.press("id:close-manage");
  const left = statusReads(t);
  await sleep(2000);
  expect(statusReads(t)).toBe(left);

  // Back on the page it follows again (one poller), and signing out ends it.
  await app.press("id:manage-space");
  await app.waitUntil(() => statusReads(t) >= left + 3, { timeout: 10_000 });
  await app.press("id:account-menu"); // Sign out is in the account menu
  await app.press("id:disconnect");
  await sleep(300);
  const out = statusReads(t);
  await sleep(2000);
  expect(statusReads(t)).toBe(out);
  // Never more than the open read plus the bounded follow-up, per visit.
  expect(out - left).toBeLessThanOrEqual(1 + 20 + 1);
});

test("badge reflects the worker honestly", () => {
  const base = { spaceId: "s", enabled: true, detail: null, folder: "/x" } as const;
  expect(localToolsBadge(null).label).toBe("Off");
  expect(localToolsBadge({ ...base, state: "on", worker: "online" }).label).toBe("On");
  expect(localToolsBadge({ ...base, state: "on", worker: "offline" }).tone).toBe("warning");
  expect(localToolsBadge({ ...base, state: "revoked", worker: "revoked" }).tone).toBe("danger");
  expect(localToolsBadge({ ...base, state: "on", worker: "stopped" }).label).toBe("Paused");
});
