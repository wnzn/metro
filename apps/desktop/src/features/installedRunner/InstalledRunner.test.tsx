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
import { runnerOfferStorageKey } from "../../runnersState";
import { launched } from "../../../test/launched";

type F = ReturnType<typeof world>;
type Req = { method: string; url: string; headers: Record<string, string>; body?: string | null };

const ROOT = "/home/me/.config/metro-desktop/local-tools/runner/0123456789abcdef/spaces";

type Opts = { requested?: boolean; available?: boolean; state?: string; canReconnect?: boolean; legacy?: boolean; unavailableFor?: string; unconfirmedName?: string };

/** The fake bridge plus the personal runner routes. */
function bridgeWithRunner(f: F, runnerId: string | null, name: string, opts: Opts = {}) {
  const b = fakeBridge(f);
  const log: { method: string; path: string; body: string | null | undefined }[] = [];
  const t = { log };
  const st = { state: opts.state ?? "online", canReconnect: opts.canReconnect ?? false, runnerId, delay: 0, status: 200, available: opts.available ?? true };
  const status = () => ({
    requested: opts.requested ?? true,
    marker: opts.requested === false ? "absent" : "accepted",
    marker_detail: null,
    available: st.available,
    state: !st.available ? "unavailable" : st.state,
    detail: st.state === "revoked" ? "This computer's runner is no longer accepted by the local Metro server." : null,
    runner: !st.available || !st.runnerId ? null : { id: st.runnerId, name },
    ...(opts.unconfirmedName && !st.runnerId ? { unconfirmed_name: opts.unconfirmedName } : {}),
    spaces_folder: !st.available ? null : ROOT,
    tools: ["workspace.list", "workspace.read"],
    can_reconnect: st.canReconnect,
  });
  const handle = (req: Req) => {
    const path = new URL(req.url).pathname;
    log.push({ method: req.method, path, body: req.body });
    if (header(req as any, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return { status: 401, body: { error: { code: "unauthorized", message: "Unauthorized." } } };
    if (opts.legacy && path.startsWith("/v1/local-tools/runner")) return { status: 404, body: { error: { code: "not_found", message: "unknown path" } } };
    if (path === "/v1/local-tools/runner" && req.method === "GET") {
      if (opts.unavailableFor && b.state.session?.account_id === opts.unavailableFor) {
        return { status: 200, body: { ...status(), available: false, runner: null, state: "unavailable" } };
      }
      const body = st.status === 200 ? status() : { error: { code: "unavailable", message: "try again" } };
      return { status: st.status, body, ...(st.delay ? { delay: st.delay } : {}) };
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

async function openWith(f: F, t: ReturnType<typeof bridgeWithRunner> | null, manualClock = false) {
  const network = { ...(f.network as any), ...(t ? t.network : {}) };
  return open("../../../App.tsx", { network, platform: "linux", width: 1280, height: 900, modules: (t ? bridgeEnv() : launched()) as any, ...(manualClock ? { clock: {} } : {}) });
}

async function signIn(app: App, user: string) {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-chat");
}

async function openRunners(app: App) {
  if (!app.shows("id:nav-settings:runners")) await app.press("id:nav-settings");
  await app.press("id:nav-settings:runners");
  await app.waitFor("id:runners-settings");
}

async function signInToRunners(app: App, user: string) {
  await signIn(app, user);
  await openRunners(app);
}

const text = (app: App) => app.texts().join(" | ");
const posts = (t: ReturnType<typeof bridgeWithRunner>) => t.log.filter((l) => l.method === "POST");
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

for (const variant of ["member", "unversioned"] as const) {
  test(`a fresh ${variant} assignment cannot certify an installed folder`, async () => {
    const f = world();
    const mine = f.addRunner("This PC", "personal", "alice");
    f.addSpace("target", "Target", "alice");
    f.addSpace("valid", "Valid sibling", "alice");
    f.addSpace("partial", "Unreadable sibling", "alice");
    f.state.attached.set("target", mine.id);
    const target = f.state.spaces.get("target")!;
    const original = f.network[`GET ${BASE}/v1/*`]!;
    let unproven = false;
    let partial = false;
    f.network[`GET ${BASE}/v1/*`] = (req) => {
      const path = new URL(req.url).pathname;
      const reply = original(req) as { status: number; body: Record<string, unknown> };
      if (partial && path === "/v1/spaces/partial/runner") return { status: 503, body: {} };
      if (unproven && path === "/v1/spaces/target/runner") {
        if (variant === "member") return reply; // Generated while the caller is actually a member.
        const { assignment_revision: _revision, ...body } = reply.body;
        return { ...reply, body };
      }
      return reply;
    };
    const t = bridgeWithRunner(f, mine.id, "This PC");
    const app = await openWith(f, t, true);
    await signInToRunners(app, "alice");
    await app.waitFor("id:installed-runner-space:target");
    await app.press("id:settings-close");
    if (variant === "member") target.owner = "bob"; // The local Space list still says owner.
    unproven = true;
    partial = true;
    f.state.attached.set("valid", mine.id);
    await openRunners(app);
    await app.waitFor("id:installed-runner-space:valid");
    expect(app.shows("id:installed-runner-space:target")).toBe(false);
    expect(app.shows("id:installed-runner-folder:target")).toBe(false);
    expect(app.shows("id:installed-runner-space:partial")).toBe(false);

    // When owner proof returns the target is admitted, even though another
    // row of that same folder snapshot still fails.
    await app.press("id:settings-close");
    unproven = false;
    target.owner = "alice";
    await openRunners(app);
    await app.waitFor("id:installed-runner-space:target");
    expect(app.shows("id:installed-runner-space:valid")).toBe(true);
  });
}

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
  expect(text(app)).toContain("Every Space you own has a runner.");
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
  expect(app.shows(`id:rotate-runner:${mine.id}`)).toBe(false); // Reconnect already rotates the token.
  expect(text(app)).toContain("Token no longer accepted");
  await app.press("id:installed-runner-reconnect");
  await app.waitFor("id:installed-runner-reconnect:warning");
  expect(text(app)).toContain("its old token stops working at once");
  expect(text(app)).toContain("If it no longer exists, Metro creates a new personal runner instead");
  expect(posts(t)).toEqual([]);
  await app.press("id:installed-runner-reconnect:cancel");
  expect(posts(t)).toEqual([]);

  await app.press("id:installed-runner-reconnect");
  await app.press("id:installed-runner-reconnect:confirm");
  await app.waitFor("id:installed-runner-notice");
  // Plain `{}`: never a chosen runner id.
  expect(posts(t).map((l) => [l.path, l.body])).toEqual([["/v1/local-tools/runner/reconnect", "{}"]]);
  expect(text(app)).toContain("Online");
  expect(app.shows("id:installed-runner-reconnect")).toBe(false);
});

test("the healthy installed runner has exactly one scoped Rotate and Delete path", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const mine = f.addRunner("This PC", "personal", "alice");
  f.state.attached.set("solo", mine.id);
  const t = bridgeWithRunner(f, mine.id, mine.name);
  const app = await openWith(f, t);
  await signInToRunners(app, "alice");
  await app.waitFor("id:installed-runner-folder:solo");
  const rotate = `id:rotate-runner:${mine.id}`;
  const remove = `id:delete-runner:${mine.id}`;
  expect(app.shows("id:installed-runner-reconnect")).toBe(false);
  expect(app.shows(`id:installed-runner >> ${rotate}`)).toBe(true);
  expect(app.shows(`id:installed-runner >> ${remove}`)).toBe(true);
  expect(app.shows(`id:runner-list >> ${rotate}`)).toBe(false);
  expect(app.shows(`id:runner-list >> ${remove}`)).toBe(false);
  await app.press(rotate);
  await app.waitFor(`text:/This computer's runner will disconnect/`);
  await app.waitFor("text:/Spaces you belong to that use it: Solo/");
  expect(f.posts().filter((p) => p.path.startsWith("/v1/runners/"))).toEqual([]);
  await app.press(`${rotate}:confirm`);
  await app.waitFor("id:token-reveal");
  await app.press("id:hide-runner-token");
  await app.waitFor("id:installed-runner-folder:solo");
  await app.press(remove);
  await app.waitFor(`${remove}:warning`);
  await app.press(`${remove}:confirm`);
  await app.waitFor("id:runners-notice");
  expect(app.shows("id:installed-runner-folder:solo")).toBe(false);
  expect(f.state.attached.has("solo")).toBe(false);
  expect(f.posts().filter((p) => p.path.startsWith("/v1/runners/")).map((p) => [p.method, p.path])).toEqual([
    ["POST", `/v1/runners/${mine.id}/token`], ["DELETE", `/v1/runners/${mine.id}`],
  ]);
  expect(posts(t)).toEqual([]);
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
  await app.waitFor("id:installed-runner-reconnect:warning");
  expect(text(app)).toContain("Reconnecting creates a new personal runner");
  expect(text(app)).toContain("may have left an unused runner");
  expect(text(app)).toContain("You can choose where to attach it afterward.");
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

test("each account binds one Sidebar read; Settings refreshes without carrying the previous runner over", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice", ["bob"]);
  const mine = f.addRunner("This PC", "personal", "alice");
  f.state.attached.set("solo", mine.id);
  // Alice's session is this computer's own server; Bob's answer says "not here".
  const t = bridgeWithRunner(f, mine.id, "This PC", { unavailableFor: "bob" });
  const reads = () => t.log.filter((l) => l.path === "/v1/local-tools/runner").length;
  const app = await openWith(f, t, true);
  await signIn(app, "alice");
  await app.press("id:new-space-toggle");
  await app.waitFor("id:new-space-use-runner");
  expect(reads()).toBe(1);
  await openRunners(app);
  await app.waitFor("id:installed-runner-space:solo");
  expect(reads()).toBe(2);
  await app.press("id:settings-close");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect");
  await signIn(app, "bob");
  await app.waitUntil(() => reads() === 3);
  await app.clock!.runFor(5000);
  expect(reads()).toBe(3);
  await openRunners(app);
  await app.waitUntil(() => reads() === 4);
  await app.clock!.runFor(1);
  expect(app.shows("id:new-space-use-runner")).toBe(false);
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

test("bulk attach starts checked, respects picks, and loses safely to both kinds of concurrent pairing", async () => {
  const f = world();
  for (const [id, name] of [["a", "First"], ["b", "Second"], ["c", "Third"], ["d", "Unchecked"]]) f.addSpace(id!, name!, "alice");
  const mine = f.addRunner("This PC", "personal", "alice");
  const other = f.addRunner("Shared", "organization", "alice");
  f.state.beforeEmptyAttach = (sid) => {
    if (sid === "b") f.state.attached.set(sid, other.id);
    if (sid === "c") f.state.spaces.get(sid)!.runnerToken = "local-fixture";
  };
  const app = await openWith(f, bridgeWithRunner(f, mine.id, mine.name));
  await signInToRunners(app, "alice");
  await app.waitFor("id:runner-attach-pick:d");
  expect(app.shows("id:runner-attach-pick:acme")).toBe(false);
  expect(app.shows("id:runner-attach-pick:side")).toBe(false);
  expect(text(app)).toContain("Attach to 4 Spaces");
  for (const id of ["a", "b", "c", "d"]) await app.press(`id:runner-attach-pick:${id}`);
  expect(app.enabled("id:runner-attach-many")).toBe(false);
  for (const id of ["a", "b", "c"]) await app.press(`id:runner-attach-pick:${id}`);
  expect(f.posts().filter((p) => p.path.includes("/runner"))).toEqual([]);
  await app.press("id:runner-attach-many");
  await app.waitFor("id:runner-attach-result");
  expect(text(app)).toContain("Attached to 1 of 3 Spaces.");
  expect(text(app)).toContain("Second: Already has a runner.");
  expect(text(app)).toContain("Third: Already has a runner.");
  expect(f.state.attached.get("a")).toBe(mine.id);
  expect(f.state.attached.get("b")).toBe(other.id);
  expect(f.state.spaces.get("c")!.runnerToken).toBe("local-fixture");
  expect(f.state.attached.has("d")).toBe(false);
  expect(app.shows("id:installed-runner-folder:a")).toBe(true);
  expect(f.posts().filter((p) => p.path.includes("/runner")).map((p) => [p.method, p.path, p.body])).toEqual(
    ["a", "b", "c"].map((id) => ["POST", `/v1/spaces/${id}/runner/attach-if-empty`, { runner_id: mine.id }]),
  );
  await app.waitUntil(() => app.storage()[runnerOfferStorageKey(BASE, "alice")] === "dismissed");
  expect(app.storage()[runnerOfferStorageKey(BASE, "bob")]).toBe(undefined);
  expect(app.storage()[runnerOfferStorageKey("https://other.example", "alice")]).toBe(undefined);
  expect(app.shows("id:runner-attach-pick:d")).toBe(true); // dismissal does not remove the permanent action
});

test("legacy server or a launcher runner from another scope can't use bulk attach", async () => {
  for (const mode of ["legacy", "organization", "other account"]) {
    const f = world();
    f.addSpace("solo", "Solo", "alice");
    f.state.attachIfEmpty = mode !== "legacy";
    const mine = f.addRunner("This PC", mode === "organization" ? "organization" : "personal", mode === "other account" ? "bob" : "alice");
    const app = await openWith(f, bridgeWithRunner(f, mine.id, mine.name));
    await signInToRunners(app, "alice");
    await app.waitFor(mode === "legacy" ? "text:/This server doesn't support attaching only/" : "text:/runner isn't available to attach for this account/");
    expect(app.shows("id:runner-attach-many")).toBe(false);
    expect(f.posts().filter((p) => p.path.includes("/runner"))).toEqual([]);
  }
});

test("a missing safe attach route never falls back to replacement", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const mine = f.addRunner("This PC", "personal", "alice");
  f.state.missingEmptyAttachRoute = true;
  const app = await openWith(f, bridgeWithRunner(f, mine.id, mine.name));
  await signInToRunners(app, "alice");
  await app.waitFor("id:runner-attach-pick:solo");
  await app.press("id:runner-attach-many");
  await app.waitFor("id:runner-attach-result");
  expect(text(app)).toContain("Attached to 0 of 1 Space.");
  expect(f.state.attached.has("solo")).toBe(false);
  expect(f.posts().filter((p) => p.path.includes("/runner")).map((p) => [p.method, p.path])).toEqual([
    ["POST", "/v1/spaces/solo/runner/attach-if-empty"],
  ]);
  expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
});

test("an account change during the live check cannot run the old batch or set its dismissal", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const mine = f.addRunner("This PC", "personal", "alice");
  const app = await openWith(f, bridgeWithRunner(f, mine.id, mine.name));
  await signInToRunners(app, "alice");
  await app.waitFor("id:runner-attach-pick:solo");
  const path = "/v1/spaces/solo/runner";
  const late = f.answerLate(path, TOKENS.alice!, 1600);
  await app.press("id:runner-attach-many");
  await app.waitUntil(() => late.arrived() > 0);
  await app.press("id:settings-close");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect");
  await signInToRunners(app, "bob");
  await app.waitFor("text:/runner isn't available to attach for this account/");
  expect(f.state.attached.has("solo")).toBe(false);
  expect(f.posts().filter((p) => p.path.includes("/runner/attach-if-empty"))).toEqual([]);
  expect(app.shows("id:runner-attach-result")).toBe(false);
  expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
});

test("persistent concurrent consumers see late online and subsequent offline; dismissal stops refresh without clearing Create", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const mine = f.addRunner("This PC", "personal", "alice");
  const t = bridgeWithRunner(f, mine.id, mine.name, { state: "offline" });
  const reads = () => t.log.filter((l) => l.method === "GET" && l.path === "/v1/local-tools/runner").length;
  const launch = (storage?: Record<string, string>) => open("./runnerWatch.fixture.tsx", {
    network: { ...f.network, ...t.network } as any, modules: bridgeEnv() as any, platform: "linux", clock: {}, storage,
  });
  const app = await launch();
  await app.press("id:fixture-connect");
  await app.waitFor(`text:Create choice: ${mine.id}`);
  expect(reads()).toBe(1); // Both watchers use the initial read.
  expect(app.shows("id:dismiss-fixture-offer")).toBe(false);
  await app.clock!.runFor(36_000); // Longer than the old six-try window.
  expect(reads()).toBeGreaterThanOrEqual(7);
  expect(reads()).toBeLessThanOrEqual(8); // At most one poll per five seconds.
  expect(app.shows("id:dismiss-fixture-offer")).toBe(false);

  t.st.state = "online";
  await app.clock!.runFor(5100);
  expect(app.shows("text:Offer appearances: 1")).toBe(true);
  expect(app.shows("id:dismiss-fixture-offer")).toBe(true);
  t.st.state = "offline";
  await app.clock!.runFor(5100);
  expect(app.shows("id:dismiss-fixture-offer")).toBe(false);
  expect(app.shows(`text:Create choice: ${mine.id}`)).toBe(true);
  t.st.state = "online";
  await app.clock!.runFor(5100);
  expect(app.shows("text:Offer appearances: 2")).toBe(true);

  t.st.delay = 1500;
  const before = reads();
  await app.clock!.runFor(5000);
  expect(reads()).toBe(before + 1); // A read is now held in flight.
  await app.press("id:dismiss-fixture-offer");
  await app.clock!.runFor(20_000);
  expect(app.shows("text:Offer appearances: 2")).toBe(true);
  expect(app.shows("id:dismiss-fixture-offer")).toBe(false);
  expect(app.shows(`text:Create choice: ${mine.id}`)).toBe(true);
  expect(reads()).toBe(before + 1);
  const storage = app.storage();
  expect(storage[runnerOfferStorageKey(BASE, "alice")]).toBe("dismissed");

  t.st.delay = 0;
  const reopened = await launch(storage);
  await reopened.press("id:fixture-connect");
  await reopened.waitFor(`text:Create choice: ${mine.id}`);
  await reopened.clock!.runFor(15_000);
  expect(reopened.shows("text:Offer appearances: 0")).toBe(true);
  expect(reads()).toBe(before + 2); // Initialization is allowed; polling isn't.
});

test("a stalled bridge expires the online offer; unmount stops reads and remount validates before showing", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const mine = f.addRunner("This PC", "personal", "alice");
  const t = bridgeWithRunner(f, mine.id, mine.name);
  const app = await open("./runnerWatch.fixture.tsx", {
    network: { ...f.network, ...t.network } as any, modules: bridgeEnv() as any, platform: "linux", clock: {},
  });
  await app.press("id:fixture-connect");
  await app.waitFor("id:dismiss-fixture-offer");
  t.st.delay = 9000;
  t.st.state = "offline";
  await app.clock!.runFor(7200);
  expect(app.shows("id:dismiss-fixture-offer")).toBe(false); // Not waiting out the transport timeout.
  expect(app.shows(`text:Create choice: ${mine.id}`)).toBe(true);
  const before = t.log.length;
  await app.press("id:toggle-watchers");
  await app.clock!.runFor(15_000);
  expect(t.log.length).toBe(before);
  expect(app.shows("id:dismiss-fixture-offer")).toBe(false);
  expect(app.shows(`text:Create choice: ${mine.id}`)).toBe(true);
  t.st.delay = 0;
  await app.press("id:toggle-watchers");
  await app.clock!.runFor(1);
  expect(app.shows("text:Offer appearances: 1")).toBe(true);
  expect(app.shows("id:dismiss-fixture-offer")).toBe(false);
});

test("Settings upgrades a held Sidebar poll to one folder fetch for the still-mounted exact runner only", async () => {
  for (const change of ["same runner", "unmounted", "different runner"]) {
    const f = world();
    f.addSpace("solo", "Solo", "alice");
    f.addSpace("empty", "Empty", "alice");
    const mine = f.addRunner("This PC", "personal", "alice");
    const replacement = f.addRunner("Replacement", "personal", "alice");
    const t = bridgeWithRunner(f, mine.id, mine.name);
    const reads = () => t.log.filter((l) => l.method === "GET" && l.path === "/v1/local-tools/runner").length;
    const folders = () => f.state.log.filter((l) => l.method === "GET" && l.path === "/v1/spaces/solo/runner").length;
    const app = await open("./runnerWatch.fixture.tsx", {
      network: { ...f.network, ...t.network } as any, modules: bridgeEnv() as any, platform: "linux", width: 1280, height: 900, clock: {},
    });
    await app.press("id:fixture-connect");
    await app.waitFor("id:dismiss-fixture-offer");
    expect(reads()).toBe(1);
    expect(folders()).toBe(0); // Sidebar read verified ownership, not the folders.
    const id = change === "different runner" ? replacement.id : mine.id;
    f.state.attached.set("solo", id);
    t.st.runnerId = id;
    t.st.delay = 1800;
    await app.clock!.runFor(5100);
    expect(reads()).toBe(2);
    await app.press("id:toggle-runner-settings");
    await app.waitFor("id:runners-settings");
    expect(app.shows("id:installed-runner-folder:solo")).toBe(false);
    expect(reads()).toBe(2); // Two folder consumers joined the same held read.
    if (change === "unmounted") await app.press("id:toggle-runner-settings");
    await app.clock!.runFor(1800);
    if (change === "same runner") {
      await app.waitFor("id:installed-runner-folder:solo");
      expect(folders()).toBe(1);
    } else {
      expect(app.shows("id:installed-runner-folder:solo")).toBe(false);
      expect(folders()).toBe(0);
    }
    expect(reads()).toBe(2);
    t.st.delay = 0;
    await app.clock!.runFor(5300);
    if (change === "different runner") {
      await app.waitFor(`text:Create choice: ${replacement.id}`);
      await app.waitFor("id:installed-runner-folder:solo");
    }
    expect(folders()).toBe(change === "unmounted" ? 0 : 1);
    expect(posts(t)).toEqual([]);
  }
});

test("mounted Settings retries a failed or empty joined read after offer dismissal, then stops after one folder refresh", async () => {
  for (const failure of ["error", "no runner", "temporarily unavailable", "unmounted"]) {
    const f = world();
    f.addSpace("solo", "Solo", "alice");
    const mine = f.addRunner("This PC", "personal", "alice");
    const t = bridgeWithRunner(f, mine.id, mine.name);
    const reads = () => t.log.filter((l) => l.method === "GET" && l.path === "/v1/local-tools/runner").length;
    const folderReads = () => f.state.log.filter((l) => l.method === "GET" && l.path === "/v1/spaces/solo/runner").length;
    const app = await open("./runnerWatch.fixture.tsx", {
      network: { ...f.network, ...t.network } as any, modules: bridgeEnv() as any, platform: "linux", width: 1280, height: 900, clock: {},
    });
    await app.press("id:fixture-connect");
    await app.waitFor("id:dismiss-fixture-offer");
    expect(reads()).toBe(1);
    f.state.attached.set("solo", mine.id);
    t.st.delay = 1800;
    if (failure === "temporarily unavailable") t.st.available = false;
    else if (failure === "error" || failure === "unmounted") t.st.status = 503;
    else {
      t.st.runnerId = null;
      t.st.state = "starting";
    }
    await app.clock!.runFor(5100);
    await app.press("id:toggle-runner-settings");
    await app.waitFor("id:runners-settings");
    await app.press("id:dismiss-fixture-offer");
    await app.clock!.runFor(6400);
    expect(reads()).toBe(2); // The failed read didn't trigger an eager retry.
    expect(folderReads()).toBe(0);
    expect(app.shows("id:installed-runner-folder:solo")).toBe(false);

    t.st.delay = 0;
    if (failure === "unmounted") {
      await app.press("id:toggle-runner-settings");
      t.st.status = 200;
      await app.clock!.runFor(15_000);
      expect(reads()).toBe(2);
      expect(folderReads()).toBe(0);
      continue;
    }
    await app.clock!.runFor(600);
    expect(reads()).toBe(3); // One shared retry, despite two mounted folder consumers.
    expect(folderReads()).toBe(0);

    t.st.status = 200;
    t.st.available = true;
    t.st.runnerId = mine.id;
    t.st.state = "online";
    await app.clock!.runFor(5000);
    await app.waitFor("id:installed-runner-folder:solo");
    expect(reads()).toBe(4);
    expect(folderReads()).toBe(1);
    await app.clock!.runFor(15_000);
    expect(reads()).toBe(4);
    expect(folderReads()).toBe(1);
    expect(app.shows("id:dismiss-fixture-offer")).toBe(false);
    expect(posts(t)).toEqual([]);
  }
});

test("persistent folder failures exhaust the mounted Settings budget without stopping Sidebar liveness", async () => {
  for (const mode of ["bridge failure", "unregistered", "offer still active"]) {
    const f = world();
    f.addSpace("solo", "Solo", "alice");
    f.addSpace("empty", "Empty", "alice");
    const mine = f.addRunner("This PC", "personal", "alice");
    const t = bridgeWithRunner(f, mine.id, mine.name);
    const reads = () => t.log.filter((l) => l.method === "GET" && l.path === "/v1/local-tools/runner").length;
    const folderReads = () => f.state.log.filter((l) => l.method === "GET" && l.path === "/v1/spaces/solo/runner").length;
    const app = await open("./runnerWatch.fixture.tsx", {
      network: { ...f.network, ...t.network } as any, modules: bridgeEnv() as any, platform: "linux", width: 1280, height: 900, clock: {},
    });
    await app.press("id:fixture-connect");
    await app.waitFor("id:dismiss-fixture-offer");
    if (mode !== "offer still active") await app.press("id:dismiss-fixture-offer");
    if (mode === "unregistered") {
      t.st.runnerId = "no-longer-registered";
      t.st.state = "revoked";
      t.st.canReconnect = true;
    } else t.st.status = 503;
    f.state.attached.set("solo", mine.id);

    await app.press("id:toggle-runner-settings");
    await app.waitFor("id:runners-settings");
    await app.clock!.runFor(24_000);
    expect(app.shows("id:installed-runner-folders-retry")).toBe(false);
    // A duplicate consumer mounting mid-budget doesn't restart the clock.
    const beforeDuplicate = reads();
    await app.press("id:toggle-folder-watch");
    await app.press("id:toggle-folder-watch");
    expect(reads()).toBe(beforeDuplicate);
    await app.clock!.runFor(7000);
    await app.waitFor("id:installed-runner-folders-retry");
    expect(text(app)).toContain("Automatic checks stopped.");
    expect(folderReads()).toBe(0);
    const atExpiry = reads();
    expect(atExpiry).toBeGreaterThan(2);
    expect(atExpiry).toBeLessThanOrEqual(8); // Initial read plus bounded retries.

    t.st.status = 200;
    t.st.runnerId = mine.id;
    t.st.state = "online";
    t.st.canReconnect = false;
    await app.clock!.runFor(45_000);
    if (mode === "offer still active") {
      expect(reads()).toBeGreaterThan(atExpiry);
      expect(app.shows(`text:Create choice: ${mine.id}`)).toBe(true);
      expect(app.shows("id:dismiss-fixture-offer")).toBe(true);
    } else {
      expect(reads()).toBe(atExpiry); // Settings stayed mounted, but stopped polling.
    }
    expect(folderReads()).toBe(0); // Liveness doesn't reset the folder budget.
    expect(app.shows("id:installed-runner-folders-retry")).toBe(true);

    await app.press("id:installed-runner-folders-retry");
    await app.waitFor("id:installed-runner-folder:solo");
    expect(app.shows("id:installed-runner-folders-retry")).toBe(false);
    expect(folderReads()).toBe(1);
    await app.clock!.runFor(35_000);
    expect(folderReads()).toBe(1);
    // A stale registration ID needs one guarded follow-up for the new ID.
    if (mode !== "offer still active") expect(reads()).toBe(atExpiry + (mode === "unregistered" ? 2 : 1));
    expect(posts(t)).toEqual([]);
  }
});

const fixtureRelease = (install: string, action: string, extra = {}) => ({
  presence: { version: "0.4.0", online: true, seen_at: Date.now(), install, self_update: install === "managed" },
  update: { latest: "0.5.0", available: true, action, pending: null, last_result: null, ...extra },
});

test("managed runner updates require confirmation, report denial and accepted progress, and retry only on another confirmation", async () => {
  const f = world();
  const mine = f.addRunner("Build box", "personal", "alice");
  const release = fixtureRelease("managed", "self_update");
  f.state.runnerRelease.set(mine.id, release);
  const app = await openWith(f, null, true);
  await signInToRunners(app, "alice");
  const id = `id:runner-update:${mine.id}`;
  const posts = () => f.posts().filter((p) => p.path.endsWith("/update"));
  await app.waitFor(id);
  expect(text(app)).toContain("v0.4.0 · Online");
  expect(posts()).toEqual([]);
  await app.press(id);
  expect(text(app)).toContain("Build box downloads 0.5.0 from GitHub, verifies the signature itself and restarts.");
  await app.press(`${id}:cancel`);
  expect(posts()).toEqual([]);

  f.state.rejectRunnerUpdate.add(mine.id);
  await app.press(id);
  await app.press(`${id}:confirm`);
  await app.waitFor(`id:runner-update-error:${mine.id}`);
  expect(text(app)).toContain("Build box: You no longer have permission");
  expect(text(app)).not.toContain("Update to 0.5.0 requested");
  expect(posts()).toHaveLength(1);

  f.state.rejectRunnerUpdate.delete(mine.id);
  await app.press(id);
  await app.press(`${id}:confirm`);
  await app.waitFor("text:Update to 0.5.0 requested");
  expect(posts()).toHaveLength(2);
  expect(posts().at(-1)).toMatchObject({ user: "alice", path: `/v1/runners/${mine.id}/update`, body: { version: "0.5.0" } });
  f.state.runnerRelease.set(mine.id, fixtureRelease("managed", "self_update", {
    last_result: { target: "0.5.0", state: "failed", code: "verify_failed", at: Date.now() },
  }));
  await app.clock!.runFor(5100);
  await app.waitFor(`id:runner-update-failed:${mine.id}`);
  expect(text(app)).toContain("Build box: The runner could not verify the release. Last reported version: v0.4.0.");
  expect(text(app)).toContain("Try again…");
  expect(posts()).toHaveLength(2);

  await app.press(id);
  await app.press(`${id}:confirm`);
  await app.waitFor("text:Update to 0.5.0 requested");
  const completed = fixtureRelease("managed", "none", {
    available: false, last_result: { target: "0.5.0", state: "updated", code: null, at: Date.now() },
  });
  completed.presence.version = "0.5.0";
  f.state.runnerRelease.set(mine.id, completed);
  await app.clock!.runFor(5100);
  await app.waitFor("text:v0.5.0 · Online");
  expect(app.shows(id)).toBe(false);
  expect(posts()).toHaveLength(3);
});

test("an accepted update waits for the held pre-acceptance poll before polling again", async () => {
  const f = world();
  const mine = f.addRunner("Build box", "personal", "alice");
  const other = f.addRunner("Already updating", "personal", "alice");
  f.state.runnerRelease.set(mine.id, fixtureRelease("managed", "self_update"));
  f.state.runnerRelease.set(other.id, fixtureRelease("managed", "self_update", {
    pending: { target: "0.5.0", requested_at: Date.now() },
  }));
  const app = await openWith(f, null, true);
  await signInToRunners(app, "alice");
  const id = `id:runner-update:${mine.id}`;
  const pending = `id:runner-update-result:${mine.id} >> text:Update to 0.5.0 requested`;
  await app.waitFor(`id:runner-update-result:${other.id} >> text:Update to 0.5.0 requested`);

  // The existing pending row causes a production roster poll. Its old answer
  // knows neither row as pending: the unrelated update has already finished.
  f.state.runnerRelease.set(other.id, fixtureRelease("managed", "none", { available: false }));
  const held = f.answerLate("/v1/runners", TOKENS.alice!, 8000);
  await app.clock!.runFor(5100);
  expect(held.arrived()).toBe(1);
  f.answerLate("/v1/runners", TOKENS.alice!, 0);
  // Don't mutate the release object captured by the held reply.
  f.state.runnerRelease.set(mine.id, fixtureRelease("managed", "self_update"));
  await app.press(id);
  await app.press(`${id}:confirm`);
  await app.waitFor(pending);

  const completed = fixtureRelease("managed", "none", { available: false });
  completed.presence.version = "0.5.0";
  f.state.runnerRelease.set(mine.id, completed);
  const authoritative = f.answerLate("/v1/runners", TOKENS.alice!, 6300);

  await app.clock!.runFor(5600); // A tick passes with the old GET still held.
  expect(authoritative.arrived()).toBe(0);
  expect(app.shows(pending)).toBe(true);
  expect(app.shows(id)).toBe(false);

  await app.clock!.runFor(2800); // The old answer is released, but is stale.
  expect(authoritative.arrived()).toBe(0);
  expect(app.shows(pending)).toBe(true);
  expect(app.shows(id)).toBe(false);

  await app.clock!.runFor(6900); // A fresh poll starts; its next tick must not overlap.
  expect(authoritative.arrived()).toBe(1);
  expect(app.shows(pending)).toBe(true);
  await app.clock!.runFor(1700);
  await app.waitFor("text:v0.5.0 · Online");
  expect(app.shows(pending)).toBe(false);
  expect(app.shows(id)).toBe(false);
  await app.clock!.runFor(6000);
  expect(authoritative.arrived()).toBe(1);
  expect(f.posts().filter((p) => p.path.endsWith("/update"))).toEqual([
    { user: "alice", method: "POST", path: `/v1/runners/${mine.id}/update`, body: { version: "0.5.0" } },
  ]);
});

test("a delayed update acknowledgement cannot stamp the next account's runner roster", async () => {
  const f = world();
  const shared = f.addRunner("Shared build box", "organization", "alice");
  const path = `/v1/runners/${shared.id}/update`;
  f.state.runnerRelease.set(shared.id, fixtureRelease("managed", "self_update"));
  const held = f.answerLate(path, TOKENS.alice!, 3000);
  const app = await openWith(f, null, true);
  await signInToRunners(app, "alice");
  const id = `id:runner-update:${shared.id}`;
  await app.press(id);
  await app.press(`${id}:confirm`);
  await app.waitFor(`${id}:busy`);
  expect(held.arrived()).toBe(1);

  await app.press("id:settings-close");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect");
  // Bob can still see the same registered ID but the server isn't reporting
  // a pending update anymore.
  f.state.runnerRelease.set(shared.id, fixtureRelease("managed", "self_update"));
  await signInToRunners(app, "bob");
  await app.waitFor("text:Shared build box");
  await app.clock!.runFor(3100);
  expect(app.shows("text:Update to 0.5.0 requested")).toBe(false);
  expect(app.shows(id)).toBe(false);
  expect(f.posts().filter((p) => p.path === path).map((p) => p.user)).toEqual(["alice"]);
});

test("an update offer changed while confirming never requests a different target", async () => {
  const f = world();
  const mine = f.addRunner("Build box", "personal", "alice");
  f.state.runnerRelease.set(mine.id, fixtureRelease("managed", "self_update"));
  const app = await openWith(f, null);
  await signInToRunners(app, "alice");
  const id = `id:runner-update:${mine.id}`;
  await app.press(id);
  await app.waitFor(`${id}:warning`);
  f.state.runnerRelease.set(mine.id, fixtureRelease("managed", "self_update", { latest: "0.6.0" }));
  await app.press(`${id}:confirm`);
  await app.waitFor(`id:runner-update-error:${mine.id}`);
  expect(text(app)).toContain("This runner is no longer eligible for that update. Nothing was requested.");
  expect(f.posts().filter((p) => p.path.endsWith("/update"))).toEqual([]);
  await app.press(id);
  expect(text(app)).toContain("Build box downloads 0.6.0 from GitHub");
});

test("a held keyboard update keeps its scoped busy panel and restores focus to retry or result", async () => {
  const f = world();
  const mine = f.addRunner("Build box", "personal", "alice");
  const path = `/v1/runners/${mine.id}/update`;
  const updates = () => f.posts().filter((p) => p.path === path);
  f.state.runnerRelease.set(mine.id, fixtureRelease("managed", "self_update"));
  f.state.rejectRunnerUpdate.add(mine.id);
  f.answerLate(path, TOKENS.alice!, 1400);
  const app = await openWith(f, null, true);
  await signInToRunners(app, "alice");
  const id = `id:runner-update:${mine.id}`;
  const start = async () => {
    await app.waitFor(`${id}:panel`);
    await app.keyboard.press("tab"); // Cancel → Confirm
    await app.keyboard.press("enter");
    await app.waitFor(`${id}:busy >> text:Requesting…`);
  };
  await app.press(id);
  await start();
  expect(app.shows(`${id}:confirm`)).toBe(false);
  expect(app.enabled(`${id}:cancel`)).toBe(false);
  await app.keyboard.press("escape");
  await app.keyboard.press("enter");
  await app.keyboard.press("space");
  expect(app.shows("id:runners-settings")).toBe(true);
  expect(app.shows(`${id}:busy`)).toBe(true);
  expect(updates()).toHaveLength(1);
  await app.clock!.runFor(1500);
  await app.waitFor(`id:runner-update-error:${mine.id}`);
  expect(app.shows(`${id}:panel`)).toBe(false);
  expect(updates()).toHaveLength(1);

  f.state.rejectRunnerUpdate.delete(mine.id);
  await app.keyboard.press("enter"); // Focus returned to the retry trigger.
  await start();
  expect(updates()).toHaveLength(2);
  await app.clock!.runFor(1500);
  await app.waitFor(`id:runner-update-result:${mine.id} >> text:Update to 0.5.0 requested`);
  expect(app.shows(`${id}:panel`)).toBe(false);
  expect(app.shows(id)).toBe(false);
  await app.keyboard.press("tab"); // Result summary → next control (Rotate token).
  await app.keyboard.press("enter");
  await app.waitFor(`id:rotate-runner:${mine.id}:panel`);
  expect(updates()).toHaveLength(2);
});

test("an idle confirmation closes without a POST and hands keyboard focus to status when its runner goes offline", async () => {
  const f = world();
  const mine = f.addRunner("Build box", "personal", "alice");
  const other = f.addRunner("Already updating", "personal", "alice");
  const release = fixtureRelease("managed", "self_update");
  f.state.runnerRelease.set(mine.id, release);
  // An unrelated accepted update drives the page's read-only roster refresh.
  f.state.runnerRelease.set(other.id, fixtureRelease("managed", "self_update", {
    pending: { target: "0.5.0", requested_at: Date.now() },
  }));
  const app = await openWith(f, null, true);
  await signInToRunners(app, "alice");
  const id = `id:runner-update:${mine.id}`;
  await app.press(id);
  await app.waitFor(`${id}:panel`);
  await app.keyboard.press("tab"); // Cancel → Confirm, but don't submit.
  release.presence.online = false;
  await app.clock!.runFor(5100);
  await app.waitFor(`id:runner-update-result:${mine.id} >> text:Bring this runner online to request an update.`);
  expect(app.shows(`${id}:panel`)).toBe(false);
  expect(app.shows(id)).toBe(false);
  await app.keyboard.press("enter"); // The focused status cannot submit.
  await app.keyboard.press("tab"); // Status → this runner's Rotate token.
  await app.keyboard.press("enter");
  await app.waitFor(`id:rotate-runner:${mine.id}:panel`);
  expect(f.posts().filter((p) => p.path.endsWith("/update"))).toEqual([]);
});

test("Docker guidance only exposes official version-matched pinned images; embedded, unsupported and non-admin rows can't update", async () => {
  const f = world();
  const embedded = f.addRunner("Laptop", "personal", "bob");
  const old = f.addRunner("Old", "personal", "bob");
  const docker = f.addRunner("Container", "personal", "bob");
  const wrong = f.addRunner("Bad image", "personal", "bob");
  const org = f.addRunner("Shared", "organization", "alice");
  const offline = f.addRunner("Offline", "personal", "bob");
  const manual = f.addRunner("Manual binary", "personal", "bob");
  const image = `ghcr.io/wnzn/metro-runner:v0.5.0@sha256:${"a".repeat(64)}`;
  f.state.runnerRelease.set(embedded.id, fixtureRelease("desktop", "with_app"));
  f.state.runnerRelease.set(docker.id, fixtureRelease("container", "container", { container_image: image }));
  f.state.runnerRelease.set(wrong.id, fixtureRelease("container", "container", { container_image: image.replace("v0.5.0", "v0.4.0") }));
  f.state.runnerRelease.set(org.id, fixtureRelease("managed", "self_update"));
  f.state.runnerRelease.set(manual.id, fixtureRelease("unmanaged", "install_script"));
  const disconnected = fixtureRelease("managed", "self_update");
  disconnected.presence.online = false;
  f.state.runnerRelease.set(offline.id, disconnected);
  const app = await openWith(f, null);
  await signInToRunners(app, "bob");
  await app.waitFor(`id:runner-update-docker:${docker.id}:toggle`);
  expect(text(app)).toContain("Version unknown until it connects");
  expect(text(app)).toContain("Updates with Metro.");
  expect(text(app)).toContain("An org admin can update it.");
  expect(text(app)).toContain("Bring this runner online to request an update.");
  expect(app.shows(`id:runner-update-release:${manual.id}`)).toBe(true);
  for (const r of [docker, wrong, embedded, old, org, offline, manual]) expect(app.shows(`id:runner-update:${r.id}`)).toBe(false);
  await app.press(`id:runner-update-docker:${docker.id}:toggle`);
  expect(text(app)).toContain("recreate the runner container with that image. Keep its existing configuration and volume mounts.");
  await app.press(`id:runner-copy-image:${docker.id}`);
  expect(app.clipboard()).toBe(image);
  await app.press(`id:runner-update-docker:${wrong.id}:toggle`);
  expect(app.shows(`id:runner-copy-image:${wrong.id}`)).toBe(false);
  expect(text(app)).toContain("The server did not provide a valid pinned Metro runner image.");
  expect(f.posts().filter((p) => p.path.endsWith("/update"))).toEqual([]);
});
