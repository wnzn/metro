// Resizable side panels: the navigation, the Chat's runs and the settings
// sections. A drag on the inner edge's handle resizes within the panel's
// range and is saved; a drag far below the navigation's narrowest width
// snaps it to the icon rail, keeping its width for when it opens again; the
// keyboard moves the edge in 16-point steps. Real NSH engine, fake server.

import { expect, open, test } from "nsh/test";
import type { App } from "nsh/test";
import { BASE, TOKENS, world } from "../../test/fakeServer";
import { spaceWorld } from "./space/test/fixtures";

type F = ReturnType<typeof world>;

async function inChat(f: F | ReturnType<typeof spaceWorld>, storage?: Record<string, string>) {
  const app = await open("../../App.tsx", { network: f.network as any, platform: "linux", width: 1400, height: 800, storage });
  await app.clear("id:server-url");
  await app.type("id:server-url", BASE);
  await app.type("id:access-token", TOKENS.alice!);
  await app.press("id:connect");
  await app.waitFor("id:chat-view");
  return app;
}

/** The width a panel saved (points); the headless engine reports no layout sizes, so the saved value stands for it. */
const saved = (app: App, name: string) => app.storage()[`metro.layout.${name}.width`];
const savedIs = (app: App, name: string, w: number) => app.waitUntil(() => saved(app, name) === String(w));

test("the navigation resizes by its edge within 200–400, is saved, and snaps to the rail below its minimum", async () => {
  const app = await inChat(world());
  expect(saved(app, "navigation")).toBeUndefined();
  expect(app.shows("label:Resize workspace navigation")).toBe(true);

  await app.drag("id:resize-navigation", { x: 64 });
  await savedIs(app, "navigation", 320);

  // Never wider than its maximum.
  await app.drag("id:resize-navigation", { x: 300 });
  await savedIs(app, "navigation", 400);

  // Narrower stops at the minimum...
  await app.drag("id:resize-navigation", { x: -210 });
  await savedIs(app, "navigation", 200);
  expect(app.shows("id:sidebar-rail")).toBe(false);

  // ...and a drag well past it collapses to the icon rail, keeping the width.
  await app.drag("id:resize-navigation", { x: -120 });
  await app.waitFor("id:sidebar-rail");
  expect(app.storage()["metro.layout.navigation"]).toBe("collapsed");
  expect(app.storage()["metro.layout.navigation.width"]).toBe("200");
  await app.press("id:toggle-navigation");
  await app.waitFor("id:resize-navigation");
  // Reopened at that width: one step from 200.
  await app.press("id:resize-navigation");
  await app.keyboard.press("right");
  await savedIs(app, "navigation", 216);
});

test("a saved width comes back, clamped to the panel's range", async () => {
  const app = await inChat(world(), { "metro.layout.navigation.width": "333", "metro.layout.runs.width": "9999" });
  await app.waitFor("id:runs-aside");
  // One step from each restored width shows where it was: 333, and 9999 clamped to 520.
  await app.press("id:resize-navigation");
  await app.keyboard.press("right");
  await savedIs(app, "navigation", 349);
  await app.press("id:resize-runs");
  await app.keyboard.press("right"); // the runs' edge is on their left: Right narrows them
  await savedIs(app, "runs", 504);
});

test("the runs panel resizes from its left edge: dragging left widens it", async () => {
  const app = await inChat(world());
  await app.waitFor("id:runs-aside");
  await app.drag("id:resize-runs", { x: -80 });
  await savedIs(app, "runs", 420);
  // Its minimum holds; the runs panel never collapses by dragging.
  await app.drag("id:resize-runs", { x: 400 });
  await savedIs(app, "runs", 260);
  expect(app.shows("id:runs-aside")).toBe(true);
});

test("the handle is a keyboard stop: Left and Right move the edge by 16, Return restores the default", async () => {
  const app = await inChat(world());
  await app.press("id:resize-navigation");
  await app.keyboard.press("right");
  await savedIs(app, "navigation", 272);
  await app.keyboard.press("left");
  await app.keyboard.press("left");
  await savedIs(app, "navigation", 240);
  await app.keyboard.press("shift+right");
  await savedIs(app, "navigation", 304);
  await app.keyboard.press("enter"); // activates the handle: the default width
  await savedIs(app, "navigation", 256);
});

test("the settings sections resize too, one width for Settings and Space settings", async () => {
  const app = await inChat(spaceWorld());
  await app.press("id:nav-settings");
  await app.waitFor("id:settings-nav");
  await app.drag("id:settings-nav-resize", { x: 40 });
  await savedIs(app, "settings-sections", 268);
  await app.press("id:settings-close").catch(() => app.back());
  await app.waitFor("id:settings-dialog", { state: "gone" });
  await app.press("id:space:acme");
  await app.waitFor("id:room-title");
  await app.press("id:space-settings-open");
  await app.waitFor("id:space-settings-nav-resize");
  await app.drag("id:space-settings-nav-resize", { x: -20 });
  await savedIs(app, "settings-sections", 248);
});
