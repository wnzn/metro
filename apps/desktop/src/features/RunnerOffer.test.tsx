import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, fakeServer, header, TOKENS, world } from "../../test/fakeServer";
import { BRIDGE, BRIDGE_TOKEN, bridgeEnv, fakeBridge } from "../device/test/fakeBridge";
import { runnerOfferStorageKey } from "../runnersState";

type Server = ReturnType<typeof fakeServer>;
type Request = Parameters<Server["network"][string]>[0];
type Network = NonNullable<Parameters<typeof open>[1]>["network"];

const choice = "id:new-space-use-runner";
const offer = "id:runner-offer";
const failure = "id:new-space-runner-failed";
const success = "id:new-space-runner-created";
const otherUrl = "http://127.0.0.1:8788";
const creates = (f: Server) => f.posts().filter((p) => p.path === "/v1/spaces");
const runnerWrites = (f: Server) => f.posts().filter((p) => p.path.includes("/runner"));
const space = (f: Server, name: string) => [...f.state.spaces.values()].find((s) => s.name === name)!;
const words = (app: App) => app.texts().join(" | ");

function fixture(f: Server, runnerId: string | null, state = "online") {
  const status = { runnerId, state, available: true, reads: 0, failReads: 0, delay: 0 };
  const b = fakeBridge(f);
  const network = { ...f.network, ...b.network } as Record<string, (req: Request) => unknown>;
  const bridgeGet = network[`GET ${BRIDGE}/v1/*`]!;
  network[`GET ${BRIDGE}/v1/*`] = (req) => {
    if (new URL(req.url).pathname !== "/v1/local-tools/runner") return bridgeGet(req);
    if (header(req, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return { status: 401 };
    status.reads++;
    if (status.reads <= status.failReads) return { status: 503, body: {} };
    return {
      status: 200, delay: status.delay,
      body: {
        requested: true, available: status.available, state: status.state,
        runner: status.runnerId && { id: status.runnerId, name: "This PC" },
        tools: ["workspace.list", "workspace.read"], can_reconnect: status.state === "revoked",
      },
    };
  };
  // The server applies the write before these gates hold or spoil the reply.
  const gate = { create: 0, attach: 0, badConfirmation: false };
  const serverPost = network[`POST ${BASE}/v1/*`]!;
  network[`POST ${BASE}/v1/*`] = (req) => {
    const reply = serverPost(req);
    const path = new URL(req.url).pathname;
    if (path === "/v1/spaces" && gate.create) return { ...(reply as object), delay: gate.create };
    if (path.endsWith("/runner/attach-if-empty")) {
      if (gate.badConfirmation) return { status: 200, body: {} };
      if (gate.attach) return { ...(reply as object), delay: gate.attach };
    }
    return reply;
  };
  return { status, gate, network };
}

async function launch(t: ReturnType<typeof fixture>, opts: { storage?: Record<string, string>; manual?: boolean; second?: Server; width?: number } = {}) {
  const network = { ...t.network };
  if (opts.second) {
    for (const [rule, respond] of Object.entries(opts.second.network)) {
      network[rule.replace(BASE, otherUrl)] = (req) => respond({ ...req, url: req.url.replace(otherUrl, BASE) });
    }
  }
  return open("../../App.tsx", {
    network: network as Network, modules: bridgeEnv() as any, platform: "linux", width: opts.width ?? 1280, height: 900,
    storage: opts.storage, ...(opts.manual ? { clock: {} } : {}),
  });
}

async function signIn(app: App, who = "alice", url = BASE) {
  if (app.shows("id:show-server-form")) await app.press("id:show-server-form");
  await app.clear("id:server-url");
  await app.type("id:server-url", url);
  await app.type("id:access-token", TOKENS[who]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-chat");
}

async function signOut(app: App) {
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect");
}

async function startSidebar(app: App, name: string, hasChoice = true) {
  await app.press("id:new-space-toggle");
  if (hasChoice) await app.waitFor(choice);
  await app.clear("id:new-space-name");
  await app.type("id:new-space-name", name);
}

test("sidebar requires submit consent, supports keyboard, and starts each new form checked", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  f.addSpace("other", "Attached", "alice");
  f.state.attached.set("other", r.id);
  const app = await launch(fixture(f, r.id));
  await signIn(app);
  await startSidebar(app, "Without runner");
  expect(words(app)).toContain("Agents in this Space can list and read files in its own folder on this computer.");
  expect(creates(f)).toEqual([]);
  expect(runnerWrites(f)).toEqual([]);
  await app.keyboard.press("tab");
  await app.keyboard.press("space"); // The checkbox is next after the name.
  await app.submit("id:new-space-name");
  await app.waitFor(`id:space:${space(f, "Without runner").id}`);
  await app.waitUntil(() => !app.shows("id:new-space-name"));
  expect(runnerWrites(f)).toEqual([]);

  await startSidebar(app, "With runner");
  await app.press(choice);
  await app.press("id:new-space-toggle"); // cancel and reopen also resets the choice
  await startSidebar(app, "With runner");
  await app.press("id:create-space");
  await app.waitFor(success);
  const id = space(f, "With runner").id;
  expect(creates(f)).toHaveLength(2);
  expect(runnerWrites(f)).toEqual([{ user: "alice", method: "POST", path: `/v1/spaces/${id}/runner/attach-if-empty`, body: { runner_id: r.id } }]);
  expect(f.state.attached.get(id)).toBe(r.id);
  expect(f.state.attached.get("other")).toBe(r.id);
  expect(words(app)).toContain("Created With runner with this computer's runner.");
  await app.waitUntil(() => app.storage()[runnerOfferStorageKey(BASE, "alice")] === "dismissed");
  expect(app.shows(offer)).toBe(false); // the first Space is eligible but the offer is now dismissed
});

test("first-Space onboarding attaches offline, does not show an offline offer, and enters the exact Space", async () => {
  const f = fakeServer();
  const r = f.addRunner("This PC", "personal", "carol");
  const app = await launch(fixture(f, r.id, "offline"));
  await signIn(app, "carol");
  await app.waitFor("id:onboarding-space-use-runner");
  expect(runnerWrites(f)).toEqual([]);
  await app.type("id:onboarding-space-name", "My first Space");
  await app.submit("id:onboarding-space-name");
  await app.waitFor(success);
  const id = space(f, "My first Space").id;
  expect(f.state.attached.get(id)).toBe(r.id);
  expect(app.shows("id:space-view")).toBe(true);
  expect(app.shows("id:onboarding")).toBe(false);
  expect(app.shows(offer)).toBe(false);
  expect(words(app)).toContain("It was offline when last checked; jobs may time out");
});

test("a revoked runner needs a reconnect, not just a wait for it to come back", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  const app = await launch(fixture(f, r.id, "revoked"));
  await signIn(app);
  await startSidebar(app, "Revoked warning");
  await app.press("id:create-space");
  await app.waitFor(success);
  expect(f.state.attached.get(space(f, "Revoked warning").id)).toBe(r.id);
  expect(words(app)).toContain("Its token is no longer accepted. Reconnect it in Settings › Runners. Jobs may time out");
});

test("unknown, foreign and legacy choices never render or send a runner attachment", async () => {
  for (const mode of ["not installed", "wrong account", "organization", "old server"]) {
    const f = world();
    f.addSpace("eligible", "Otherwise eligible", "alice");
    const r = f.addRunner("This PC", mode === "organization" ? "organization" : "personal", mode === "wrong account" ? "bob" : "alice");
    f.state.attachIfEmpty = mode !== "old server";
    const t = fixture(f, mode === "not installed" ? null : r.id);
    const app = await launch(t);
    await signIn(app);
    await app.waitUntil(() => t.status.reads > 0);
    if (mode !== "not installed") await app.waitUntil(() => f.state.log.some((p) => p.path === "/v1/runners"));
    await startSidebar(app, mode, false);
    expect(app.shows(choice)).toBe(false);
    expect(app.shows(offer)).toBe(false);
    await app.press("id:create-space");
    await app.waitUntil(() => !app.shows("id:new-space-name"));
    expect(creates(f)).toHaveLength(1);
    expect(runnerWrites(f)).toEqual([]);
  }
});

test("on a narrow window, the onboarding checkbox and unchecked creation work by keyboard", async () => {
  const f = fakeServer();
  const r = f.addRunner("This PC", "personal", "carol");
  const app = await launch(fixture(f, r.id), { width: 760 });
  await signIn(app, "carol");
  await app.waitFor("id:onboarding-space-use-runner");
  await app.type("id:onboarding-space-name", "No pairing");
  await app.keyboard.press("tab");
  await app.keyboard.press("space");
  await app.keyboard.press("tab");
  await app.keyboard.press("enter");
  await app.waitFor("id:space-view");
  expect(creates(f)).toHaveLength(1);
  expect(runnerWrites(f)).toEqual([]);
  expect(app.shows(success)).toBe(false);
});

test("atomic conflicts and a missing empty-only route preserve the created Space and expose scoped recovery", async () => {
  for (const kind of ["registered", "dedicated", "missing route"]) {
    const f = world();
    const r = f.addRunner("This PC", "personal", "alice");
    const other = f.addRunner("Shared", "organization", "alice");
    f.state.missingEmptyAttachRoute = kind === "missing route";
    f.state.beforeEmptyAttach = (id) => {
      if (kind === "registered") f.state.attached.set(id, other.id);
      if (kind === "dedicated") f.state.spaces.get(id)!.runnerToken = "fixture-dedicated";
    };
    const app = await launch(fixture(f, r.id));
    await signIn(app);
    await startSidebar(app, kind);
    await app.press("id:create-space");
    await app.waitFor(failure);
    const created = space(f, kind);
    expect(creates(f)).toHaveLength(1);
    expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([["POST", `/v1/spaces/${created.id}/runner/attach-if-empty`]]);
    expect(f.state.attached.get(created.id)).toBe(kind === "registered" ? other.id : undefined);
    expect(!!created.runnerToken).toBe(kind === "dedicated");
    expect(words(app)).toContain(`Created ${kind}. Metro didn't attach this computer's runner.`);
    expect(words(app)).not.toContain("before retrying");
    expect(words(app)).toContain(kind === "missing route" ? "safe attachment route" : "already has a runner");
    expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
    await app.press("id:new-space-open-runner");
    await app.waitFor("id:runner-card"); // existing management until U9 lands
  }
});

test("an unconfirmed attach is not a refusal or a retry; Open Runner checks the created ID", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  const t = fixture(f, r.id);
  t.gate.badConfirmation = true;
  const app = await launch(t);
  await signIn(app);
  await startSidebar(app, "Keep on unknown");
  await app.press("id:create-space");
  await app.waitFor(failure);
  const created = space(f, "Keep on unknown");
  expect(words(app)).toContain("Metro couldn't confirm whether this computer's runner was attached. Open Runner to check this Space before retrying.");
  expect(words(app)).not.toContain("Metro didn't attach");
  expect(f.state.attached.get(created.id)).toBe(r.id);
  await app.press("id:new-space-open-runner");
  await app.waitFor("id:attached-runner");
  expect(f.state.log.some((p) => p.method === "GET" && p.path === `/v1/spaces/${created.id}/runner`)).toBe(true);
  expect(creates(f)).toHaveLength(1);
  expect(runnerWrites(f)).toHaveLength(1);
});

test("a held successful attach reply cannot resurrect a runner after a newer Manage detach", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  const t = fixture(f, r.id);
  t.gate.attach = 800;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await startSidebar(app, "Detach before receipt");
  await app.press("id:create-space");
  await app.waitUntil(() => {
    const created = space(f, "Detach before receipt");
    return !!created && f.state.attached.get(created.id) === r.id;
  });
  const id = space(f, "Detach before receipt").id;
  await app.press("id:manage-space");
  await app.waitFor("id:attached-runner");
  await app.press("id:detach-runner");
  await app.press("id:confirm-detach");
  await app.waitFor(`id:attach-runner:${r.id}`);
  expect(f.state.attached.has(id)).toBe(false);

  await app.clock!.runFor(800);
  await app.waitUntil(() => !app.shows("id:new-space-name"));
  expect(app.shows("id:attached-runner")).toBe(false);
  expect(app.shows("id:test-runner-row")).toBe(false);
  expect(creates(f)).toHaveLength(1);
  expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([
    ["POST", `/v1/spaces/${id}/runner/attach-if-empty`],
    ["DELETE", `/v1/spaces/${id}/runner`],
  ]);
  await app.press("id:nav-settings");
  await app.waitFor(`id:runner-attach-pick:${id}`); // The paired flag is false.
  expect(app.shows(`id:installed-runner-space:${id}`)).toBe(false);
});

test("the follow-up read also detects a newer assignment made outside this app", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  const other = f.addRunner("Other computer", "personal", "alice");
  const t = fixture(f, r.id);
  t.gate.attach = 800;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await startSidebar(app, "Another client");
  await app.press("id:create-space");
  await app.waitUntil(() => {
    const created = space(f, "Another client");
    return !!created && f.state.attached.get(created.id) === r.id;
  });
  const id = space(f, "Another client").id;
  f.state.attached.set(id, other.id); // A later pairing by the same owner on another client.
  await app.clock!.runFor(800);
  await app.waitFor(failure);
  expect(words(app)).toContain("The runner assignment changed");
  expect(app.shows(success)).toBe(false);
  expect(f.state.attached.get(id)).toBe(other.id);
  await app.press("id:new-space-open-runner");
  await app.waitFor("id:attached-runner");
  expect(words(app)).toContain(other.name);
});

test("changing and restoring consent while the create response is held never posts an attach or duplicate create", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  const t = fixture(f, r.id);
  t.gate.create = 600;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await startSidebar(app, "One only");
  await app.submit("id:new-space-name");
  await app.waitUntil(() => creates(f).length === 1);
  await app.submit("id:new-space-name"); // Return cannot re-submit while busy
  await app.press(choice);
  await app.press(choice);
  await app.clock!.runFor(600);
  await app.waitFor(failure);
  expect(words(app)).toContain("The Create Space choice changed");
  expect(creates(f)).toHaveLength(1);
  expect(runnerWrites(f)).toEqual([]);
  expect(space(f, "One only")).toBeTruthy();
});

test("a runner becoming eligible during creation was not part of the submitted choice", async () => {
  const f = world();
  f.addSpace("eligible", "Eligible", "alice"); // Watches the offer while the worker starts.
  const r = f.addRunner("This PC", "personal", "alice");
  const t = fixture(f, null, "starting");
  t.gate.create = 5500;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await app.waitUntil(() => t.status.reads > 0);
  await startSidebar(app, "Before registration", false);
  expect(app.shows(choice)).toBe(false);
  await app.press("id:create-space");
  await app.waitUntil(() => creates(f).length === 1);
  t.status.runnerId = r.id;
  t.status.state = "online";
  await app.clock!.runFor(5000);
  await app.waitFor(choice);
  await app.clock!.runFor(500);
  await app.waitUntil(() => !app.shows("id:new-space-name"));
  expect(creates(f)).toHaveLength(1);
  expect(runnerWrites(f)).toEqual([]);
  expect(app.shows(success)).toBe(false);
});

test("the fresh bridge check cannot retarget a submitted choice to a different runner with the same name", async () => {
  const f = world();
  const a = f.addRunner("This PC", "personal", "alice");
  const b = f.addRunner("This PC", "personal", "alice");
  const t = fixture(f, a.id);
  t.gate.create = 600;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await startSidebar(app, "Keep my Space");
  await app.press("id:create-space");
  await app.waitUntil(() => creates(f).length === 1);
  t.status.runnerId = b.id;
  await app.clock!.runFor(600);
  await app.waitFor(failure);
  expect(words(app)).toContain("no longer available for this account");
  expect(runnerWrites(f)).toEqual([]);
  expect(space(f, "Keep my Space")).toBeTruthy();
});

test("signing out during live verification cannot attach using the old session", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  const t = fixture(f, r.id);
  const app = await launch(t, { manual: true });
  await signIn(app);
  await startSidebar(app, "Still mine");
  const late = f.answerLate("/v1/runners", TOKENS.alice!, 800);
  await app.press("id:create-space");
  await app.waitUntil(() => late.arrived() > 0);
  await signOut(app);
  await signIn(app, "bob");
  await app.clock!.runFor(800);
  expect(creates(f)).toHaveLength(1);
  expect(runnerWrites(f)).toEqual([]);
  expect(space(f, "Still mine").owner).toBe("alice");
  expect(app.shows(failure)).toBe(false);
  expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
});

test("rechecking ownership of the created ID prevents an attach even when its create reply said owner", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  const t = fixture(f, r.id);
  t.gate.create = 600;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await startSidebar(app, "Owner changed");
  await app.press("id:create-space");
  await app.waitUntil(() => creates(f).length === 1);
  const created = space(f, "Owner changed");
  created.members.add("bob");
  created.owner = "bob";
  await app.clock!.runFor(600);
  await app.waitFor(failure);
  expect(words(app)).toContain("You no longer own this Space");
  expect(runnerWrites(f)).toEqual([]);
});

test("a user can navigate away and back during verification without authorizing a late attach", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  const app = await launch(fixture(f, r.id), { manual: true });
  await signIn(app);
  await startSidebar(app, "Moved away");
  const late = f.answerLate("/v1/runners", TOKENS.alice!, 800);
  await app.press("id:create-space");
  await app.waitUntil(() => late.arrived() > 0);
  const id = space(f, "Moved away").id;
  await app.press("id:nav-chat");
  await app.press(`id:space:${id}`);
  await app.clock!.runFor(800);
  await app.waitUntil(() => !app.shows("id:new-space-name"));
  expect(creates(f)).toHaveLength(1);
  expect(runnerWrites(f)).toEqual([]);
});

test("the online offer opens the attach list without writing; Not now survives restart only for that person and server", async () => {
  const f = world();
  const a = f.addRunner("This PC", "personal", "alice");
  const b = f.addRunner("This PC", "personal", "bob");
  f.addSpace("eligible", "Eligible", "alice");
  const t = fixture(f, a.id);
  const app = await launch(t);
  await signIn(app);
  await app.waitFor(offer);
  expect(words(app)).toContain("1 of your Spaces has no runner.");
  await app.press("id:runner-offer-choose");
  await app.waitFor("id:runner-attach-pick:eligible");
  expect(app.shows("id:runner-attach-pick:side")).toBe(false);
  expect(app.shows("id:runner-attach-pick:acme")).toBe(false);
  expect(runnerWrites(f)).toEqual([]);
  expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
  await app.press("id:settings-close");
  await app.press("id:runner-offer-dismiss");
  await app.waitUntil(() => app.storage()[runnerOfferStorageKey(BASE, "alice")] === "dismissed");
  expect(app.shows(offer)).toBe(false);

  const second = world();
  const secondRunner = second.addRunner("This PC", "personal", "alice");
  second.addSpace("eligible2", "Eligible on other server", "alice");
  const restart = fixture(f, a.id);
  const next = await launch(restart, { storage: app.storage(), second });
  await signIn(next);
  await startSidebar(next, "Check eligibility");
  expect(next.shows(offer)).toBe(false);
  await signOut(next);
  restart.status.runnerId = b.id;
  await signIn(next, "bob");
  await next.waitFor(offer);
  await signOut(next);
  restart.status.runnerId = secondRunner.id;
  await signIn(next, "alice", otherUrl);
  await next.waitFor(offer);
  expect(runnerWrites(f)).toEqual([]);
  expect(runnerWrites(second)).toEqual([]);
});

test("only a recently verified online status offers existing Spaces; becoming offline removes the offer but not the checkbox", async () => {
  const f = world();
  f.addSpace("eligible", "Eligible", "alice");
  const r = f.addRunner("This PC", "personal", "alice");
  const t = fixture(f, r.id, "offline");
  const app = await launch(t, { manual: true });
  await signIn(app);
  await startSidebar(app, "Maybe later");
  expect(app.shows(offer)).toBe(false);
  t.status.state = "online";
  await app.clock!.runFor(5000);
  await app.waitFor(offer);
  t.status.state = "offline";
  await app.clock!.runFor(5000);
  await app.waitUntil(() => !app.shows(offer));
  expect(app.shows(choice)).toBe(true);
  expect(runnerWrites(f)).toEqual([]);
  expect(creates(f)).toEqual([]);
});

test("Settings reopen joins held Sidebar liveness and refreshes folders after manual attach and detach", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  f.addSpace("manual", "Manual", "alice");
  f.addSpace("eligible", "Keeps the Sidebar offer eligible", "alice");
  const t = fixture(f, r.id);
  const app = await launch(t, { manual: true });
  await signIn(app);
  await app.waitFor(offer);
  await app.press("id:runner-offer-choose");
  await app.waitFor("id:runner-attach-pick:manual");
  expect(app.shows("id:installed-runner-space:manual")).toBe(false);
  await app.press("id:settings-close");
  await app.press("id:space:manual");
  await app.press("id:manage-space");
  await app.waitFor(`id:attach-runner:${r.id}`);
  await app.press(`id:attach-runner:${r.id}`);
  await app.waitFor("id:attached-runner");
  t.status.delay = 800;
  const reopenDuringLiveness = async () => {
    const before = t.status.reads;
    await app.clock!.runFor(5000);
    expect(t.status.reads).toBe(before + 1);
    await app.press("id:nav-settings");
    await app.waitFor("id:runners-settings");
    expect(t.status.reads).toBe(before + 1); // Settings joins; no second bridge request.
    await app.clock!.runFor(800);
  };
  await reopenDuringLiveness();
  await app.waitFor("id:installed-runner-space:manual");
  await app.press("id:settings-close");
  await app.press("id:detach-runner");
  await app.press("id:confirm-detach");
  await app.waitFor(`id:attach-runner:${r.id}`);
  const folderReads = () => f.state.log.filter((p) => p.method === "GET" && p.path === "/v1/spaces/manual/runner").length;
  const before = folderReads();
  await reopenDuringLiveness();
  await app.waitUntil(() => folderReads() > before);
  await app.waitUntil(() => !app.shows("id:installed-runner-space:manual"));
  expect(app.shows("id:runner-attach-pick:manual")).toBe(true);
});

test("Sidebar recovers from its first failed status read and reopening Settings doesn't add a poller", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  f.addSpace("eligible", "Eligible", "alice");
  const t = fixture(f, r.id);
  t.status.failReads = 1;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await app.waitUntil(() => t.status.reads > 0);
  expect(t.status.reads).toBe(1);
  expect(app.shows(offer)).toBe(false);
  await app.clock!.runFor(5000);
  await app.waitFor(offer);
  expect(t.status.reads).toBe(2);
  await app.press("id:runner-offer-choose");
  await app.waitFor("id:runner-attach-pick:eligible");
  const opened = t.status.reads;
  await app.clock!.runFor(5000);
  expect(t.status.reads).toBe(opened + 1);
  await app.press("id:settings-close");
  await app.clock!.runFor(5000);
  expect(t.status.reads).toBe(opened + 2);
  await signOut(app);
  await app.clock!.runFor(15_000);
  expect(t.status.reads).toBe(opened + 2);
});
