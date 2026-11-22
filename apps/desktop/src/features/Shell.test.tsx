// The signed-in shell: the sidebar (Metro mark, Library, Spaces, the account
// menu), the top search box's shortcut, Manage Space's destructive steps and
// reduced motion. Against the fake server (test/fakeServer.ts); fake tokens only.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { serverPlace } from "./Sidebar";

type F = ReturnType<typeof world>;

async function openApp(f: F, opts: { reducedMotion?: boolean; width?: number; height?: number } = {}) {
  return open("../../App.tsx", { network: f.network as any, platform: "linux", width: 1200, height: 800, ...opts });
}

async function signIn(app: App, user: string) {
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS[user]!);
  await app.press("id:connect");
  await app.waitFor("id:account-menu");
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
