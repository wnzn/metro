// Settings › Runners › This computer's runner, against the fake server
// (test/fakeServer.ts) and the fake desktop bridge extended with the
// `/v1/local-tools/runner…` routes the launcher forwards to desktop_tools.
// Fake tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, header, TOKENS, world } from "../../../test/fakeServer";
import { BRIDGE, BRIDGE_TOKEN, bridgeEnv, fakeBridge } from "../../device/test/fakeBridge";
import { installedRunnerBadge } from "./InstalledRunnerSection";
import { parseInstalledRunner } from "./installedRunner";
import { launched } from "../../../test/launched";

type F = ReturnType<typeof world>;
type Req = { method: string; url: string; headers: Record<string, string>; body?: string | null };

const ROOT = "/home/me/.config/metro-desktop/local-tools/runner/0123456789abcdef/spaces";

type Opts = { requested?: boolean; available?: boolean; state?: string; canReconnect?: boolean; legacy?: boolean; laterAvailable?: boolean; unconfirmedName?: string };

/** The fake bridge plus the personal runner routes. */
function bridgeWithRunner(f: F, runnerId: string | null, name: string, opts: Opts = {}) {
  const b = fakeBridge(f);
  const log: { method: string; path: string; body: string | null | undefined }[] = [];
  const t = { log };
  const st = { state: opts.state ?? "online", canReconnect: opts.canReconnect ?? false, runnerId };
  const status = () => ({
    requested: opts.requested ?? true,
    marker: opts.requested === false ? "absent" : "accepted",
    marker_detail: null,
    available: opts.available ?? true,
    state: opts.available === false ? "unavailable" : st.state,
    detail: st.state === "revoked" ? "This computer's runner is no longer accepted by the local Metro server." : null,
    runner: opts.available === false || !st.runnerId ? null : { id: st.runnerId, name },
    ...(opts.unconfirmedName && !st.runnerId ? { unconfirmed_name: opts.unconfirmedName } : {}),
    spaces_folder: opts.available === false ? null : ROOT,
    tools: ["workspace.list", "workspace.read"],
    can_reconnect: st.canReconnect,
  });
  const handle = (req: Req) => {
    const path = new URL(req.url).pathname;
    log.push({ method: req.method, path, body: req.body });
    if (header(req as any, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return { status: 401, body: { error: { code: "unauthorized", message: "Unauthorized." } } };
    if (opts.legacy && path.startsWith("/v1/local-tools/runner")) return { status: 404, body: { error: { code: "not_found", message: "unknown path" } } };
    if (path === "/v1/local-tools/runner" && req.method === "GET") {
      const reads = t.log.filter((l) => l.path === path && l.method === "GET").length;
      if (reads > 1 && opts.laterAvailable === false) return { status: 200, body: { ...status(), available: false, runner: null, state: "unavailable" } };
      return { status: 200, body: status() };
    }
    if (path === "/v1/local-tools/runner/reconnect" && req.method === "POST") {
      st.state = "online";
      st.canReconnect = false;
      return { status: 200, body: status() };
    }
    const m = /^\/v1\/local-tools\/runner\/spaces\/([^/]+)\/folder$/.exec(path);
    if (m && req.method === "POST") {
      const sid = m[1]!;
      return { status: 200, body: { space_id: sid, folder: `${ROOT}/${sid}`, attached: f.state.attached.get(sid) === st.runnerId, tools: ["workspace.list", "workspace.read"] } };
    }
    if (path === "/v1/local-tools") return { status: 200, body: { available: true, reason: null, spaces: [] } };
    return { status: 404, body: { error: { code: "not_found", message: "unknown path" } } };
  };
  const network: Record<string, (req: Req) => unknown> = {};
  for (const method of ["GET", "POST", "PUT", "DELETE"]) {
    const inner = (b.network as any)[`${method} ${BRIDGE}/v1/*`];
    network[`${method} ${BRIDGE}/v1/*`] = (req: Req) => (new URL(req.url).pathname.startsWith("/v1/local-tools") ? handle(req) : inner(req));
  }
  return { log, network, st };
}

async function openWith(f: F, t: ReturnType<typeof bridgeWithRunner> | null) {
  const network = { ...(f.network as any), ...(t ? t.network : {}) };
  return open("../../../App.tsx", { network, platform: "linux", width: 1280, height: 900, modules: (t ? bridgeEnv() : launched()) as any });
}

async function signInToRunners(app: App, user: string) {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-chat");
  if (!app.shows("id:nav-settings:runners")) await app.press("id:nav-settings");
  await app.press("id:nav-settings:runners");
  await app.waitFor("id:runners-settings");
}

const text = (app: App) => app.texts().join(" | ");
const posts = (t: ReturnType<typeof bridgeWithRunner>) => t.log.filter((l) => l.method === "POST");
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

test("shows the installer's runner, read-only tools and how to attach; opening only reads", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  f.addSpace("other", "Other", "alice");
  const mine = f.addRunner("This PC", "personal", "alice");
  // A different runner with the same name: never mistaken for this one.
  const twin = f.addRunner("This PC", "personal", "alice");
  f.state.attached.set("solo", mine.id);
  f.state.attached.set("other", twin.id);
  const t = bridgeWithRunner(f, mine.id, "This PC");
  const app = await openWith(f, t);
  await signInToRunners(app, "alice");
  await app.waitFor("id:installed-runner");
  await app.waitFor("id:installed-runner-space:solo");
  expect(app.shows("id:installed-runner-space:other")).toBe(false);
  expect(app.shows("id:installed-runner-space:side")).toBe(false); // Bob's Space
  expect(text(app)).toContain("workspace.list, workspace.read");
  expect(text(app)).toContain("Online");
  expect(text(app)).toContain("open Manage and choose it under Runner");
  expect(app.shows("id:installed-runner-reconnect")).toBe(false);
  await sleep(300);
  expect(posts(t)).toEqual([]);
  expect(f.posts().filter((p) => p.path.includes("/runner"))).toEqual([]);

  // The folder only on a press.
  await app.press("id:installed-runner-folder:solo");
  await app.waitFor("id:installed-runner-path:solo");
  expect(text(app)).toContain(`${ROOT}/solo`);
  expect(posts(t).map((l) => [l.path, l.body])).toEqual([["/v1/local-tools/runner/spaces/solo/folder", "{}"]]);
  await app.press("id:installed-runner-copy:solo");
  expect(app.clipboard()).toBe(`${ROOT}/solo`);

  // Nothing local-tools ever goes to the Metro server, and the bridge bearer stays on the bridge.
  for (const r of app.requests()) {
    if (!r.url.startsWith(BRIDGE)) {
      expect(r.url).not.toContain("/v1/local-tools");
      expect(JSON.stringify(r)).not.toContain(BRIDGE_TOKEN);
    }
  }
});

test("reconnect is offered only when allowed, explains itself, and posts only after confirming", async () => {
  const f = world();
  const mine = f.addRunner("This PC", "personal", "alice");
  const t = bridgeWithRunner(f, mine.id, "This PC", { state: "revoked", canReconnect: true });
  const app = await openWith(f, t);
  await signInToRunners(app, "alice");
  await app.waitFor("id:installed-runner-reconnect");
  expect(text(app)).toContain("Token no longer accepted");
  await app.press("id:installed-runner-reconnect");
  await app.waitFor("id:installed-runner-impact");
  expect(text(app)).toContain("its old token stops working at once");
  expect(text(app)).toContain("If it no longer exists, Metro creates a new personal runner instead");
  expect(posts(t)).toEqual([]);
  await app.press("id:installed-runner-cancel");
  expect(posts(t)).toEqual([]);

  await app.press("id:installed-runner-reconnect");
  await app.press("id:installed-runner-confirm-reconnect");
  await app.waitFor("id:installed-runner-notice");
  // Plain `{}`: never a chosen runner id.
  expect(posts(t).map((l) => [l.path, l.body])).toEqual([["/v1/local-tools/runner/reconnect", "{}"]]);
  expect(text(app)).toContain("Online");
  expect(app.shows("id:installed-runner-reconnect")).toBe(false);
});

test("an interrupted setup with no known id: reconnect creates a new runner and warns of an unused one", async () => {
  const f = world();
  const t = bridgeWithRunner(f, null, "This PC", { state: "interrupted", canReconnect: true, unconfirmedName: "Metro desktop abc" });
  const app = await openWith(f, t);
  await signInToRunners(app, "alice");
  await app.waitFor("id:installed-runner");
  expect(text(app)).toContain("Not confirmed (asked for “Metro desktop abc”)");
  expect(app.shows("id:installed-runner-spaces")).toBe(false);
  await app.press("id:installed-runner-reconnect");
  await app.waitFor("id:installed-runner-impact");
  expect(text(app)).toContain("Reconnecting creates a new personal runner");
  expect(text(app)).toContain("may have left an unused runner");
  expect(text(app)).toContain("attach it in each Space's Manage view");
  expect(posts(t)).toEqual([]);
});

test("hidden on a remote server, when not requested, with an older launcher, and without the launcher", async () => {
  for (const opts of [{ available: false }, { requested: false, available: false }, { legacy: true }] as Opts[]) {
    const f = world();
    const t = bridgeWithRunner(f, "rn1", "This PC", opts);
    const app = await openWith(f, t);
    await signInToRunners(app, "alice");
    await app.waitUntil(() => t.log.some((l) => l.path === "/v1/local-tools/runner"));
    await sleep(200);
    expect(app.shows("id:installed-runner")).toBe(false);
    expect(posts(t)).toEqual([]);
  }
  const plain = await openWith(world(), null);
  await signInToRunners(plain, "alice");
  expect(plain.shows("id:installed-runner")).toBe(false);
  expect(plain.requests().some((r) => r.url.includes("/v1/local-tools"))).toBe(false);
});

test("switching account re-reads once; the previous account's runner doesn't carry over", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice", ["bob"]);
  const mine = f.addRunner("This PC", "personal", "alice");
  f.state.attached.set("solo", mine.id);
  // Alice's session is this computer's own server; Bob's answer says "not here".
  const t = bridgeWithRunner(f, mine.id, "This PC", { laterAvailable: false });
  const app = await openWith(f, t);
  await signInToRunners(app, "alice");
  await app.waitFor("id:installed-runner-space:solo");
  // The page mounts more than once as it opens: still a single read.
  expect(t.log.filter((l) => l.path === "/v1/local-tools/runner").length).toBe(1);
  await app.press("id:settings-close");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect");
  await signInToRunners(app, "bob");
  await app.waitUntil(() => t.log.filter((l) => l.path === "/v1/local-tools/runner").length === 2);
  await sleep(300);
  expect(app.shows("id:installed-runner")).toBe(false);
  expect(app.shows("id:installed-runner-space:solo")).toBe(false);
  expect(posts(t)).toEqual([]);
});

test("parse and badge are strict and honest", () => {
  expect(parseInstalledRunner(null)).toBe(null);
  expect(parseInstalledRunner({ requested: true, available: false, state: "unavailable" })).toBe(null);
  expect(parseInstalledRunner({ requested: false, available: true, state: "online" })).toBe(null);
  expect(parseInstalledRunner({ requested: true, available: true, state: "weird" })).toBe(null);
  const r = parseInstalledRunner({ requested: true, available: true, state: "offline", runner: { id: "rn1", name: "PC" }, tools: ["workspace.list"], can_reconnect: false })!;
  expect(r.runner).toEqual({ id: "rn1", name: "PC" });
  expect(installedRunnerBadge(r).tone).toBe("warning");
  expect(installedRunnerBadge({ ...r, state: "revoked" }).tone).toBe("danger");
});
