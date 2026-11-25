import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BRIDGE, bridgeEnv, fakeBridge } from "./src/device/test/fakeBridge";
import { BASE, TOKENS, world } from "./test/fakeServer";

const statusPath = `${BRIDGE}/v1/update`;
const webPath = `${BRIDGE}/v1/web-search`;
const chatPath = `${BASE}/v1/web-search/prefs`;
const serverPath = `${BASE}/v1/server/update`;
const calls = (app: App, url: string, method = "GET") =>
  app.requests().filter(req => req.url === url && req.method === method).length;
const updateWrites = (app: App) => app.requests()
  .filter(req => req.url.startsWith(`${statusPath}/`) && req.method !== "GET")
  .map(req => req.url.slice(statusPath.length));

function fixture() {
  const f = world();
  const bridge = fakeBridge(f);
  let available = false;
  const updateState = () => ({
    current: { version: "0.5.0", channel: "release", install: "linux-installer" },
    can_check: true, can_update_in_app: true, auto_check: true,
    result: available ? "available" : "up_to_date", phase: "idle", last_checked_at: 123,
    ...(available ? { latest: { version: "0.5.1" } } : {}),
  });
  const network = {
    [`GET ${statusPath}`]: () => ({ body: updateState() }),
    [`POST ${statusPath}/started`]: { body: {} },
    [`POST ${statusPath}/check`]: () => ({ body: updateState() }),
    [`GET ${webPath}`]: { status: 404, body: "" },
    [`GET ${chatPath}`]: { body: { personal_chat: false } },
    [`GET ${serverPath}`]: { status: 404, body: "" },
    ...bridge.network, ...f.network,
  };
  return { f, bridge, network, setAvailable(value: boolean) { available = value; } };
}

async function signIn(app: App, user: "alice" | "bob") {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:nav-settings");
}

async function start(x: ReturnType<typeof fixture>, user: "alice" | "bob" = "alice", height = 900) {
  const app = await open("./App.tsx", {
    modules: bridgeEnv(), network: x.network as any, platform: "linux",
    width: 1280, height, clock: {},
  });
  await app.waitFor("id:connect-screen");
  expect(app.shows("id:about-settings")).toBe(false);
  expect(app.shows("id:websearch-settings")).toBe(false);
  await signIn(app, user);
  await app.waitUntil(() => x.bridge.state.session?.account_id === user && calls(app, `${statusPath}/started`, "POST") === 1);
  return app;
}

test("the production registry reads only the open Settings page and keeps it mounted on resize", async () => {
  const app = await start(fixture());
  await app.press("id:nav-settings");
  await app.waitFor("id:profile-settings");
  expect(app.shows("id:nav-settings:websearch")).toBe(true);
  expect(app.shows("id:nav-settings:about")).toBe(true);
  expect(app.shows("id:websearch-settings")).toBe(false);
  expect(app.shows("id:about-settings")).toBe(false);
  expect(calls(app, statusPath)).toBe(1); // the existing layout startup read
  expect(calls(app, webPath) + calls(app, chatPath) + calls(app, serverPath)).toBe(0);

  await app.press("id:nav-settings:websearch");
  await app.waitFor("id:websearch-unsupported");
  await app.waitFor("id:websearch-chat-opt-in");
  expect(calls(app, chatPath)).toBe(1);
  expect(calls(app, webPath)).toBe(1);
  expect(calls(app, serverPath)).toBe(0);
  await app.setViewport(760, 900);
  await app.waitFor("id:nav-settings:websearch");
  expect(calls(app, webPath)).toBe(1);
  expect(calls(app, chatPath)).toBe(1);

  await app.press("id:nav-settings:about");
  await app.waitFor("id:update-status");
  expect(app.shows("id:websearch-settings")).toBe(false);
  expect(calls(app, serverPath)).toBe(1); // Alice is an org admin.
  await app.waitUntil(() => calls(app, statusPath) === 2);
  await app.setViewport(1280, 900);
  await app.press("id:toggle-settings-sections");
  await app.waitFor("id:rail-settings:about");
  expect(calls(app, statusPath)).toBe(2);
  expect(calls(app, serverPath)).toBe(1);
  await app.press("id:settings-close");
  await app.waitFor("id:settings-dialog", { state: "gone" });
  await app.wait(2500); // longer than About's watcher interval, shorter than the startup poll
  expect(app.shows("id:about-settings")).toBe(false);
  expect(calls(app, statusPath)).toBe(2);
  expect(updateWrites(app)).toEqual(["/started"]);
});

test("a polled offer turns on and off in the wide, collapsed and compact Settings navigation", async () => {
  const x = fixture();
  const app = await start(x);
  await app.press("id:nav-settings");
  expect(app.shows("id:nav-settings:about:dot")).toBe(false);
  await app.press("id:toggle-settings-sections");
  expect(app.shows("id:rail-settings:about:dot")).toBe(false);
  x.setAvailable(true);
  await app.wait(30_000);
  await app.waitFor("id:rail-settings:about:dot");
  expect(app.shows("label:About & updates, Update available")).toBe(true);
  await app.press("id:toggle-settings-sections");
  await app.waitFor("id:nav-settings:about:dot");
  await app.setViewport(760, 900);
  await app.waitFor("id:nav-settings:about:dot");
  expect(app.shows("label:About & updates, Update available")).toBe(true);
  x.setAvailable(false);
  await app.wait(30_000);
  await app.waitFor("id:nav-settings:about:dot", { state: "gone" });
  expect(app.shows("label:About & updates, Update available")).toBe(false);
  await app.setViewport(1280, 900);
  await app.press("id:toggle-settings-sections");
  expect(app.shows("id:rail-settings:about:dot")).toBe(false);
  expect(app.shows("id:about-settings")).toBe(false);
  expect(calls(app, serverPath)).toBe(0);
  expect(updateWrites(app)).toEqual(["/started"]);
});

test("Settings entry points show the same offer and the account shortcut only navigates", async () => {
  const x = fixture();
  x.setAvailable(true);
  const app = await start(x, "bob");
  await app.waitFor("label:Settings, Update available");
  await app.press("id:toggle-navigation");
  await app.waitFor("id:settings-update-dot");
  expect(app.shows("label:Settings, Update available")).toBe(true);
  await app.press("id:rail-account-menu");
  await app.press("id:account-update");
  await app.waitFor("id:update-status");
  await app.waitFor("id:nav-settings:about:dot");
  expect(calls(app, serverPath)).toBe(0); // not an org admin
  expect(updateWrites(app)).toEqual(["/started"]);
  x.setAvailable(false);
  await app.press("id:update-check");
  await app.waitFor("id:nav-settings:about:dot", { state: "gone" });
  await app.press("id:settings-close");
  await app.waitFor("id:settings-update-dot", { state: "gone" });
  await app.press("id:rail-account-menu");
  await app.waitFor("id:disconnect");
  expect(app.shows("id:account-update")).toBe(false);
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  expect(updateWrites(app)).toEqual(["/started", "/check"]);
});

test("Web search opens About normally for an old helper, or takes you to the real Privacy heading", async () => {
  const app = await start(fixture(), "bob", 640);
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:websearch");
  await app.waitFor("id:websearch-unsupported");
  await app.press("text:Open About & updates");
  await app.waitFor("id:about-version");
  expect(app.shows("id:websearch-settings")).toBe(false);

  await app.press("id:nav-settings:websearch");
  await app.press("id:websearch-privacy");
  await app.waitFor("id:about-privacy >> text:Privacy");
  expect(app.shows("text:What you send goes to the connection that answers. A shared connection's owner can see those requests and may pay for them.")).toBe(true);
  expect(app.openedUrls()).toEqual([]);
  expect(updateWrites(app)).toEqual(["/started"]);
  // The heading is the keyboard destination; the previous stop is Check now.
  await app.keyboard.press("shift+tab");
  await app.keyboard.press("enter");
  await app.waitUntil(() => calls(app, `${statusPath}/check`, "POST") === 1);

  await app.press("id:settings-close");
  await app.press("id:nav-settings");
  await app.waitFor("id:about-version");
  expect(updateWrites(app)).toEqual(["/started", "/check"]);
});

test("only the attributed starter can open production Web search Settings from activity after changing accounts", async () => {
  const x = fixture();
  const error = x.f.post("acme/lobby", {
    kind: "error", actor: "system", text: "fixture-only private refusal",
    activity: { id: "run_web", name: "Metro", source: "agent", tool: { name: "web_search", failure: "no_device", starter_id: "alice" } },
  } as any);
  async function activity(app: App) {
    await app.press("id:space:acme");
    await app.press("id:activity-toggle:agent:run_web");
    await app.waitFor(`id:activity-web-failure:${error.id}`);
  }
  const app = await start(x, "bob");
  await activity(app);
  expect(app.shows("id:activity-open-websearch")).toBe(false);
  expect(app.shows("text:fixture-only private refusal")).toBe(false);
  expect(calls(app, chatPath)).toBe(0);
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await signIn(app, "alice");
  await activity(app);
  await app.press("id:activity-open-websearch");
  await app.waitFor("id:websearch-settings");
  await app.press("id:settings-close");
  await app.waitFor(`id:activity-web-failure:${error.id}`);
  expect(app.shows("id:activity-open-websearch")).toBe(true);
});
