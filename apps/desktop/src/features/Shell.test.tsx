// The signed-in shell: the sidebar (Metro mark, Library, Spaces, the account
// menu), the top search box's shortcut, Manage Space's destructive steps and
// reduced motion. Against the fake server (test/fakeServer.ts); fake tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { serverPlace, spaceInitial } from "./Sidebar";

type F = ReturnType<typeof world>;

async function openApp(f: F, opts: { reducedMotion?: boolean; width?: number; height?: number } = {}) {
  return open("../../App.tsx", { network: f.network as any, platform: "linux", width: 1200, height: 800, ...opts });
}

async function signIn(app: App, user: string, ready = "id:account-menu") {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor(ready);
}

const text = (app: App) => app.texts().join(" | ");

test("the connect screen and the sidebar carry the Metro mark", async () => {
  const app = await openApp(world());
  await app.waitFor("id:connect-screen");
  expect(app.shows("id:metro-mark")).toBe(true);
  await signIn(app, "alice");
  expect(app.shows("id:sidebar >> id:metro-mark")).toBe(true);
});

test("Library is a place of its own in the sidebar", async () => {
  const app = await openApp(world());
  await signIn(app, "alice");
  await app.press("id:nav-library");
  await app.waitFor("id:library-home");
  await app.press("id:nav-chat");
  await app.waitFor("id:library-home", { state: "gone" });
});

test("navigation collapses and restores with its draft, and stays collapsed across a restart", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  await app.press("id:new-space-toggle");
  await app.type("id:new-space-name", "Unfinished space");
  await app.press("label:Hide workspace navigation");
  await app.waitFor("label:Show workspace navigation");
  expect(app.shows("id:runs-aside")).toBe(true);
  expect(app.shows("id:nav-chat")).toBe(false);
  const storage = app.storage();
  expect(storage["metro.layout.navigation"]).toBe("collapsed");
  await app.keyboard.press("mod+b");
  await app.waitFor("id:new-space-name");
  expect(app.value("id:new-space-name")).toBe("Unfinished space");
  const second = await open("../../App.tsx", { network: world().network as any, platform: "linux", width: 760, height: 560, storage });
  await second.press("id:show-server-form");
  await signIn(second, "alice", "id:toggle-navigation");
  await second.waitFor("label:Show workspace navigation");
  await second.press("id:toggle-navigation");
  await second.waitFor("id:nav-library");
});

test("collapsing Settings sections keeps the active page and leaves the workspace panel alone", async () => {
  const app = await openApp(world());
  await signIn(app, "alice");
  await app.press("id:nav-settings");
  await app.press("id:nav-settings:appearance");
  await app.press("label:Hide settings sections");
  await app.waitFor("id:settings-nav", { state: "gone" });
  expect(app.shows("id:appearance-settings")).toBe(true);
  await app.keyboard.press("mod+b");
  await app.press("label:Show settings sections");
  await app.waitFor("id:nav-settings:profile");
  await app.press("id:settings-close");
  await app.waitFor("id:nav-settings");
});

test("the account shows the host once, and its menu copies the server URL and the ID, then signs out", async () => {
  const app = await openApp(world());
  await signIn(app, "alice");
  // The host once; not the full URL beside it.
  expect(app.count("text:127.0.0.1:8787")).toBe(1);
  expect(app.texts()).not.toContain(BASE);
  expect(text(app)).not.toContain("On this device");

  await app.press("id:account-menu");
  await app.waitFor("id:copy-server-url");
  expect(app.shows("id:copy-id")).toBe(true);
  expect(app.shows("text:Sign out")).toBe(true);
  await app.press("id:copy-server-url");
  await app.waitFor("id:account-copied");
  expect(app.clipboard()).toBe(BASE);

  await app.press("id:account-menu");
  await app.press("id:copy-id");
  await app.waitUntil(() => app.clipboard() === "alice");

  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
});

test("On this device only for the app-hosted workspace, never from a loopback URL", () => {
  expect(serverPlace("http://127.0.0.1:8787", false)).toEqual({ local: false, title: "127.0.0.1:8787", detail: "http://127.0.0.1:8787" });
  expect(serverPlace("http://127.0.0.1:41234", true).title).toBe("On this device");
});

test("mod+k puts the keyboard focus in the top search box, and nothing else", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  expect(app.shows("id:top-search-shortcut")).toBe(true);
  const before = app.requests().length;
  await app.keyboard.press("mod+k");
  await app.keyboard.press("q");
  expect(app.value("id:top-search")).toBe("q");
  // Focusing and typing send nothing to search.
  expect(app.requests().slice(before).some((r) => r.url.includes("/search"))).toBe(false);
});

test("mod+k leaves the search box alone while Settings is open over it", async () => {
  const app = await openApp(world());
  await signIn(app, "alice");
  await app.press("id:nav-settings");
  await app.waitFor("id:settings-dialog");
  await app.keyboard.press("mod+k");
  await app.keyboard.press("q");
  expect(app.value("id:top-search")).toBe("");
});

test("mod+k still focuses the search box after signing out and in again", async () => {
  const app = await openApp(world());
  await signIn(app, "alice");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
  await signIn(app, "alice");
  await app.keyboard.press("mod+k");
  await app.keyboard.press("z");
  expect(app.value("id:top-search")).toBe("z");
});

test("new room sits in line with the rooms", async () => {
  const app = await openApp(world());
  await signIn(app, "alice");
  await app.press("id:space:acme");
  await app.waitFor("id:room-list");
  await app.press("id:new-room-toggle");
  await app.waitFor("id:new-room-name");
});

test("revoking a dedicated token asks first; Cancel changes nothing", async () => {
  const f = world(); // Acme has a dedicated token
  const app = await openApp(f);
  await signIn(app, "alice");
  await app.press("id:space:acme");
  await app.waitFor("id:manage-space");
  await app.press("id:manage-space");
  await app.waitFor("id:dedicated-runner");
  await app.press("id:revoke-runner-token");
  await app.waitFor("id:confirm-revoke");
  expect(app.shows("text:Revoke now: the runner stops serving this Space")).toBe(true);
  await app.press("text:Cancel");
  await app.waitFor("id:confirm-revoke", { state: "gone" });
  expect(f.state.spaces.get("acme")!.runnerToken).toBe("runner-acme");

  await app.press("id:revoke-runner-token");
  await app.press("id:confirm-revoke");
  await app.waitFor("id:manage-notice");
  expect(text(app)).toContain("Runner token revoked.");
  expect(f.state.spaces.get("acme")!.runnerToken).toBe(null);
});

test("with reduced motion, Settings and the account menu still open and close", async () => {
  const app = await openApp(world(), { reducedMotion: true });
  await signIn(app, "alice");
  await app.press("id:nav-settings");
  await app.waitFor("id:settings-dialog");
  await app.back();
  await app.waitFor("id:settings-dialog", { state: "gone" });
  await app.press("id:account-menu");
  await app.waitFor("id:copy-id");
  await app.back(); // Escape closes it
  await app.waitFor("id:copy-id", { state: "gone" });
});

test("compact window: mod+k focuses search, and the account menu opens, copies and signs out", async () => {
  const app = await openApp(world(), { width: 760, height: 560 });
  await signIn(app, "alice");
  await app.keyboard.press("mod+k");
  await app.keyboard.press("w");
  expect(app.value("id:top-search")).toBe("w");

  await app.press("id:account-menu");
  await app.press("id:copy-id");
  await app.waitUntil(() => app.clipboard() === "alice");
  await app.press("id:account-menu");
  await app.press("id:disconnect");
  await app.waitFor("id:connect-screen");
});

test("collapsed, the navigation is an icon rail with names on hover; a narrow window hides it", async () => {
  const app = await openApp(world());
  await signIn(app, "alice");
  // The toggle is an icon: its caption is only its tooltip and accessible name.
  expect(app.shows("id:toggle-navigation >> text:Navigation")).toBe(false);
  await app.press("label:Hide workspace navigation");
  await app.waitFor("id:sidebar-rail");
  expect(app.shows("id:nav-chat")).toBe(false);
  for (const id of ["rail-chat", "rail-library", "rail-schedules", "rail-space:acme", "rail-new-space", "rail-settings", "rail-account-menu"]) {
    expect(app.shows(`id:${id}`)).toBe(true);
  }
  expect(app.shows("id:rail-space:acme >> text:A")).toBe(true); // the Space's initial
  expect(app.shows("label:Space Acme")).toBe(true);
  expect(app.shows("label:Library")).toBe(true);
  await app.press("id:rail-space:acme");
  await app.waitFor("text:hello from bob in acme");
  await app.press("id:rail-library");
  await app.waitFor("id:library-home");
  await app.setViewport(760, 800);
  await app.waitFor("id:sidebar-rail", { state: "gone" });
  await app.setViewport(1200, 800);
  await app.waitFor("id:sidebar-rail");
  // New Space opens the whole panel on its form.
  await app.press("id:rail-new-space");
  await app.waitFor("id:new-space-name");
  expect(app.storage()["metro.layout.navigation"]).toBe("expanded");
});

test("collapsed Settings sections stay as a rail of their icons", async () => {
  const app = await openApp(world());
  await signIn(app, "alice");
  await app.press("id:nav-settings");
  expect(app.shows("id:toggle-settings-sections >> text:Sections")).toBe(false);
  await app.press("label:Hide settings sections");
  await app.waitFor("id:settings-rail");
  await app.press("id:rail-settings:appearance");
  await app.waitFor("id:appearance-settings");
  expect(app.shows("label:Model providers")).toBe(true);
});

test("the connection is a dot on the account's server line; words only when not online", async () => {
  const f = world();
  const app = await openApp(f);
  await signIn(app, "alice");
  expect(app.shows("id:sidebar >> id:metro-mark")).toBe(true);
  expect(app.shows("id:active-server >> id:connection-state")).toBe(true);
  expect(app.shows("label:Connection: Online")).toBe(true);
  expect(app.shows("text:Online")).toBe(false);
  expect(app.shows("label:/^alice, connected to 127\\.0\\.0\\.1:8787\\. Connection: Online\\. Account menu$/")).toBe(true);
});

test("a room's breadcrumb and actions share the one header row with the search box", async () => {
  const app = await openApp(world());
  await signIn(app, "alice");
  await app.press("id:space:acme");
  await app.waitFor("id:room-title");
  expect(app.shows("id:top-bar >> id:room-title")).toBe(true);
  expect(app.shows("id:top-bar >> id:top-search")).toBe(true);
  expect(app.shows("id:top-bar >> id:manage-space")).toBe(true);
  expect(app.shows("id:top-bar >> id:toggle-navigation")).toBe(true);
  expect(app.shows("text:Members of this Space share its rooms and its agent.")).toBe(false); // its tooltip
  await app.press("id:nav-chat");
  await app.waitFor("id:chat-view");
  expect(app.shows("id:top-bar >> id:manage-space")).toBe(false);
});

test("a Space's rail tile is its first letter or digit", () => {
  expect(spaceInitial("acme")).toBe("A");
  expect(spaceInitial("  #ops-2")).toBe("O");
  expect(spaceInitial("42 club")).toBe("4");
  expect(spaceInitial("---")).toBe("#");
});

test("the Spaces heading's + is quiet until hover, and a Space running an agent shows a dot", async () => {
  const f = world();
  f.state.runs.push({ id: "run_x", space_id: "acme", room_id: "lobby", status: "running", actor: "alice", task: "t" });
  const app = await openApp(f);
  await signIn(app, "alice");
  await app.waitFor("label:Acme, the agent is working here");
  expect(app.viewStyle("id:new-space-toggle")?.opacity).toBe(0);
  await app.press("id:new-space-toggle"); // still a target for the keyboard and pointer
  await app.waitFor("id:new-space-name");
  expect(app.viewStyle("id:new-space-toggle")?.opacity).toBe(1); // Cancel stays shown while the form is open
});

test("in a narrow header a thread's link back to its parent stays reachable", async () => {
  const f = world();
  f.addRoom("acme", { id: "fix", name: "a-thread-with-a-rather-long-name", parent_room_id: "lobby" } as any);
  const app = await openApp(f, { width: 920 });
  await signIn(app, "alice");
  await app.press("id:space:acme");
  await app.press("id:room:fix");
  await app.waitFor("id:open-parent");
  await app.press("id:open-parent");
  await app.waitFor("text:hello from bob in acme");
});
