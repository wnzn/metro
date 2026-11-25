// Settings › Monitoring end to end in the app: the real NSH engine against the
// in-memory fake server (test/fakeServer.ts) and, for this computer, the fake
// desktop bridge (src/device/test/fakeBridge.ts). No network, fake values only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { BRIDGE, BRIDGE_TOKEN, bridgeEnv, fakeBridge } from "../device/test/fakeBridge";
import { launched } from "../../test/launched";

type F = ReturnType<typeof world>;
type B = ReturnType<typeof fakeBridge>;

// The stub reply is held on app time, not wall time, and comes before the
// transport's timeout. Take the intervening actions before advancing it.
const LATE_REPLY = 5_000;
const BEFORE_REPLY = { timeout: 500 };
const ACME_EVENTS = "/v1/monitoring/events?space_id=acme";

const at = (minute: number) => `2026-11-23T10:${String(minute).padStart(2, "0")}:00Z`;
const event = (minute: number, component: string, code: string, summary: string) => ({ timestamp: at(minute), component, code, summary });

/** Alice owns Acme and is an org admin; Bob owns Side; Carol owns nothing. Each source (and each person's Chat) has its own events. */
function monitoringWorld() {
  const f = world();
  f.state.monitoring.spaces.set("acme", [event(5, "runner", "runner_offline", "Acme runner went offline"), event(1, "agent", "run_failed", "Acme run failed")]);
  f.state.monitoring.spaces.set("side", [event(4, "runner", "claim_retry", "Side claim retried")]);
  f.state.monitoring.instance = [event(9, "server", "db_slow", "Instance database slow")];
  f.state.monitoring.users.set("alice", [event(3, "agent", "chat_failed", "Alice chat turn failed")]);
  f.state.monitoring.users.set("bob", [event(2, "agent", "chat_failed", "Bob chat turn failed")]);
  return f;
}

async function launch(f: F, b: B | null = null) {
  const network = { ...(f.network as any), ...(b ? (b.network as any) : {}) };
  return open("../../App.tsx", { network, platform: "linux", width: 1200, height: 900, clock: {}, modules: (b ? bridgeEnv() : launched()) as any });
}

async function signIn(app: App, user: string) {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-settings");
}

async function openMonitoring(app: App) {
  if (!app.shows("id:nav-settings:monitoring")) await app.press("id:nav-settings");
  await app.press("id:nav-settings:monitoring");
  await app.waitFor("id:monitoring-settings");
}

async function signOut(app: App) {
  if (app.shows("id:settings-close")) await app.press("id:settings-close");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect");
}

async function pick(app: App, key: string) {
  await app.press("id:monitoring-source");
  await app.press(`id:monitoring-source:${key}`);
}

const text = (app: App) => app.texts().join(" | ");
/** How many event rows are shown (rows are numbered from 0). */
const rows = (app: App) => {
  let n = 0;
  while (app.shows(`id:monitoring-event:${n}`)) n++;
  return n;
};
const monitoringCalls = (f: F, user?: string) => f.state.log.filter((l) => l.path.startsWith("/v1/monitoring/") && (!user || l.user === user)).map((l) => l.path);

async function waitForHeldReply(app: App, reply: { arrived: () => number }) {
  await app.waitUntil(() => reply.arrived() === 1, BEFORE_REPLY);
  await app.waitFor("id:monitoring-loading", BEFORE_REPLY);
}

test("an owner sees one owned Space's events at a time, with the server named; an admin also gets the instance", async () => {
  const f = monitoringWorld();
  const app = await launch(f);
  await signIn(app, "alice");
  await openMonitoring(app);
  await app.waitFor("text:Alice chat turn failed"); // My Chat comes first without a packaged helper
  await pick(app, "space:acme");
  await app.waitFor("text:Acme runner went offline");
  // Acme's events only, and which server they came from.
  expect(app.shows("id:monitoring-source-facts >> text:Space “Acme”")).toBe(true);
  expect(app.shows(`id:monitoring-source-facts >> text:${BASE}`)).toBe(true);
  expect(app.shows("text:2026-11-23 10:05:00 UTC")).toBe(true);
  expect(rows(app)).toBe(2);
  expect(text(app)).not.toContain("Instance database slow");
  expect(text(app)).not.toContain("Side claim retried");
  expect(text(app)).not.toContain("Alice chat turn failed");

  // Offered: the Space she owns and the instance. Side (she's only a member) never is.
  await app.press("id:monitoring-source");
  expect(app.shows("id:monitoring-source:space:acme")).toBe(true);
  expect(app.shows("id:monitoring-source:instance")).toBe(true);
  expect(app.shows("id:monitoring-source:space:side")).toBe(false);
  expect(app.shows("id:monitoring-source:device")).toBe(false); // no packaged helper here
  await app.press("id:monitoring-source:instance");
  await app.waitFor("text:Instance database slow");
  // The instance alone: nothing of Acme stays on screen.
  expect(text(app)).not.toContain("Acme runner went offline");
  expect(rows(app)).toBe(1);
  expect(app.shows("id:monitoring-source-facts >> text:This server's instance")).toBe(true);

  // Every request named exactly one scope: never a personal or all-Spaces aggregate.
  const calls = monitoringCalls(f, "alice");
  expect(calls).toContain("/v1/monitoring/events?space_id=acme");
  expect(calls).toContain("/v1/monitoring/events");
  expect(calls).toContain("/v1/monitoring/events?scope=me");
  for (const c of calls) expect(["/v1/monitoring/events", "/v1/monitoring/events?space_id=acme", "/v1/monitoring/events?scope=me"]).toContain(c);
});

test("a non-admin owner gets My Chat and only their own Spaces; someone owning nothing gets only My Chat", async () => {
  const f = monitoringWorld();
  const app = await launch(f);
  await signIn(app, "bob");
  await openMonitoring(app);
  await app.waitFor("text:Bob chat turn failed");
  await app.press("id:monitoring-source");
  expect(app.shows("id:monitoring-source:me")).toBe(true);
  expect(app.shows("id:monitoring-source:space:side")).toBe(true);
  expect(app.shows("id:monitoring-source:space:acme")).toBe(false);
  expect(app.shows("id:monitoring-source:instance")).toBe(false);
  await app.press("id:monitoring-source:space:side");
  await app.waitFor("text:Side claim retried");
  expect(monitoringCalls(f, "bob")).toEqual(["/v1/monitoring/events?scope=me", "/v1/monitoring/events?space_id=side"]);
  await signOut(app);

  await signIn(app, "carol");
  await openMonitoring(app);
  await app.waitFor("id:monitoring-empty"); // her Chat has no events
  await app.press("id:monitoring-source");
  expect(app.shows("id:monitoring-source:me")).toBe(true);
  expect(app.shows("id:monitoring-source:space:side")).toBe(false);
  expect(app.shows("id:monitoring-source:instance")).toBe(false);
  await app.press("id:monitoring-source:me");
  expect(monitoringCalls(f, "carol")).toEqual(["/v1/monitoring/events?scope=me"]);
});

test("My Chat is the signed-in account's own, from the server, named apart from this computer; no user id is sent", async () => {
  const f = monitoringWorld();
  const b = fakeBridge(f);
  b.state.monitoring = [event(7, "connector", "worker_restarted", "Local worker restarted")];
  const app = await launch(f, b);
  await signIn(app, "alice");
  await openMonitoring(app);
  await app.waitFor("text:Local worker restarted"); // this computer comes first with a packaged helper
  await pick(app, "me");
  await app.waitFor("text:Alice chat turn failed");
  expect(app.shows("id:monitoring-source-facts >> text:My Chat: your personal Chat, only you")).toBe(true);
  expect(app.shows(`id:monitoring-source-facts >> text:${BASE}`)).toBe(true);
  expect(app.shows("id:monitoring-source-facts >> text:alice")).toBe(true);
  expect(text(app)).not.toContain("Local worker restarted");
  expect(text(app)).not.toContain("Bob chat turn failed");
  // Alice is an admin, yet the request names no one: the server reads whose from her session.
  for (const c of monitoringCalls(f)) {
    expect(c).not.toContain("user");
    expect(c).not.toContain("alice");
    expect(c).not.toContain("bob");
  }
  expect(monitoringCalls(f, "alice")).toContain("/v1/monitoring/events?scope=me");
});

test("an older server without My Chat (400 or 404) is unsupported and never falls back to the instance", async () => {
  for (const mode of ["400", "404"] as const) {
    const f = monitoringWorld();
    if (mode === "400") f.state.monitoring.noMe = true;
    else f.state.monitoring.supported = false;
    const app = await launch(f);
    await signIn(app, "alice");
    await openMonitoring(app);
    await app.waitFor("id:monitoring-unsupported");
    expect(text(app)).toContain(`${BASE} doesn't report your Chat's monitoring events. It may run an older Metro version.`);
    await app.wait(300);
    expect(monitoringCalls(f, "alice")).toEqual(["/v1/monitoring/events?scope=me"]);
    expect(text(app)).not.toContain("Instance database slow");
    expect(rows(app)).toBe(0);
    if (mode === "400") {
      // Space and instance events still work on that server.
      await pick(app, "space:acme");
      await app.waitFor("text:Acme runner went offline");
      await pick(app, "instance");
      await app.waitFor("text:Instance database slow");
    }
    app.dispose();
  }
});

test("a late My Chat answer of the previous account never shows for the next one", async () => {
  const f = monitoringWorld();
  const reply = f.answerLate("/v1/monitoring/events?scope=me", TOKENS.alice!, LATE_REPLY);
  const app = await launch(f);
  await signIn(app, "alice");
  await openMonitoring(app);
  await waitForHeldReply(app, reply);
  await signOut(app);
  await signIn(app, "bob");
  await openMonitoring(app);
  await app.waitFor("text:Bob chat turn failed", BEFORE_REPLY);
  expect(text(app)).not.toContain("Alice chat turn failed");
  await app.clock!.runFor(LATE_REPLY); // now let Alice's reply come due in Bob's session
  expect(text(app)).not.toContain("Alice chat turn failed");
  expect(app.shows("id:monitoring-source-facts >> text:bob")).toBe(true);
  expect(rows(app)).toBe(1);
});

test("an older server without monitoring shows a useful unsupported state", async () => {
  const f = monitoringWorld();
  f.state.monitoring.supported = false;
  const app = await launch(f);
  await signIn(app, "alice");
  await openMonitoring(app);
  await app.waitFor("id:monitoring-unsupported");
  await pick(app, "space:acme");
  await app.waitFor(`text:/doesn't report monitoring events for “Acme”/`);
  expect(text(app)).toContain(`${BASE} doesn't report monitoring events for “Acme”. It may run an older Metro version, or the Space is no longer yours.`);
  await pick(app, "instance");
  await app.waitFor(`text:${BASE} doesn't report instance monitoring events. It may run an older Metro version.`);
  expect(rows(app)).toBe(0);
});

test("a rejected session (401) signs out instead of showing events", async () => {
  const f = monitoringWorld();
  const app = await launch(f);
  await signIn(app, "alice");
  await app.press("id:nav-settings");
  await app.waitFor("id:nav-settings:monitoring");
  f.state.monitoring.unauthorized = true;
  await app.press("id:nav-settings:monitoring");
  await app.waitFor("id:connect");
  expect(text(app)).toContain("HTTP 401");
  expect(text(app)).not.toContain("Acme runner went offline");
});

test("a late answer for the source left behind never shows under the new one", async () => {
  const f = monitoringWorld();
  const reply = f.answerLate(ACME_EVENTS, TOKENS.alice!, LATE_REPLY);
  const app = await launch(f);
  await signIn(app, "alice");
  await openMonitoring(app);
  await app.waitFor("text:Alice chat turn failed");
  await pick(app, "space:acme");
  await waitForHeldReply(app, reply);
  await pick(app, "instance");
  await app.waitFor("text:Instance database slow", BEFORE_REPLY);
  await app.clock!.runFor(LATE_REPLY); // now deliver Acme's answer
  expect(text(app)).not.toContain("Acme runner went offline");
  expect(text(app)).not.toContain("Acme run failed");
  expect(rows(app)).toBe(1);
});

test("a late answer from the last session never shows after sign-out and another sign-in", async () => {
  const f = monitoringWorld();
  const reply = f.answerLate(ACME_EVENTS, TOKENS.alice!, LATE_REPLY);
  const app = await launch(f);
  await signIn(app, "alice");
  await openMonitoring(app);
  await app.waitFor("text:Alice chat turn failed");
  await pick(app, "space:acme");
  await waitForHeldReply(app, reply);
  await signOut(app);
  await signIn(app, "bob");
  await openMonitoring(app);
  await app.waitFor("text:Bob chat turn failed", BEFORE_REPLY);
  await pick(app, "space:side");
  await app.waitFor("text:Side claim retried", BEFORE_REPLY);
  await app.clock!.runFor(LATE_REPLY);
  expect(text(app)).not.toContain("Acme runner went offline");
  expect(text(app)).not.toContain("Instance database slow");
  expect(app.shows("id:monitoring-source-facts >> text:Space “Side project”")).toBe(true);
});

test("losing ownership of a Space drops it as a source", async () => {
  const f = monitoringWorld();
  const app = await launch(f);
  await signIn(app, "bob");
  await openMonitoring(app);
  await app.waitFor("text:Bob chat turn failed");
  await pick(app, "space:side");
  await app.waitFor("text:Side claim retried");
  f.state.spaces.get("side")!.owner = "alice"; // ownership moved on the server
  await app.waitFor("text:Bob chat turn failed", { timeout: 15_000 }); // Space list refreshes; back to My Chat
  expect(text(app)).not.toContain("Side claim retried");
  await app.press("id:monitoring-source");
  expect(app.shows("id:monitoring-source:space:side")).toBe(false);
});

test("long lists are capped and unreadable entries are counted, not shown", async () => {
  const f = monitoringWorld();
  f.state.monitoring.spaces.set("acme", [
    { timestamp: "not a time", component: "x", code: "y", summary: "bad entry" },
    ...Array.from({ length: 230 }, (_, i) => event(i % 60, "agent", "tick", `tick ${i}`)),
  ]);
  const app = await launch(f);
  await signIn(app, "alice");
  await openMonitoring(app);
  await pick(app, "space:acme");
  await app.waitFor("id:monitoring-truncated");
  expect(text(app)).toContain("Showing the newest 200 events.");
  expect(text(app)).toContain("1 entry couldn't be read and isn't shown.");
  expect(text(app)).not.toContain("bad entry");
});

test("this computer's events come from the authenticated bridge, apart from any server's", async () => {
  const f = monitoringWorld();
  const b = fakeBridge(f);
  b.state.monitoring = [event(7, "connector", "worker_restarted", "Local worker restarted")];
  const app = await launch(f, b);
  await signIn(app, "alice");
  await openMonitoring(app);
  await app.waitFor("text:Local worker restarted");
  expect(app.shows("id:monitoring-source-facts >> text:This computer's Metro helper")).toBe(true);
  expect(app.shows(`id:monitoring-source-facts >> text:${BASE}`)).toBe(false);
  expect(text(app)).not.toContain("Acme runner went offline");
  const calls = b.calls("/v1/monitoring/events", "GET");
  expect(calls.length).toBeGreaterThan(0);
  for (const c of calls) expect(c.headers).toBeTruthy();
  expect(monitoringCalls(f)).toEqual([]); // nothing asked of the server for this computer

  // Switching to a Space shows only that Space's, from the server.
  await pick(app, "space:acme");
  await app.waitFor("text:Acme runner went offline");
  expect(text(app)).not.toContain("Local worker restarted");
  expect(text(app)).not.toContain(BRIDGE_TOKEN);
  expect(text(app)).not.toContain(BRIDGE);
});

test("an older helper is unsupported; a refused bridge bearer is an error; a late bridge answer is dropped", async () => {
  const f = monitoringWorld();
  const b = fakeBridge(f);
  const app = await launch(f, b);
  await signIn(app, "alice");
  await openMonitoring(app);
  await app.waitFor("id:monitoring-unsupported");
  expect(text(app)).toContain("This computer's Metro helper doesn't report monitoring events. It may be an older version.");

  b.state.failWith = { status: 401, code: "unauthorized", message: "Unauthorized." };
  await app.press("id:monitoring-refresh");
  await app.waitFor("id:monitoring-error");
  expect(text(app)).toContain("Metro's device helper didn't accept this window. Quit and reopen Metro, then try again.");
  // The bridge refusing its bearer never signs the user out of the server.
  expect(app.shows("id:nav-settings:monitoring")).toBe(true);

  b.state.failWith = null;
  b.state.monitoring = [event(8, "connector", "late", "Late local event")];
  const calls = () => b.calls("/v1/monitoring/events", "GET").length;
  const before = calls();
  b.state.monitoringDelay = LATE_REPLY;
  await app.press("id:monitoring-retry");
  await app.waitUntil(() => calls() === before + 1, BEFORE_REPLY);
  await app.waitFor("id:monitoring-loading", BEFORE_REPLY);
  await pick(app, "space:acme");
  await app.waitFor("text:Acme runner went offline", BEFORE_REPLY);
  await app.clock!.runFor(LATE_REPLY);
  expect(text(app)).not.toContain("Late local event");
  expect(text(app)).toContain("Acme runner went offline");
});
