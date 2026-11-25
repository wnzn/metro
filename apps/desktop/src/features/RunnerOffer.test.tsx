import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, fakeServer, header, TOKENS, world } from "../../test/fakeServer";
import { BRIDGE, BRIDGE_TOKEN, bridgeEnv, fakeBridge } from "../device/test/fakeBridge";
import { runnerOfferStorageKey } from "../runnersState";
import { spaceWorld } from "./space/test/fixtures";

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
  const status = { runnerId, state, available: true, reads: 0, failReads: 0, delay: 0, reconnects: 0 };
  const gate = {
    create: 0, attach: 0, badConfirmation: false, reconnect: 0,
    attachSpace: null as string | null,
    uncommittedAttach: null as { delay: number; commit: (() => void) | null } | null,
    runnerReadError: null as string | null,
    runnerRead: null as { sid: string; delay: number; parked: boolean; errorStatus?: number } | null,
    detach: null as { sid: string; beforeCommit: boolean; delay: number; commit: (() => void) | null } | null,
  };
  const runnerStatus = () => ({
    requested: true, available: status.available, state: status.state,
    runner: status.runnerId && { id: status.runnerId, name: "This PC" },
    tools: ["workspace.list", "workspace.read"], can_reconnect: status.state === "revoked",
  });
  const b = fakeBridge(f);
  const network = { ...f.network, ...b.network } as Record<string, (req: Request) => unknown>;
  const bridgeGet = network[`GET ${BRIDGE}/v1/*`]!;
  network[`GET ${BRIDGE}/v1/*`] = (req) => {
    if (new URL(req.url).pathname !== "/v1/local-tools/runner") return bridgeGet(req);
    if (header(req, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return { status: 401 };
    status.reads++;
    if (status.reads <= status.failReads) return { status: 503, body: {} };
    return { status: 200, delay: status.delay, body: runnerStatus() };
  };
  const bridgePost = network[`POST ${BRIDGE}/v1/*`]!;
  network[`POST ${BRIDGE}/v1/*`] = (req) => {
    if (new URL(req.url).pathname !== "/v1/local-tools/runner/reconnect") return bridgePost(req);
    if (header(req, "authorization") !== `Bearer ${BRIDGE_TOKEN}`) return { status: 401 };
    status.reconnects++;
    status.state = "online";
    return { status: 200, delay: gate.reconnect, body: runnerStatus() };
  };
  // The server handles the request before these gates hold or spoil its reply.
  const serverGet = network[`GET ${BASE}/v1/*`]!;
  network[`GET ${BASE}/v1/*`] = (req) => {
    const path = new URL(req.url).pathname;
    const reply = serverGet(req);
    const result = gate.runnerReadError && path === `/v1/spaces/${gate.runnerReadError}/runner`
      ? { status: 503, body: {} } : reply;
    const read = gate.runnerRead;
    if (read && !read.parked && path === `/v1/spaces/${read.sid}/runner`) {
      read.parked = true;
      if (read.errorStatus) return { status: read.errorStatus, body: {}, delay: read.delay };
      return { ...(result as object), delay: read.delay };
    }
    return result;
  };
  const serverPost = network[`POST ${BASE}/v1/*`]!;
  network[`POST ${BASE}/v1/*`] = (req) => {
    const path = new URL(req.url).pathname;
    const detach = gate.detach;
    if (detach && !detach.commit && path === `/v1/spaces/${detach.sid}/runner/detach-if-current`) {
      if (detach.beforeCommit) {
        detach.commit = () => { serverPost(req); };
        return { status: 204, delay: detach.delay };
      }
      detach.commit = () => {};
      return { ...(serverPost(req) as object), delay: detach.delay };
    }
    const uncommitted = gate.uncommittedAttach;
    if (uncommitted && !uncommitted.commit && path.endsWith("/runner/attach-if-empty")) {
      // This fixture gives a newly created, never-paired Space its first
      // revision. Verify the promised receipt against the actual fake write.
      const body = { runner: { id: runnerId, name: "This PC", visibility: "personal", kind: "registered" }, assignment_revision: "1" };
      uncommitted.commit = () => { expect(serverPost(req)).toMatchObject({ status: 200, body }); };
      return { status: 200, body, delay: uncommitted.delay };
    }
    const reply = serverPost(req);
    if (path === "/v1/spaces" && gate.create) return { ...(reply as object), delay: gate.create };
    if (path.endsWith("/runner/attach-if-empty")) {
      if (gate.badConfirmation) return { status: 200, body: {} };
      if (gate.attach && (!gate.attachSpace || path === `/v1/spaces/${gate.attachSpace}/runner/attach-if-empty`)) {
        return { ...(reply as object), delay: gate.attach };
      }
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

async function openRunnerSettings(app: App) {
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:runners");
  await app.waitFor("id:runners-settings");
}

async function openSpaceRunner(app: App) {
  await app.press("id:manage-space");
  await app.press("id:space-settings-tab:runner");
}

// U5 won't start a new folder demand during a batch. Begin the Settings
// snapshot first, and hold just its status reply until the interesting race.
async function openWithHeldFolderStatus(app: App, t: ReturnType<typeof fixture>, delay: number) {
  const before = t.status.reads;
  t.status.delay = delay;
  await openRunnerSettings(app);
  await app.waitUntil(() => t.status.reads > before);
  t.status.delay = 0;
}

for (const exactFails of [false, true]) {
  test(`an older Space list cannot restore empty after attach with ${exactFails ? "a failed" : "a successful"} exact read`, async () => {
    const f = world();
    const r = f.addRunner("This PC", "personal", "alice");
    f.addSpace("solo", "Solo", "alice");
    const t = fixture(f, r.id);
    const app = await launch(t, { manual: true });
    await signIn(app);
    await app.press("id:space:solo");
    await openSpaceRunner(app);
    await app.waitFor(`id:space-runner-attach:${r.id}`);
    const staleList = f.answerLate("/v1/spaces", TOKENS.alice!, 3000);
    await app.clock!.runFor(11_000);
    await app.waitUntil(() => staleList.arrived() === 1);

    const held = { sid: "solo", delay: 700, parked: false };
    t.gate.runnerRead = held;
    if (exactFails) t.gate.runnerReadError = "solo";
    await app.press(`id:space-runner-attach:${r.id}`);
    await app.waitUntil(() => held.parked);
    expect(f.state.attached.get("solo")).toBe(r.id);
    await app.waitFor("id:runner-state >> text:Runner not confirmed");
    expect(app.shows("id:attached-runner")).toBe(false);
    expect(app.shows(`id:space-runner-attach:${r.id}`)).toBe(false);
    expect(app.shows("id:runner-state >> text:No runner")).toBe(false);

    await app.clock!.runFor(700);
    await app.waitFor(exactFails ? "id:manage-error" : "id:attached-runner");
    await app.clock!.runFor(3000);
    await app.waitFor(exactFails ? "id:runner-state >> text:Runner not confirmed" : "id:runner-state >> text:Runner attached");
    await app.press("id:space-settings-close");
    await openRunnerSettings(app);
    expect(app.shows("id:runner-attach-pick:solo")).toBe(false);
    expect(app.shows("id:runner-state >> text:No runner")).toBe(false);
    if (exactFails) {
      await app.waitFor("text:Some runner assignments haven’t been confirmed yet.");
      expect(app.shows("text:Every Space you own has a runner.")).toBe(false);
    }
  });
}

for (const mode of ["Create", "bulk"] as const) {
test(`a ${mode} acknowledgement fences a pre-commit folder read${mode === "Create" ? " after leaving the form" : ""}`, async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  if (mode === "bulk") f.addSpace("solo", "Solo", "alice");
  f.addSpace("sibling", "Sibling", "alice");
  f.state.attached.set("sibling", r.id);
  const t = fixture(f, r.id);
  const attach = { delay: 800, commit: null as (() => void) | null };
  t.gate.uncommittedAttach = attach;
  const app = await launch(t, { manual: true });
  await signIn(app);
  if (mode === "Create") {
    await startSidebar(app, "Held Create attach");
    await app.press("id:create-space");
  } else {
    await openWithHeldFolderStatus(app, t, 300);
    await app.waitFor("id:runner-attach-pick:solo");
    await app.press("id:runner-attach-many");
  }
  await app.waitUntil(() => attach.commit !== null);
  const sid = mode === "Create" ? space(f, "Held Create attach").id : "solo";
  expect(f.state.attached.has(sid)).toBe(false);

  const preCommit = { sid, delay: mode === "Create" ? 1600 : 1300, parked: false };
  t.gate.runnerRead = preCommit;
  if (mode === "Create") await openRunnerSettings(app);
  else await app.clock!.runFor(300);
  await app.waitUntil(() => preCommit.parked);
  expect(app.shows(`id:runner-attach-pick:${sid}`)).toBe(true);
  const exact = { sid, delay: 1600, parked: false };
  t.gate.runnerRead = exact;
  attach.commit!();
  await app.clock!.runFor(mode === "Create" ? 800 : 500);
  await app.waitUntil(() => exact.parked);
  expect(app.shows(`id:runner-attach-pick:${sid}`)).toBe(false);
  await app.clock!.runFor(800);
  await app.waitFor("id:installed-runner-space:sibling");
  expect(app.shows(`id:runner-attach-pick:${sid}`)).toBe(false);
  expect(app.shows(`id:installed-runner-space:${sid}`)).toBe(false);
  await app.clock!.runFor(800);
  await app.waitFor(`id:installed-runner-space:${sid}`);
  expect(app.shows("id:installed-runner-space:sibling")).toBe(true);
  expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([["POST", `/v1/spaces/${sid}/runner/attach-if-empty`]]);
});
}

test("deleting a registration reconciles its confirmed Space without guessing empty", async () => {
  const f = world();
  const r = f.addRunner("Remote runner", "personal", "alice");
  f.addSpace("solo", "Solo", "alice");
  f.state.attached.set("solo", r.id);
  const t = fixture(f, null);
  const app = await launch(t, { manual: true });
  await signIn(app);
  await app.press("id:space:solo");
  await app.waitFor("id:runner-state >> text:Runner attached");
  await openRunnerSettings(app);
  await app.press(`id:delete-runner:${r.id}`);
  await app.waitFor("text:/Spaces you belong to that use it: Solo/");
  const held = { sid: "solo", delay: 600, parked: false };
  t.gate.runnerRead = held;
  await app.press(`id:delete-runner:${r.id}:confirm`);
  await app.waitUntil(() => held.parked);
  await app.waitFor("id:runner-state >> text:Runner not confirmed");
  await app.press("id:settings-close");
  await app.clock!.runFor(600);
  await app.waitFor("id:runner-state >> text:No runner");
});

test("Delete cannot submit while impact is held or failed and can retry with proof", async () => {
  const f = world();
  const r = f.addRunner("Remote runner", "personal", "alice");
  f.addSpace("solo", "Solo", "alice");
  f.state.attached.set("solo", r.id);
  const t = fixture(f, null);
  const app = await launch(t, { manual: true });
  await signIn(app);
  await openRunnerSettings(app);
  const held = { sid: "solo", delay: 800, parked: false };
  t.gate.runnerRead = held;
  t.gate.runnerReadError = "solo";
  const confirm = `id:delete-runner:${r.id}:confirm`;
  await app.press(`id:delete-runner:${r.id}`);
  await app.waitUntil(() => held.parked);
  expect(app.enabled(confirm)).toBe(false);
  await app.press(confirm).catch(() => {});
  expect(runnerWrites(f)).toEqual([]);

  await app.clock!.runFor(800);
  await app.waitFor(`id:delete-runner:${r.id}:retry-impact`);
  expect(app.enabled(confirm)).toBe(false);
  expect(words(app)).not.toContain("None of your Spaces use it.");
  t.gate.runnerReadError = null;
  await app.press(`id:delete-runner:${r.id}:retry-impact`);
  await app.waitUntil(() => app.enabled(confirm));
  await app.waitFor("text:/Spaces you belong to that use it: Solo/");
  await app.press(confirm);
  await app.waitUntil(() => !f.state.runners.has(r.id));
  expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([["DELETE", `/v1/runners/${r.id}`]]);
});

test("a held assignment write immediately disables Delete and exposes a fresh impact retry", async () => {
  const f = world();
  const installed = f.addRunner("This PC", "personal", "alice");
  const other = f.addRunner("Remote runner", "personal", "alice");
  f.addSpace("kept", "Kept folder", "alice");
  f.state.attached.set("kept", installed.id);
  f.addSpace("remote", "Remote Space", "alice");
  f.state.attached.set("remote", other.id);
  f.addSpace("solo", "Solo", "alice");
  const t = fixture(f, installed.id);
  t.gate.attach = 1000;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await openRunnerSettings(app);
  await app.waitFor("id:installed-runner-space:kept");
  const confirm = `id:delete-runner:${other.id}:confirm`;
  const retry = `id:delete-runner:${other.id}:retry-impact`;
  await app.press(`id:delete-runner:${other.id}`);
  await app.waitUntil(() => app.enabled(confirm));
  expect(app.shows(retry)).toBe(false);

  await app.press("id:runner-attach-many");
  await app.waitUntil(() => f.state.attached.get("solo") === installed.id);
  // Do not release the write reply or refresh membership: dispatch alone
  // must invalidate this panel's previous exact impact proof.
  await app.waitFor(retry);
  expect(app.enabled(confirm)).toBe(false);
  await app.press(confirm).catch(() => {});
  expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([["POST", "/v1/spaces/solo/runner/attach-if-empty"]]);
  expect(app.shows("id:installed-runner-space:kept")).toBe(true);

  await app.clock!.runFor(1000);
  await app.waitFor("id:installed-runner-space:solo");
  expect(app.enabled(confirm)).toBe(false);
  await app.press(retry);
  await app.waitUntil(() => app.enabled(confirm));
  await app.press(confirm);
  await app.waitUntil(() => !f.state.runners.has(other.id));
  expect(app.shows("id:installed-runner-space:kept")).toBe(true);
  expect(f.state.attached.get("solo")).toBe(installed.id);
  expect(f.state.attached.get("kept")).toBe(installed.id);
});

test("a held Delete impact cannot authorize a different registration or a reopened panel", async () => {
  const f = world();
  const a = f.addRunner("Runner A", "personal", "alice");
  const b = f.addRunner("Runner B", "personal", "alice");
  f.addSpace("solo", "Solo", "alice");
  f.state.attached.set("solo", a.id);
  const t = fixture(f, null);
  const app = await launch(t, { manual: true });
  await signIn(app);
  await openRunnerSettings(app);
  const aHeld = { sid: "solo", delay: 500, parked: false };
  t.gate.runnerRead = aHeld;
  await app.press(`id:delete-runner:${a.id}`);
  await app.waitUntil(() => aHeld.parked);
  await app.press("id:settings-close");
  await openRunnerSettings(app);
  const bHeld = { sid: "solo", delay: 1000, parked: false };
  t.gate.runnerRead = bHeld;
  await app.press(`id:delete-runner:${b.id}`);
  await app.waitUntil(() => bHeld.parked);
  const confirm = `id:delete-runner:${b.id}:confirm`;
  await app.clock!.runFor(500);
  expect(app.shows(`id:delete-runner:${a.id}:panel`)).toBe(false);
  expect(app.enabled(confirm)).toBe(false);
  expect(words(app)).not.toContain("Spaces you belong to that use it: Solo");
  expect(runnerWrites(f)).toEqual([]);
  await app.clock!.runFor(500);
  await app.waitUntil(() => app.enabled(confirm));
  await app.waitFor("text:/None of your Spaces use it/");
  await app.press(confirm);
  await app.waitUntil(() => !f.state.runners.has(b.id));
  expect(f.state.attached.get("solo")).toBe(a.id);
});

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

test("a held Reconnect hides both Create checkboxes without silently dropping a previously checked choice", async () => {
  for (const onboarding of [false, true]) {
    const f = onboarding ? fakeServer() : world();
    const r = f.addRunner("This PC", "personal", "alice");
    const t = fixture(f, r.id, "revoked");
    t.gate.reconnect = 1000;
    const app = await launch(t, { manual: true });
    await signIn(app);
    const label = "Create during reconnect";
    const checkbox = onboarding ? "id:onboarding-space-use-runner" : choice;
    const field = onboarding ? "id:onboarding-space-name" : "id:new-space-name";
    if (onboarding) {
      await app.waitFor(checkbox);
      await app.type(field, label);
    } else await startSidebar(app, label);
    await openRunnerSettings(app);
    await app.press("id:installed-runner-reconnect");
    await app.press("id:installed-runner-reconnect:confirm");
    await app.waitUntil(() => t.status.reconnects === 1);
    await app.press("id:settings-close");
    await app.waitFor(onboarding ? "id:onboarding-space-runner-unavailable" : "id:new-space-runner-unavailable");
    expect(app.shows(checkbox)).toBe(false);
    expect(creates(f)).toEqual([]);

    await app.submit(field);
    await app.waitFor(failure);
    expect(words(app)).toContain(`Created ${label}. Metro didn't attach this computer's runner.`);
    expect(words(app)).toContain("wasn't available when you submitted");
    expect(app.shows("id:new-space-open-runner")).toBe(true);
    await app.clock!.runFor(1000);
    expect(creates(f)).toHaveLength(1);
    expect(runnerWrites(f)).toEqual([]);
    expect(f.state.attached.has(space(f, label).id)).toBe(false);
    expect(app.shows(success)).toBe(false);
  }
});

test("Reconnect during post-create verification reports the missing attach in the same Space", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  const t = fixture(f, r.id, "revoked");
  t.gate.reconnect = 1200;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await startSidebar(app, "Before reconnect");
  const late = f.answerLate("/v1/runners", TOKENS.alice!, 800);
  await app.press("id:create-space");
  await app.waitUntil(() => late.arrived() > 0);
  await openRunnerSettings(app);
  await app.press("id:installed-runner-reconnect");
  await app.press("id:installed-runner-reconnect:confirm");
  await app.waitUntil(() => t.status.reconnects === 1);
  await app.press("id:settings-close");
  await app.clock!.runFor(800);
  await app.waitFor(failure);
  expect(words(app)).toContain("Created Before reconnect. Metro didn't attach this computer's runner.");
  expect(words(app)).toContain("no longer available for this account");
  expect(app.shows("id:new-space-open-runner")).toBe(true);
  await app.clock!.runFor(400);
  expect(creates(f)).toHaveLength(1);
  expect(runnerWrites(f)).toEqual([]);
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
    expect(app.shows("id:new-space-runner-unavailable")).toBe(false);
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
    await app.waitFor("id:space-runner");
    expect(app.shows("id:space-members")).toBe(false);
    const expected = kind === "registered" ? "Attached · Shared" : kind === "dedicated" ? "Dedicated token" : "No runner";
    await app.waitFor(`id:space-runner-status >> text:${expected}`);
    if (kind === "missing route") {
      await app.waitFor(`id:space-runner-attach:${r.id}`);
      expect(app.shows("text:Recommended")).toBe(true);
    } else {
      expect(app.shows(`id:space-runner-attach:${r.id}`)).toBe(false);
    }
    expect(runnerWrites(f)).toHaveLength(1);
    if (kind === "missing route") {
      f.state.missingEmptyAttachRoute = false;
      await app.press(`id:space-runner-attach:${r.id}`);
      await app.waitFor("id:space-runner-status >> text:Attached · This PC");
      expect(f.state.attached.get(created.id)).toBe(r.id);
      expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([
        ["POST", `/v1/spaces/${created.id}/runner/attach-if-empty`],
        ["POST", `/v1/spaces/${created.id}/runner/attach-if-empty`],
      ]);
      await app.waitUntil(() => app.storage()[runnerOfferStorageKey(BASE, "alice")] === "dismissed");
    }
    await app.press("id:space-settings-close");
    await app.waitFor("id:space-settings-dialog", { state: "gone" });
    expect(app.shows("id:space-view")).toBe(true);
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

test("a held successful attach reply cannot resurrect a runner after a newer Space settings detach", async () => {
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
  await app.press("id:space-settings-tab:runner");
  await app.waitFor("id:attached-runner");
  await app.press("id:space-runner-detach");
  await app.press("id:space-runner-detach:confirm");
  await app.waitFor(`id:space-runner-attach:${r.id}`);
  expect(f.state.attached.has(id)).toBe(false);

  await app.clock!.runFor(800);
  await app.waitUntil(() => !app.shows("id:new-space-name"));
  expect(app.shows("id:attached-runner")).toBe(false);
  expect(app.shows("id:test-runner-row")).toBe(false);
  expect(creates(f)).toHaveLength(1);
  expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([
    ["POST", `/v1/spaces/${id}/runner/attach-if-empty`],
    ["POST", `/v1/spaces/${id}/runner/detach-if-current`],
  ]);
  await app.press("id:space-settings-close");
  await openRunnerSettings(app);
  await app.waitFor(`id:runner-attach-pick:${id}`); // The paired flag is false.
  expect(app.shows(`id:installed-runner-space:${id}`)).toBe(false);
});

test("a held pre-create Space list cannot purge the successfully attached new Space", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  const app = await launch(fixture(f, r.id), { manual: true });
  await signIn(app);
  const list = f.answerLate("/v1/spaces", TOKENS.alice!, 3000);
  await app.clock!.runFor(11_000);
  await app.waitUntil(() => list.arrived() === 1); // Snapshot predates the Space.
  f.answerLate("/v1/spaces", TOKENS.alice!, 0); // The create preflight sees live membership.
  await startSidebar(app, "Survives the old list");
  await app.press("id:create-space");
  await app.waitFor(success);
  const id = space(f, "Survives the old list").id;
  expect(f.state.attached.get(id)).toBe(r.id);
  await app.clock!.runFor(3000);
  await app.waitFor(`id:space:${id}`);
  await app.waitFor("id:runner-state >> text:Runner attached");

  // The protection is ordered, not a permanent pin on a created Space.
  f.state.spaces.delete(id);
  await app.clock!.runFor(11_000);
  await app.waitFor(`id:space:${id}`, { state: "gone" });
});

test("an older positive Space list cannot restore a forced removal, but a newer rejoin can", async () => {
  const w = spaceWorld();
  const f = w.f;
  const t = fixture(f, null);
  const app = await launch({ ...t, network: { ...t.network, ...w.network } }, { manual: true });
  await signIn(app);
  await app.press("id:space:side");
  await app.waitFor("id:space-settings-open");
  const oldList = f.answerLate("/v1/spaces", TOKENS.alice!, 4000);
  await app.clock!.runFor(11_000);
  await app.waitUntil(() => oldList.arrived() === 1);
  f.answerLate("/v1/spaces", TOKENS.alice!, 0);

  await app.press("id:space-settings-open");
  await app.waitFor("id:leave-space");
  await app.press("id:leave-space");
  await app.press("id:leave-confirm");
  await app.waitFor("id:space:side", { state: "gone" });
  expect(f.state.spaces.get("side")!.members.has("alice")).toBe(false);
  await app.clock!.runFor(4000);
  expect(app.shows("id:space:side")).toBe(false);

  f.state.spaces.get("side")!.members.add("alice");
  await app.clock!.runFor(11_000);
  await app.waitFor("id:space:side");
  await app.press("id:space:side");
  await app.waitFor("id:space-settings-open");
});

test("a newer exact assignment read protects an existing Space from an older omission", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  f.addSpace("solo", "Solo", "alice");
  f.state.attached.set("solo", r.id);
  const get = f.network[`GET ${BASE}/v1/*`]!;
  let armed = false;
  let parked = false;
  f.network[`GET ${BASE}/v1/*`] = (req) => {
    const response = get(req);
    if (!armed || parked || new URL(req.url).pathname !== "/v1/spaces") return response;
    parked = true;
    const reply = response as { status: number; body: { spaces: { id: string }[] } };
    return { ...reply, body: { ...reply.body, spaces: reply.body.spaces.filter((s) => s.id !== "solo") }, delay: 2400 };
  };
  const app = await launch(fixture(f, r.id), { manual: true });
  await signIn(app);
  await app.waitFor("id:space:solo");
  armed = true;
  await app.clock!.runFor(11_000);
  await app.waitUntil(() => parked);
  await app.press("id:space:solo");
  await openSpaceRunner(app);
  await app.waitFor("id:attached-runner");
  await app.clock!.runFor(2400);
  expect(app.shows("id:space:solo")).toBe(true);
  expect(app.shows("id:attached-runner")).toBe(true);
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

for (const mode of ["create", "bulk"] as const) {
  test(`${mode} attach does not confirm a same-runner replacement revision`, async () => {
    const f = world();
    const r = f.addRunner("This PC", "personal", "alice");
    if (mode === "bulk") f.addSpace("solo", "Solo", "alice");
    const t = fixture(f, r.id);
    t.gate.attach = 800;
    const app = await launch(t, { manual: true });
    await signIn(app);
    if (mode === "create") {
      await startSidebar(app, "Same runner ABA");
      await app.press("id:create-space");
    } else {
      await openRunnerSettings(app);
      await app.waitFor("id:runner-attach-pick:solo");
      await app.press("id:runner-attach-many");
    }
    const sid = () => mode === "bulk" ? "solo" : space(f, "Same runner ABA")?.id;
    await app.waitUntil(() => !!sid() && f.state.attached.get(sid()!) === r.id);
    const path = `${BASE}/v1/spaces/${sid()}/runner`;
    const headers = { authorization: `Bearer ${TOKENS.alice}` };
    await f.network[`DELETE ${BASE}/v1/*`]!({ method: "DELETE", url: path, headers });
    await f.network[`POST ${BASE}/v1/*`]!({ method: "POST", url: `${path}/attach-if-empty`, headers,
      body: JSON.stringify({ runner_id: r.id }) });
    await app.clock!.runFor(800);
    await app.waitFor(mode === "create" ? failure : "id:runner-attach-result");
    expect(words(app)).toContain("The runner assignment changed.");
    expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
    expect(app.shows(success)).toBe(false);
    expect(app.shows(`id:installed-runner-space:${sid()}`)).toBe(false);
  });
}

test("a verified Space settings attach dismisses the offer and records its folder after Runner closes", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  f.addSpace("manual", "Manual", "alice");
  f.addSpace("eligible", "Still eligible", "alice");
  const t = fixture(f, r.id);
  t.gate.attach = 800;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await app.waitFor(offer);
  await app.press("id:space:manual");
  await app.press("id:manage-space");
  await app.press("id:space-settings-tab:runner");
  await app.press(`id:space-runner-attach:${r.id}`);
  await app.waitUntil(() => f.state.attached.get("manual") === r.id);
  await app.press("id:space-settings-close");
  expect(app.shows(offer)).toBe(true);
  expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);

  t.status.delay = 2_000; // Don't let the Runners view's own refresh populate the folder.
  await openRunnerSettings(app);
  expect(app.shows("id:installed-runner-space:manual")).toBe(false);
  await app.clock!.runFor(800);
  await app.waitUntil(() => app.storage()[runnerOfferStorageKey(BASE, "alice")] === "dismissed");
  await app.waitFor("id:installed-runner-space:manual");
  expect(app.shows("id:runners-settings")).toBe(true);
  expect(app.shows("id:space-settings-dialog")).toBe(false);
  expect(app.shows("id:manage-notice")).toBe(false);
  expect(app.shows("id:token-reveal")).toBe(false);
  expect(app.shows(offer)).toBe(false);
  expect(runnerWrites(f).map((p) => p.path)).toEqual(["/v1/spaces/manual/runner/attach-if-empty"]);
});

test("a member-readable or unversioned assignment cannot dismiss the offer or record a folder", async () => {
  for (const dropsRole of [true, false]) {
    const f = world();
    const r = f.addRunner("This PC", "personal", "alice");
    f.addSpace("manual", "Manual", "alice", ["bob"]);
    f.addSpace("eligible", "Still eligible", "alice");
    // Mixed server in the first case: revision support but no token-change flag.
    f.state.guardedTokenChanges = false;
    f.state.guardedDetachSupported = dropsRole;
    const t = fixture(f, r.id);
    t.gate.attach = 800;
    const rule = `GET ${BASE}/v1/*`;
    const get = t.network[rule]!;
    const manual = f.state.spaces.get("manual")!;
    let listSawOwner = false;
    let receivedAssignment = false;
    t.network[rule] = (req) => {
      const path = new URL(req.url).pathname;
      if (path === "/v1/spaces" && manual.owner === "alice") listSawOwner = true;
      if (path === "/v1/spaces/manual/runner" && f.state.attached.get("manual") === r.id) {
        receivedAssignment = true;
        if (dropsRole) manual.owner = "bob"; // Alice still belongs and still sees the same runner.
      }
      return get(req);
    };
    const app = await launch(t, { manual: true });
    await signIn(app);
    await app.waitFor(offer);
    expect(listSawOwner).toBe(true);
    await app.press("id:space:manual");
    await app.press("id:manage-space");
    await app.press("id:space-settings-tab:runner");
    await app.press(`id:space-runner-attach:${r.id}`);
    await app.waitUntil(() => f.state.attached.get("manual") === r.id);
    await app.press("id:space-settings-close");
    t.status.delay = 2_000;
    await openRunnerSettings(app);
    await app.clock!.runFor(800);
    await app.waitUntil(() => receivedAssignment);
    await app.waitFor("id:runner-state >> text:Runner attached");
    expect(manual.owner).toBe(dropsRole ? "bob" : "alice");
    expect(manual.members.has("alice")).toBe(true);
    expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
    expect(app.shows(offer)).toBe(true);
    expect(app.shows("id:installed-runner-space:manual")).toBe(false);
    expect(f.state.attached.get("manual")).toBe(r.id);
    expect(runnerWrites(f).map((p) => p.path)).toEqual(["/v1/spaces/manual/runner/attach-if-empty"]);
  }
});

test("detach and reattach of the same runner supersedes a held Space settings receipt", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  f.addSpace("manual", "Manual", "alice");
  f.addSpace("eligible", "Still eligible", "alice");
  const t = fixture(f, r.id);
  t.gate.attach = 800;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await app.waitFor(offer);
  await app.press("id:space:manual");
  await app.press("id:manage-space");
  await app.press("id:space-settings-tab:runner");
  await app.press(`id:space-runner-attach:${r.id}`);
  await app.waitUntil(() => f.state.attached.get("manual") === r.id);
  // Another client changes the assignment, without moving this app's write marker.
  const headers = { authorization: `Bearer ${TOKENS.alice}` };
  const path = "/v1/spaces/manual/runner";
  await f.network[`DELETE ${BASE}/v1/*`]!({ method: "DELETE", url: BASE + path, headers });
  await f.network[`POST ${BASE}/v1/*`]!({ method: "POST", url: BASE + path + "/attach-if-empty", headers,
    body: JSON.stringify({ runner_id: r.id }) });
  expect(f.state.attached.get("manual")).toBe(r.id);
  await app.clock!.runFor(800);
  await app.waitFor("id:manage-error");
  expect(app.shows("id:manage-notice")).toBe(false);
  await app.waitFor("id:space-runner-status >> text:Attached · This PC");
  expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
  expect(app.shows(offer)).toBe(true);
  expect(runnerWrites(f)).toHaveLength(3);
  await app.press("id:space-settings-close");
  t.status.delay = 2_000;
  await openRunnerSettings(app);
  expect(app.shows("id:installed-runner-space:manual")).toBe(false);
});

test("a superseded Space settings attach neither dismisses the offer nor records a folder", async () => {
  const f = world();
  const r = f.addRunner("This PC", "personal", "alice");
  f.addSpace("manual", "Manual", "alice");
  const t = fixture(f, r.id);
  t.gate.attach = 800;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await app.waitFor(offer);
  await app.press("id:space:manual");
  await openSpaceRunner(app);
  await app.press(`id:space-runner-attach:${r.id}`);
  await app.waitUntil(() => f.state.attached.get("manual") === r.id);
  await app.press("id:space-settings-close");

  await openSpaceRunner(app);
  await app.press("id:space-runner-detach");
  await app.press("id:space-runner-detach:confirm");
  await app.waitFor("id:space-runner-status >> text:No runner");
  await app.clock!.runFor(800);
  await app.press("id:space-settings-close");
  expect(app.shows(offer)).toBe(true);
  expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
  expect(runnerWrites(f).map((p) => p.path)).toEqual([
    "/v1/spaces/manual/runner/attach-if-empty", "/v1/spaces/manual/runner/detach-if-current",
  ]);

  t.status.delay = 1_000;
  await openRunnerSettings(app);
  expect(app.shows("id:installed-runner-space:manual")).toBe(false);
  await app.clock!.runFor(1_000);
  await app.waitFor("id:runner-attach-pick:manual");
});

test("an older folder snapshot preserves newer presence or absence for A and still adds and removes fresh B rows", async () => {
  for (const final of ["detached", "reassigned", "attached"]) {
    const f = world();
    f.addSpace("solo", "Solo", "alice");
    f.addSpace("keep", "Keep folder", "alice");
    f.addSpace("added", "New folder", "alice");
    f.addSpace("removed", "Old folder", "alice");
    const r = f.addRunner("This PC", "personal", "alice");
    const other = f.addRunner("Other computer", "personal", "alice");
    const attaches = final === "attached";
    const expectedRunner = attaches ? r.id : final === "reassigned" ? other.id : undefined;
    f.state.attached.set("solo", r.id);
    f.state.attached.set("keep", r.id);
    f.state.attached.set("removed", r.id);
    const t = fixture(f, r.id);
    t.gate.attach = 900;
    const app = await launch(t, { manual: true });
    await signIn(app);
    await openRunnerSettings(app);
    await app.waitFor("id:installed-runner-space:solo");
    expect(app.shows("id:installed-runner-space:keep")).toBe(true);
    expect(app.shows("id:installed-runner-space:added")).toBe(false);
    expect(app.shows("id:installed-runner-space:removed")).toBe(true);
    // The normal Space list can see an external detach before the installed
    // runner's next full folder snapshot has replaced its cached row.
    f.state.attached.delete("solo");
    await app.clock!.runFor(11_000);
    await app.waitFor("id:runner-attach-pick:solo");
    expect(app.shows("id:installed-runner-space:solo")).toBe(true);
    await app.press("id:settings-close");
    await openWithHeldFolderStatus(app, t, 300);
    await app.press("id:runner-attach-pick:added"); // Not part of this batch.
    await app.press("id:runner-attach-many");
    await app.waitUntil(() => f.state.attached.get("solo") === r.id);

    const folderRead = { sid: "solo", delay: 1000, parked: false };
    t.gate.runnerRead = folderRead;
    f.state.attached.set("added", r.id);
    f.state.attached.delete("removed");
    if (attaches) f.state.attached.delete("solo"); // The held snapshot sees absence.
    await app.clock!.runFor(300);
    await app.waitUntil(() => folderRead.parked);
    expect(app.shows("id:installed-runner-space:solo")).toBe(true);

    if (expectedRunner) f.state.attached.set("solo", expectedRunner);
    else f.state.attached.delete("solo");

    await app.clock!.runFor(600);
    await app.waitFor("id:runner-attach-result");
    if (attaches) expect(words(app)).toContain("Attached to 1 Space.");
    else {
      expect(words(app)).toContain("Attached to 0 of 1 Space. Solo: The runner assignment changed.");
      expect(words(app)).toContain("Check this Space before retrying.");
    }
    expect(f.state.attached.get("solo")).toBe(expectedRunner);
    expect(app.shows("id:runner-attach-pick:solo")).toBe(final === "detached"); // Authoritative paired flag.
    expect(app.shows("id:installed-runner-space:solo")).toBe(attaches);
    await app.clock!.runFor(400); // Merge the other rows without rolling back A.
    expect(app.shows("id:installed-runner-space:solo")).toBe(attaches);
    expect(app.shows("id:installed-runner-space:keep")).toBe(true);
    expect(app.shows("id:installed-runner-space:added")).toBe(true);
    expect(app.shows("id:installed-runner-space:removed")).toBe(false);
    expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(attaches ? "dismissed" : undefined);
    expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([["POST", "/v1/spaces/solo/runner/attach-if-empty"]]);

    if (final === "reassigned") {
      // The Space badge must still reflect the newer assignment while its
      // Runner page waits for its own read before enabling any actions.
      f.answerLate("/v1/spaces/solo/runner", TOKENS.alice!, 1200);
      await app.press("id:settings-close");
      await app.press("id:space:solo");
      await openSpaceRunner(app);
      await app.waitFor("id:runner-state >> text:Runner attached");
      expect(app.shows("id:attached-runner")).toBe(false);
      await app.clock!.runFor(1200);
      await app.waitFor("id:attached-runner");
      expect(words(app)).toContain(other.name);
    }
  }
});

test("a newer folder observation for A wins over a delayed reconciliation without confirming the old batch", async () => {
  for (const older of ["absent", "attached", "denied"]) {
    const oldMatch = older === "attached";
    const f = world();
    f.addSpace("solo", "Solo", "alice");
    f.addSpace("marker", "Snapshot marker", "alice");
    const r = f.addRunner("This PC", "personal", "alice");
    const other = f.addRunner("Other computer", "personal", "alice");
    f.state.attached.set("marker", other.id);
    const t = fixture(f, r.id);
    t.gate.attach = 500;
    const app = await launch(t, { manual: true });
    await signIn(app);
    await app.waitFor(offer);
    await openWithHeldFolderStatus(app, t, 800);
    await app.waitFor("id:runner-attach-pick:solo");
    await app.press("id:runner-attach-many");
    await app.waitUntil(() => f.state.attached.get("solo") === r.id);
    if (!oldMatch) f.state.attached.delete("solo");
    const reconciliation = { sid: "solo", delay: 1000, parked: false, ...(older === "denied" ? { errorStatus: 403 } : {}) };
    t.gate.runnerRead = reconciliation;
    await app.clock!.runFor(500);
    await app.waitUntil(() => reconciliation.parked);

    if (oldMatch) f.state.attached.delete("solo");
    else f.state.attached.set("solo", r.id);
    f.state.attached.set("marker", r.id);
    await app.clock!.runFor(300);
    await app.waitFor("id:installed-runner-space:marker"); // The newer snapshot has merged.
    await app.waitFor(oldMatch ? "id:runner-attach-pick:solo" : "id:installed-runner-space:solo");

    await app.clock!.runFor(700);
    await app.waitFor("id:runner-attach-result");
    expect(words(app)).toContain("Attached to 0 of 1 Space. Solo: The attachment wasn't confirmed.");
    expect(app.shows("id:installed-runner-space:solo")).toBe(!oldMatch);
    expect(app.shows("id:runner-attach-pick:solo")).toBe(oldMatch);
    expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
    expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([["POST", "/v1/spaces/solo/runner/attach-if-empty"]]);

    // The badge uses the shared pairing state; the new page still waits for
    // its own read before showing a guarded action.
    f.answerLate("/v1/spaces/solo/runner", TOKENS.alice!, 1200);
    await app.press("id:settings-close");
    await app.press("id:space:solo");
    await openSpaceRunner(app);
    await app.waitFor(oldMatch ? "id:runner-state >> text:No runner" : "id:runner-state >> text:Runner attached");
    await app.clock!.runFor(1200);
    await app.waitFor(oldMatch ? `id:space-runner-attach:${r.id}` : "id:attached-runner");
    expect(app.shows("id:attached-runner")).toBe(!oldMatch);
  }
});

test("batch consent doesn't survive a local attach and detach while the first item or second preflight is held", async () => {
  for (const holdFirst of [true, false]) {
    const f = world();
    f.addSpace("first", "First", "alice");
    f.addSpace("second", "Second", "alice");
    const r = f.addRunner("This PC", "personal", "alice");
    const other = f.addRunner("Other computer", "personal", "alice");
    const t = fixture(f, r.id);
    const app = await launch(t, { manual: true });
    await signIn(app);
    await openRunnerSettings(app);
    await app.waitFor("id:runner-attach-pick:second");
    await app.waitUntil(() => f.state.log.some((p) => p.method === "GET" && p.path === "/v1/spaces/second/runner"));
    const preflight = { sid: "second", delay: 800, parked: false };
    if (holdFirst) {
      t.gate.attach = 800;
      t.gate.attachSpace = "first";
    }
    else t.gate.runnerRead = preflight;
    await app.press("id:runner-attach-many");
    await app.waitUntil(() => f.state.attached.get("first") === r.id && (holdFirst || preflight.parked));
    await app.press("id:settings-close");
    await app.press("id:space:second");
    await openSpaceRunner(app);
    await app.waitFor(`id:space-runner-attach:${other.id}`);
    await app.press(`id:space-runner-attach:${other.id}`);
    await app.waitFor("id:attached-runner");
    expect(f.state.attached.get("second")).toBe(other.id);
    await app.press("id:space-runner-detach");
    await app.press("id:space-runner-detach:confirm");
    await app.waitFor(`id:space-runner-attach:${other.id}`);
    expect(f.state.attached.has("second")).toBe(false);

    await app.clock!.runFor(800);
    await app.press("id:space-settings-close");
    await openRunnerSettings(app);
    await app.waitFor("id:runner-attach-result");
    expect(words(app)).toContain("Attached to 1 of 2 Spaces. Second: A newer runner action changed this Space");
    expect(app.shows("id:installed-runner-space:first")).toBe(true);
    expect(app.shows("id:installed-runner-space:second")).toBe(false);
    expect(app.shows("id:runner-attach-pick:second")).toBe(true);
    expect(f.state.attached.has("second")).toBe(false);
    await app.waitUntil(() => app.storage()[runnerOfferStorageKey(BASE, "alice")] === "dismissed");
    expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([
      ["POST", "/v1/spaces/first/runner/attach-if-empty"],
      ["POST", "/v1/spaces/second/runner/attach-if-empty"], // Only the explicit Space settings action.
      ["POST", "/v1/spaces/second/runner/detach-if-current"],
    ]);
  }
});

test("a newer local Manage detach wins over a held bulk success, including count and folders", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const r = f.addRunner("This PC", "personal", "alice");
  const t = fixture(f, r.id);
  t.gate.attach = 800;
  const app = await launch(t, { manual: true });
  await signIn(app);
  await openRunnerSettings(app);
  await app.waitFor("id:runner-attach-pick:solo");
  await app.press("id:runner-attach-many");
  await app.waitUntil(() => f.state.attached.get("solo") === r.id);
  await app.press("id:settings-close");
  await app.press("id:space:solo");
  await openSpaceRunner(app);
  await app.waitFor("id:attached-runner");
  await app.press("id:space-runner-detach");
  await app.press("id:space-runner-detach:confirm");
  await app.waitFor(`id:space-runner-attach:${r.id}`);
  await app.clock!.runFor(800);
  expect(app.shows("id:attached-runner")).toBe(false);
  expect(app.shows("id:test-runner-row")).toBe(false);
  await app.press("id:space-settings-close");
  await openRunnerSettings(app);
  await app.waitFor("id:runner-attach-result");
  expect(words(app)).toContain("Attached to 0 of 1 Space. Solo: A newer runner action superseded this attachment check.");
  expect(app.shows("id:runner-attach-pick:solo")).toBe(true);
  expect(app.shows("id:installed-runner-space:solo")).toBe(false);
  expect(f.state.attached.has("solo")).toBe(false);
  expect(app.storage()[runnerOfferStorageKey(BASE, "alice")]).toBe(undefined);
  expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([
    ["POST", "/v1/spaces/solo/runner/attach-if-empty"], ["POST", "/v1/spaces/solo/runner/detach-if-current"],
  ]);
});

test("a current Manage detach fences a pre-commit folder read without losing B or a newer post-ack observation", async () => {
  const f = world();
  f.addSpace("alpha", "Alpha", "alice");
  f.addSpace("beta", "Beta", "alice");
  const r = f.addRunner("This PC", "personal", "alice");
  f.state.attached.set("alpha", r.id);
  const t = fixture(f, r.id);
  const app = await launch(t, { manual: true });
  await signIn(app);
  await app.waitFor(offer);
  await openRunnerSettings(app);
  await app.waitFor("id:installed-runner-space:alpha");
  await app.press("id:settings-close");
  await app.press("id:space:alpha");
  await openSpaceRunner(app);
  await app.waitFor("id:attached-runner");
  const detach = { sid: "alpha", beforeCommit: true, delay: 400, commit: null as (() => void) | null };
  t.gate.detach = detach;
  await app.press("id:space-runner-detach");
  await app.press("id:space-runner-detach:confirm");
  await app.waitUntil(() => detach.commit !== null);
  expect(f.state.attached.get("alpha")).toBe(r.id); // The guarded detach hasn't committed.

  const preCommit = { sid: "alpha", delay: 800, parked: false };
  t.gate.runnerRead = preCommit;
  f.state.attached.set("beta", r.id);
  await app.press("id:space-settings-close");
  await openRunnerSettings(app);
  await app.waitUntil(() => preCommit.parked);
  expect(app.shows("id:installed-runner-space:alpha")).toBe(true);
  const afterAck = { sid: "alpha", delay: 1600, parked: false };
  t.gate.runnerRead = afterAck;
  detach.commit!();
  await app.clock!.runFor(400);
  await app.waitUntil(() => afterAck.parked); // Fresh exact-Space read after the ack.
  expect(f.state.attached.has("alpha")).toBe(false);

  await app.clock!.runFor(400); // The pre-commit snapshot returns before the fresh read.
  await app.waitFor("id:installed-runner-space:beta");
  expect(app.shows("id:installed-runner-space:alpha")).toBe(false);
  // A historical ack cannot make the Space attachable before a fresh read.
  expect(app.shows("id:runner-attach-pick:alpha")).toBe(false);
  await app.press("id:settings-close");
  expect(app.shows("id:attached-runner")).toBe(false);
  expect(app.shows("id:test-runner-row")).toBe(false);

  // A newer observation really can win: another client attaches A again
  // after the ack, while the first post-ack read is still in flight.
  f.state.attached.set("alpha", r.id);
  await openRunnerSettings(app);
  await app.waitFor("id:installed-runner-space:alpha");
  await app.clock!.runFor(1200);
  expect(app.shows("id:installed-runner-space:alpha")).toBe(true);
  expect(app.shows("id:installed-runner-space:beta")).toBe(true);
  expect(app.shows("id:runner-attach-pick:alpha")).toBe(false);
  await app.press("id:settings-close");
  await app.waitFor("id:runner-state >> text:Runner attached");
  expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([["POST", "/v1/spaces/alpha/runner/detach-if-current"]]);
});

test("a Manage detach ack superseded by a verified bulk attach doesn't publish or re-read", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  f.addSpace("marker", "Snapshot marker", "alice");
  const r = f.addRunner("This PC", "personal", "alice");
  f.state.attached.set("solo", r.id);
  const t = fixture(f, r.id);
  const app = await launch(t, { manual: true });
  await signIn(app);
  await app.waitFor(offer);
  await app.press("id:space:solo");
  await openSpaceRunner(app);
  await app.waitFor("id:attached-runner");
  const detach = { sid: "solo", beforeCommit: false, delay: 800, commit: null as (() => void) | null };
  t.gate.detach = detach;
  await app.press("id:space-runner-detach");
  await app.press("id:space-runner-detach:confirm");
  await app.waitUntil(() => detach.commit !== null);
  expect(f.state.attached.has("solo")).toBe(false);

  // Make a sibling folder mark completion of this Settings snapshot.
  f.state.attached.set("marker", r.id);
  await app.press("id:space-settings-close");
  await openRunnerSettings(app);
  await app.waitFor("id:installed-runner-space:marker");
  await app.waitFor("id:runner-attach-pick:solo");
  await app.press("id:runner-attach-many");
  await app.waitFor("id:runner-attach-result");
  expect(words(app)).toContain("Attached to 1 Space.");
  expect(app.shows("id:installed-runner-space:solo")).toBe(true);
  const reads = () => f.state.log.filter((p) => p.method === "GET" && p.path === "/v1/spaces/solo/runner").length;
  const before = reads();

  await app.clock!.runFor(800);
  expect(app.shows("id:installed-runner-space:solo")).toBe(true);
  expect(reads()).toBe(before);
  await app.press("id:settings-close");
  await app.waitFor("id:runner-state >> text:Runner attached");
  expect(runnerWrites(f).map((p) => [p.method, p.path])).toEqual([
    ["POST", "/v1/spaces/solo/runner/detach-if-current"], ["POST", "/v1/spaces/solo/runner/attach-if-empty"],
  ]);
});

test("a Manage detach acknowledgement after an account change doesn't re-read the old Space", async () => {
  const f = world();
  f.addSpace("solo", "Solo", "alice");
  const r = f.addRunner("This PC", "personal", "alice");
  f.state.attached.set("solo", r.id);
  const t = fixture(f, r.id);
  const app = await launch(t, { manual: true });
  await signIn(app);
  await app.press("id:space:solo");
  await openSpaceRunner(app);
  await app.waitFor("id:attached-runner");
  const detach = { sid: "solo", beforeCommit: false, delay: 800, commit: null as (() => void) | null };
  t.gate.detach = detach;
  await app.press("id:space-runner-detach");
  await app.press("id:space-runner-detach:confirm");
  await app.waitUntil(() => detach.commit !== null);
  expect(f.state.attached.has("solo")).toBe(false);
  const reads = () => f.state.log.filter((p) => p.method === "GET" && p.path === "/v1/spaces/solo/runner").length;
  const before = reads();
  await app.press("id:space-settings-close");
  await signOut(app);
  await signIn(app, "bob");
  await app.clock!.runFor(800);
  expect(reads()).toBe(before);
  expect(app.shows("id:space:solo")).toBe(false);
  expect(words(app)).not.toContain("Runner detached.");
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
  expect(words(app)).toContain("The Create Space choice or context changed");
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

test("Settings reopen joins held Sidebar liveness and refreshes folders after an assignment changes on another client", async () => {
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
  // Another client attaches the runner. A successful local attach dismisses
  // this launch's offer, which would stop the Sidebar liveness watch.
  f.state.attached.set("manual", r.id);
  t.status.delay = 800;
  const reopenDuringLiveness = async () => {
    const before = t.status.reads;
    await app.clock!.runFor(5000);
    expect(t.status.reads).toBe(before + 1);
    await openRunnerSettings(app);
    expect(t.status.reads).toBe(before + 1); // Settings joins; no second bridge request.
    await app.clock!.runFor(800);
  };
  await reopenDuringLiveness();
  await app.waitFor("id:installed-runner-space:manual");
  await app.press("id:settings-close");
  f.state.attached.delete("manual");
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
