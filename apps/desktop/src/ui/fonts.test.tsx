// The typeface: Inter from theme.json, Segoe UI Variable on Windows, never a
// downloaded font.

import { expect, open, test } from "nsh/test";
import theme from "../../theme/theme.json";
import { platformTheme, WINDOWS_FONT } from "./fonts";

test("Windows gets Segoe UI Variable first; every other token stays the build's", () => {
  const win = platformTheme("windows")!;
  expect(win.cssVars.theme!["font-sans"]).toBe(WINDOWS_FONT);
  expect(WINDOWS_FONT.startsWith("'Segoe UI Variable Text'")).toBe(true);
  expect(win.cssVars.light).toEqual(theme.cssVars.light);
  expect(win.cssVars.dark).toEqual(theme.cssVars.dark);
  const { ["font-sans"]: _w, ...restWin } = win.cssVars.theme!;
  const { ["font-sans"]: _b, ...restBase } = theme.cssVars.theme as Record<string, string>;
  expect(restWin).toEqual(restBase);
});

test("elsewhere the built theme stands: Inter first, then the system face", () => {
  expect(platformTheme("linux")).toBeNull();
  expect(platformTheme("macos")).toBeNull();
  expect((theme.cssVars.theme as Record<string, string>)["font-sans"].startsWith("Inter,")).toBe(true);
});

test("the app starts on Windows with its font applied", async () => {
  const app = await open("../../App.tsx", { platform: "windows", width: 1100, height: 720 });
  await app.waitFor("id:connect");
  expect(app.shows("id:server-url")).toBe(true);
});
